use serde_json::{json, Value};
use trellis_protocol::{
    decode_pagination_cursor, encode_pagination_cursor, pagination_query_digest,
};

use crate::projector::{
    is_events_projector_consumer, EventsRuntime, CONSUMER_METADATA_CONTRACT_ID,
    CONSUMER_METADATA_DEPLOYMENT_ID, CONSUMER_METADATA_GROUP, CONSUMER_METADATA_MANAGED_BY,
    CONSUMER_METADATA_RESOURCE_ID,
};
use crate::ConsumerBindingResolver;

pub(crate) async fn query_consumers(
    runtime: &EventsRuntime,
    resolver: &dyn ConsumerBindingResolver,
    input: &Value,
) -> Result<Value, String> {
    // Validate the request before reading JetStream, so a bad cursor or limit
    // costs nothing.
    let query = ConsumerPageQuery::parse(input)?;
    let mut live = runtime
        .consumers()
        .await?
        .into_iter()
        .filter(|info| info.config.durable_name.is_some())
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for binding in resolver.all().await? {
        if let Some(index) = live.iter().position(|info| {
            info.stream_name == binding.stream && info.name == binding.consumer_name
        }) {
            let mut info = live.swap_remove(index);
            info.config.metadata.insert(
                CONSUMER_METADATA_RESOURCE_ID.to_owned(),
                binding.resource_id,
            );
            info.config.metadata.insert(
                CONSUMER_METADATA_CONTRACT_ID.to_owned(),
                binding.participant_id,
            );
            info.config
                .metadata
                .insert(CONSUMER_METADATA_GROUP.to_owned(), binding.local_name);
            if binding.owner_kind == "deployment" {
                info.config
                    .metadata
                    .insert(CONSUMER_METADATA_DEPLOYMENT_ID.to_owned(), binding.owner_id);
            }
            rows.push(attributed_consumer_row(info, None, "authority"));
        } else {
            rows.push(missing_consumer_row(&binding));
        }
    }
    rows.extend(live.into_iter().map(unmatched_consumer_row));
    query.page(rows)
}

/// Sort key of one consumer row: attention rank, inverted backlog, then identity.
type ConsumerKey = (u8, u64, String, String, String);

/// A validated `Consumers.Query` request: filters, order, and keyset position.
struct ConsumerPageQuery<'a> {
    limit: usize,
    statuses: Vec<&'a str>,
    subject: Option<&'a str>,
    deployment_id: Option<&'a str>,
    resource_id: Option<&'a str>,
    contract_id: Option<&'a str>,
    attention: bool,
    query_digest: String,
    cursor: Option<ConsumerKey>,
}

impl<'a> ConsumerPageQuery<'a> {
    fn parse(input: &'a Value) -> Result<Self, String> {
        let page = input.get("page").unwrap_or(&Value::Null);
        let limit = page.get("limit").and_then(Value::as_u64).unwrap_or(100);
        if limit == 0 || limit > 500 {
            return Err("invalid pagination limit".to_owned());
        }
        let mut statuses = input
            .get("status")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        statuses.sort_unstable();
        statuses.dedup();
        let subject = input.get("subject").and_then(Value::as_str);
        let deployment_id = input.get("deploymentId").and_then(Value::as_str);
        let resource_id = input.get("resourceId").and_then(Value::as_str);
        let contract_id = input
            .get("contractId")
            .or_else(|| input.get("ownerContractId"))
            .and_then(Value::as_str);
        let attention = input.get("order").and_then(Value::as_str) == Some("attention");
        let query_digest = pagination_query_digest(
            "events.Consumers.Query",
            &json!({
                "status": statuses,
                "subject": subject,
                "deploymentId": deployment_id,
                "resourceId": resource_id,
                "contractId": contract_id,
                "order": if attention {
                    json!(["attention", "pending:desc", "resourceId", "stream", "consumerName"])
                } else {
                    json!(["resourceId", "stream", "consumerName"])
                }
            }),
        )
        .map_err(|_| "invalid pagination cursor".to_owned())?;
        let cursor = page
            .get("cursor")
            .and_then(Value::as_str)
            .map(|cursor| {
                decode_pagination_cursor::<ConsumerKey>(cursor, &query_digest)
                    .map_err(|_| "invalid pagination cursor".to_owned())
            })
            .transpose()?;
        Ok(Self {
            limit: limit as usize,
            statuses,
            subject,
            deployment_id,
            resource_id,
            contract_id,
            attention,
            query_digest,
            cursor,
        })
    }

