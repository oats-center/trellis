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
    let page = input.get("page").unwrap_or(&Value::Null);
    let limit = page.get("limit").and_then(Value::as_u64).unwrap_or(100);
    if limit == 0 || limit > 500 {
        return Err("invalid pagination limit".to_owned());
    }
    let limit = limit as usize;
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
    let query_digest = pagination_query_digest(
        "events.Consumers.Query",
        &json!({
            "status": statuses,
            "subject": subject,
            "deploymentId": deployment_id,
            "resourceId": resource_id,
            "contractId": contract_id,
            "order": ["resourceId", "stream", "consumerName"]
        }),
    )
    .map_err(|_| "invalid pagination cursor".to_owned())?;
    let cursor = page
        .get("cursor")
        .and_then(Value::as_str)
        .map(|cursor| {
            decode_pagination_cursor::<(String, String, String)>(cursor, &query_digest)
                .map_err(|_| "invalid pagination cursor".to_owned())
        })
        .transpose()?;
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
    let mut rows = rows
        .into_iter()
        .filter(|row| {
            (statuses.is_empty()
                || row
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| statuses.contains(&status)))
                && deployment_id.is_none_or(|deployment_id| {
                    row.get("deploymentId").and_then(Value::as_str) == Some(deployment_id)
                })
                && resource_id.is_none_or(|resource_id| {
                    row.get("resourceId").and_then(Value::as_str) == Some(resource_id)
                })
                && contract_id.is_none_or(|contract_id| {
                    row.get("contractId").and_then(Value::as_str) == Some(contract_id)
                })
                && subject.is_none_or(|subject| {
                    row.get("filterSubjects")
                        .and_then(Value::as_array)
                        .is_some_and(|filters| {
                            filters
                                .iter()
                                .filter_map(Value::as_str)
                                .any(|filter| nats_subjects_intersect(filter, subject))
                        })
                })
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(consumer_cursor_key);
    let mut consumers = rows
        .into_iter()
        .filter(|row| {
            cursor
                .as_ref()
                .is_none_or(|cursor| consumer_cursor_key(row) > *cursor)
        })
        .take(limit + 1)
        .collect::<Vec<_>>();
    let next_cursor = if consumers.len() > limit {
        consumers.pop();
        consumers
            .last()
            .map(consumer_cursor_key)
            .map(|after| encode_pagination_cursor(&query_digest, &after))
            .transpose()
            .map_err(|_| "invalid pagination cursor".to_owned())?
    } else {
        None
    };
    Ok(json!({ "items": consumers, "page": { "nextCursor": next_cursor } }))
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

fn consumer_cursor_key(row: &Value) -> (String, String, String) {
    (
        row.get("resourceId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        row.get("stream")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        row.get("consumerName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    )
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
        "resourceId": info.config.metadata.get(CONSUMER_METADATA_RESOURCE_ID).cloned().unwrap_or_else(|| info.name.clone()),
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