    fn matches(&self, row: &Value) -> bool {
        (self.statuses.is_empty()
            || row
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| self.statuses.contains(&status)))
            && self.deployment_id.is_none_or(|deployment_id| {
                row.get("deploymentId").and_then(Value::as_str) == Some(deployment_id)
            })
            && self.resource_id.is_none_or(|resource_id| {
                row.get("resourceId").and_then(Value::as_str) == Some(resource_id)
            })
            && self.contract_id.is_none_or(|contract_id| {
                row.get("contractId").and_then(Value::as_str) == Some(contract_id)
            })
            && self.subject.is_none_or(|subject| {
                row.get("filterSubjects")
                    .and_then(Value::as_array)
                    .is_some_and(|filters| {
                        filters
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|filter| nats_subjects_intersect(filter, subject))
                    })
            })
    }

    /// The row's position in this query's order. Under attention order, rows
    /// that need an operator come first, most severe and then largest backlog
    /// first; consumers set up outside Trellis come last.
    fn key(&self, row: &Value) -> ConsumerKey {
        let text = |field: &str| {
            row.get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (rank, backlog) = if self.attention {
            let rank = if row.get("managedBy").and_then(Value::as_str) == Some("external") {
                8
            } else {
                match row.get("status").and_then(Value::as_str) {
                    Some("missing") => 0,
                    Some("saturated") => 1,
                    Some("inactive") => 2,
                    Some("failing") => 3,
                    Some("behind") => 4,
                    Some("orphaned") => 5,
                    Some("processing") => 6,
                    Some("current") => 7,
                    _ => 8,
                }
            };
            let pending = row.get("pending").and_then(Value::as_u64).unwrap_or(0);
            (rank, u64::MAX - pending)
        } else {
            (0, 0)
        };
        (
            rank,
            backlog,
            text("resourceId"),
            text("stream"),
            text("consumerName"),
        )
    }

    /// Filters, orders, and cuts one page from every known consumer row.
    fn page(&self, rows: Vec<Value>) -> Result<Value, String> {
        let mut rows = rows
            .into_iter()
            .filter(|row| self.matches(row))
            .map(|row| (self.key(&row), row))
            .filter(|(key, _)| self.cursor.as_ref().is_none_or(|cursor| key > cursor))
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        rows.truncate(self.limit + 1);
        let next_cursor = if rows.len() > self.limit {
            rows.pop();
            rows.last()
                .map(|(key, _)| encode_pagination_cursor(&self.query_digest, key))
                .transpose()
                .map_err(|_| "invalid pagination cursor".to_owned())?
        } else {
            None
        };
        let items = rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>();
        Ok(json!({ "items": items, "page": { "nextCursor": next_cursor } }))
    }
}

fn nats_subjects_intersect(left: &str, right: &str) -> bool {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (Some(">"), Some(_)) => return left.next().is_none(),
            (Some(_), Some(">")) => return right.next().is_none(),
            (Some("*"), Some(_)) | (Some(_), Some("*")) => {}
            (Some(left), Some(right)) if left == right => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

pub(crate) async fn inspect_consumer(
    runtime: &EventsRuntime,
    resolver: &dyn ConsumerBindingResolver,
    input: &Value,
) -> Result<Value, String> {
    let resource_id = input
        .get("resourceId")
        .and_then(Value::as_str)
        .ok_or_else(|| "resourceId is required".to_string())?;
    let binding = resolver
        .by_resource(resource_id)
        .await?
        .ok_or_else(|| format!("consumer not found: {resource_id}"))?;
    let mut info = match runtime.consumer(&binding.consumer_name).await {
        Ok(info) => info,
        Err(_) => return Ok(json!({ "consumer": missing_consumer_row(&binding) })),
    };
    info.config.metadata.insert(
        CONSUMER_METADATA_RESOURCE_ID.to_owned(),
        binding.resource_id,
    );
    info.config.metadata.insert(
        CONSUMER_METADATA_CONTRACT_ID.to_owned(),
        binding.participant_id,
    );
    info.config
        .metadata
        .insert(CONSUMER_METADATA_GROUP.to_owned(), binding.local_name);
    if binding.owner_kind == "deployment" {
        info.config
            .metadata
            .insert(CONSUMER_METADATA_DEPLOYMENT_ID.to_owned(), binding.owner_id);
    }
    Ok(json!({ "consumer": attributed_consumer_row(info, None, "authority") }))
}

fn missing_consumer_row(binding: &crate::ConsumerBinding) -> Value {
    json!({
        "resourceId": binding.resource_id,
        "stream": binding.stream,
        "consumerName": binding.consumer_name,
        "filterSubjects": binding.filter_subjects,
        "status": "missing",
        "managedBy": "authority",
        "pending": 0,
        "ackPending": 0,
        "waitingPulls": 0,
        "contractId": binding.participant_id,
        "group": binding.local_name,
        "deploymentId": (binding.owner_kind == "deployment").then(|| binding.owner_id.clone()),
    })
}

fn consumer_status(info: &async_nats::jetstream::consumer::Info) -> &'static str {
    let pending = info.num_pending;
    let ack_pending = info.num_ack_pending as u64;
    let waiting = info.num_waiting as u64;
    let redelivered = info.num_redelivered as u64;
    let max_ack_pending = info.config.max_ack_pending.max(0) as u64;
    if redelivered > 0 {
        "failing"
    } else if pending == 0 && ack_pending == 0 {
        "current"
    } else if pending > 0 && max_ack_pending > 0 && ack_pending >= max_ack_pending {
        "saturated"
    } else if pending > 0 && waiting == 0 && ack_pending == 0 {
        "inactive"
    } else if pending > 0 {
        "behind"
    } else {
        "processing"
    }
}

fn consumer_row(
    info: async_nats::jetstream::consumer::Info,
    status: Option<&str>,
    managed_by: &str,
) -> Value {
    let status = status.unwrap_or_else(|| consumer_status(&info));
    let filter_subjects = if !info.config.filter_subjects.is_empty() {
        info.config.filter_subjects.clone()
    } else if info.config.filter_subject.is_empty() {
        Vec::new()
    } else {
        vec![info.config.filter_subject.clone()]
    };
    json!({
        "stream": info.stream_name,
        "consumerName": info.name,
        "resourceId": info
                .config
                .metadata
                .get(CONSUMER_METADATA_RESOURCE_ID)
                .cloned()
                .unwrap_or_else(|| info.name.clone()),
        "filterSubjects": filter_subjects,
        "status": status,
        "managedBy": managed_by,
        "pending": info.num_pending,
        "ackPending": info.num_ack_pending as u64,
        "waitingPulls": info.num_waiting as u64,
        "redelivered": info.num_redelivered as u64,
        "ackWaitMs": info.config.ack_wait.as_millis() as u64,
        "maxDeliver": info.config.max_deliver,
    })
}

fn unmatched_consumer_row(info: async_nats::jetstream::consumer::Info) -> Value {
    let managed_by = info.config.metadata.get(CONSUMER_METADATA_MANAGED_BY);
    if managed_by.is_some_and(|value| value == "authority") {
        attributed_consumer_row(info, Some("orphaned"), "authority")
    } else if managed_by.is_some_and(|value| value == "platform") {
        attributed_consumer_row(info, None, "platform")
    } else if is_events_projector_consumer(&info.name) {
        let mut row = consumer_row(info, None, "platform");
        row["contractId"] = json!("trellis.events@v1");
        row["group"] = json!("projector");
        row
    } else {
        consumer_row(info, None, "external")
    }
}

fn attributed_consumer_row(
    info: async_nats::jetstream::consumer::Info,
    status: Option<&str>,
    managed_by: &str,
) -> Value {
    let deployment_id = info
        .config
        .metadata
        .get(CONSUMER_METADATA_DEPLOYMENT_ID)
        .cloned();
    let contract_id = info
        .config
        .metadata
        .get(CONSUMER_METADATA_CONTRACT_ID)
        .cloned();
    let group = info.config.metadata.get(CONSUMER_METADATA_GROUP).cloned();
    let mut row = consumer_row(info, status, managed_by);
    if let Some(deployment_id) = deployment_id {
        row["deploymentId"] = json!(deployment_id);
    }
    if let Some(contract_id) = contract_id {
        row["contractId"] = json!(contract_id);
    }
    if let Some(group) = group {
        row["group"] = json!(group);
    }
    row
}

#[cfg(test)]
mod tests {
    use super::nats_subjects_intersect;

    #[test]
    fn consumer_subject_filter_uses_nats_token_intersection() {
        assert!(nats_subjects_intersect(
            "events.v1.orders.*",
            "events.v1.*.Created"
        ));
        assert!(nats_subjects_intersect(
            "events.v1.orders.>",
            "events.v1.orders.Created.eu"
        ));
        assert!(!nats_subjects_intersect(
            "events.v1.order.>",
            "events.v1.orders.Created"
        ));
        assert!(!nats_subjects_intersect(
            "events.v1.orders.>",
            "events.v1.orders"
        ));
    }
}

#[cfg(test)]
mod page_tests {
    use serde_json::{json, Value};

    use super::ConsumerPageQuery;

    fn row(name: &str, status: &str, managed_by: &str, pending: u64) -> Value {
        json!({
            "resourceId": format!("resource-{name}"),
            "stream": "trellis",
            "consumerName": name,
            "filterSubjects": ["events.v1.orders.>"],
            "status": status,
            "managedBy": managed_by,
            "pending": pending,
        })
    }

    fn names(page: &Value) -> Vec<String> {
        page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["consumerName"].as_str().expect("name").to_owned())
            .collect()
    }

    fn rows() -> Vec<Value> {
        let mut rows = vec![
            row("a-current", "current", "authority", 0),
            row("b-small-backlog", "behind", "authority", 3),
            row("c-external-behind", "behind", "external", 900),
            row("d-missing", "missing", "authority", 0),
            row("e-large-backlog", "behind", "authority", 400),
            row("f-processing", "processing", "platform", 0),
            row("g-failing", "failing", "authority", 1),
        ];
        rows.extend(
            (0..30).map(|index| row(&format!("z-current-{index:02}"), "current", "authority", 0)),
        );
        rows
    }

    /// Walking the attention order page by page yields every consumer once,
    /// problems before healthy ones, the largest backlog first within a
    /// status, and consumers set up outside Trellis last.
    #[test]
    fn attention_order_pages_problems_first_without_gaps() {
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let input = json!({ "order": "attention", "page": { "limit": 4, "cursor": cursor } });
            let page = ConsumerPageQuery::parse(&input)
                .expect("query")
                .page(rows())
                .expect("page");
            let names = names(&page);
            assert!(names.len() <= 4, "a page is bounded by its limit");
            seen.extend(names);
            match page["page"]["nextCursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => break,
            }
        }
        assert_eq!(seen.len(), rows().len(), "every consumer appears once");
        let unique = seen.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), seen.len(), "no consumer repeats across pages");
        assert_eq!(
            &seen[..5],
            [
                "d-missing",
                "g-failing",
                "e-large-backlog",
                "b-small-backlog",
                "f-processing",
            ]
        );
        assert_eq!(seen.last().map(String::as_str), Some("c-external-behind"));
    }

    /// The panel's "needs attention" view filters on the server, and a cursor
    /// from one order cannot continue another.
    #[test]
    fn status_filter_and_order_bind_the_cursor() {
        let filtered = |cursor: Option<&str>| {
            json!({
                "order": "attention",
                "status": ["behind", "failing", "missing"],
                "page": { "limit": 2, "cursor": cursor },
            })
        };
        let input = filtered(None);
        let first = ConsumerPageQuery::parse(&input)
            .expect("query")
            .page(rows())
            .expect("first page");
        assert_eq!(names(&first), ["d-missing", "g-failing"]);
        let cursor = first["page"]["nextCursor"]
            .as_str()
            .expect("cursor")
            .to_owned();
        let input = filtered(Some(&cursor));
        let second = ConsumerPageQuery::parse(&input)
            .expect("query")
            .page(rows())
            .expect("second page");
        assert_eq!(names(&second), ["e-large-backlog", "b-small-backlog"]);

        let input = json!({ "page": { "limit": 2, "cursor": cursor } });
        assert!(ConsumerPageQuery::parse(&input).is_err());
    }
}
