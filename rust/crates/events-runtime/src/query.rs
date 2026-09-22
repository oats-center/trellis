use serde_json::{json, Value};
use trellis_protocol::{
    decode_pagination_cursor, encode_pagination_cursor, pagination_query_digest,
};

use std::sync::Arc;

use crate::consumers::{inspect_consumer, query_consumers};
use crate::dead_letters::ConsumerBindingResolver;
use crate::projector::EventsRuntime;
use crate::storage::{EventTypeRef, EventsFilter, EventsStore, EventsStoreError};

/// Events query adapter backed by SQLite and JetStream consumer info.
#[derive(Clone)]
pub struct EventsQuery {
    store: EventsStore,
    runtime: EventsRuntime,
    resolver: Arc<dyn ConsumerBindingResolver>,
}

/// Errors returned by Events RPC query handlers.
#[derive(Debug, thiserror::Error)]
pub enum EventsQueryError {
    /// The requested event was not found.
    #[error("event not found")]
    EventNotFound {
        /// Requested event id, when supplied.
        event_id: Option<String>,
        /// Requested stream sequence, when supplied.
        stream_sequence: Option<u64>,
    },
    /// The requested consumer was not found.
    #[error("consumer not found: {0}")]
    ConsumerNotFound(String),
    /// Input validation failed.
    #[error("invalid {field}: {details}")]
    Validation {
        /// Field name.
        field: &'static str,
        /// Validation details.
        details: String,
    },
    /// SQLite projection query failed.
    #[error(transparent)]
    Store(#[from] EventsStoreError),
    /// JetStream consumer query failed.
    #[error("consumer query failed: {0}")]
    Consumer(String),
}

impl EventsQuery {
    /// Construct an Events query adapter.
    pub fn new(
        store: EventsStore,
        runtime: EventsRuntime,
        resolver: Arc<dyn ConsumerBindingResolver>,
    ) -> Self {
        Self {
            store,
            runtime,
            resolver,
        }
    }

    /// Run `Events.Query`.
    pub async fn query_events(&self, input: &Value) -> Result<Value, EventsQueryError> {
        let (filter, query_digest) = parse_event_filter(input)?;
        let (mut events, _) = self.store.query_events(&filter)?;
        let next_cursor = if events.len() > filter.limit as usize {
            events.pop();
            let last = events.last().ok_or_else(|| EventsQueryError::Validation {
                field: "page.cursor",
                details: "invalid pagination".to_owned(),
            })?;
            let value = match filter.sort_field.as_str() {
                "streamSequence" => last
                    .get("streamSequence")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    .to_string(),
                "payloadSize" => last
                    .get("payloadSizeBytes")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    .to_string(),
                _ => last
                    .get("eventTime")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            };
            let sequence = last
                .get("streamSequence")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            Some(
                encode_pagination_cursor(&query_digest, &(value, sequence)).map_err(|_| {
                    EventsQueryError::Validation {
                        field: "page.cursor",
                        details: "invalid pagination".to_owned(),
                    }
                })?,
            )
        } else {
            None
        };
        Ok(json!({ "items": events, "page": { "nextCursor": next_cursor } }))
    }

    /// Run `Events.Inspect`.
    pub async fn inspect_event(&self, input: &Value) -> Result<Value, EventsQueryError> {
        let event_id = input.get("eventId").and_then(Value::as_str);
        let stream_sequence = input.get("streamSequence").and_then(Value::as_u64);
        if event_id.is_none() && stream_sequence.is_none() {
            return Err(EventsQueryError::Validation {
                field: "eventId",
                details: "eventId or streamSequence is required".to_string(),
            });
        }
        self.store
            .inspect_event(event_id, stream_sequence)?
            .ok_or_else(|| EventsQueryError::EventNotFound {
                event_id: event_id.map(str::to_owned),
                stream_sequence,
            })
    }

    /// Run `Events.Metrics`.
    pub async fn metrics(&self, input: &Value) -> Result<Value, EventsQueryError> {
        let window = input
            .get("window")
            .and_then(Value::as_str)
            .and_then(|window| {
                let (window_seconds, bucket_seconds) = window_config(window)?;
                Some((
                    window_since(window_seconds)?,
                    window_seconds,
                    bucket_seconds,
                ))
            });
        Ok(self
            .store
            .metrics(window, input.get("resourceId").and_then(Value::as_str))?)
    }

    /// Run `Events.Diagnostics` against projection checkpoints.
    pub async fn diagnostics(&self) -> Result<Value, EventsQueryError> {
        let diagnostics = self.store.diagnostics()?;
        let (_, last_sequence) = self
            .runtime
            .stream_bounds()
            .await
            .map_err(EventsQueryError::Consumer)?;
        let projected_sequence = diagnostics["eventProjectionSequence"]
            .as_u64()
            .unwrap_or_default();
        Ok(json!({
            "asOf": crate::storage::now_timestamp_string(),
            "completeSince": diagnostics["completeSince"],
            "gapDetected": diagnostics["gapDetected"],
            "lastStreamSequence": last_sequence,
            "retainedFrom": diagnostics["retainedFrom"],
            "revision": projected_sequence,
        }))
    }

    /// Run `Events.Consumers.Query`.
    pub async fn query_consumers(&self, input: &Value) -> Result<Value, EventsQueryError> {
        query_consumers(&self.runtime, self.resolver.as_ref(), input)
            .await
            .map_err(|error| {
                if error.starts_with("invalid pagination") {
                    EventsQueryError::Validation {
                        field: if error.ends_with("limit") {
                            "page.limit"
                        } else {
                            "page.cursor"
                        },
                        details: error,
                    }
                } else {
                    EventsQueryError::Consumer(error)
                }
            })
    }

    /// Run `Events.Consumers.Inspect`.
    pub async fn inspect_consumer(&self, input: &Value) -> Result<Value, EventsQueryError> {
        inspect_consumer(&self.runtime, self.resolver.as_ref(), input)
            .await
            .map_err(|error| {
                if error.contains("consumer not found") || error.contains("404") {
                    EventsQueryError::ConsumerNotFound(
                        input
                            .get("resourceId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    )
                } else {
                    EventsQueryError::Consumer(error)
                }
            })
    }
}

fn parse_event_filter(input: &Value) -> Result<(EventsFilter, String), EventsQueryError> {
    let page = input.get("page").unwrap_or(&Value::Null);
    let limit = page.get("limit").and_then(Value::as_u64).unwrap_or(100);
    if limit == 0 || limit > 500 {
        return Err(EventsQueryError::Validation {
            field: "page.limit",
            details: "must be between 1 and 500".to_owned(),
        });
    }
    let sort = input.get("sort");
    let normalized_array = |key: &str| {
        let mut values = input
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        values.sort_by_key(Value::to_string);
        values.dedup();
        values
    };
    let query = json!({
        "search": input.get("search").unwrap_or(&Value::Null),
        "subject": input.get("subject").unwrap_or(&Value::Null),
        "ownerContractId": input.get("ownerContractId").unwrap_or(&Value::Null),
        "ownerEventName": input.get("ownerEventName").unwrap_or(&Value::Null),
        "includeEventTypes": normalized_array("includeEventTypes"),
        "excludeEventTypes": normalized_array("excludeEventTypes"),
        "publisherDeploymentId": input.get("publisherDeploymentId").unwrap_or(&Value::Null),
        "publisherParticipantId": input.get("publisherParticipantId").unwrap_or(&Value::Null),
        "resolution": normalized_array("resolution"),
        "verificationStatus": normalized_array("verificationStatus"),
        "integrityExceptionOnly": input.get("integrityExceptionOnly").and_then(Value::as_bool).unwrap_or(false),
        "window": input.get("window").unwrap_or(&Value::Null),
        "sort": {
            "field": sort.and_then(|value| value.get("field")).and_then(Value::as_str).unwrap_or("eventTime"),
            "direction": sort.and_then(|value| value.get("direction")).and_then(Value::as_str).unwrap_or("desc")
        }
    });
    let query_digest = pagination_query_digest("events.Query", &query).map_err(|_| {
        EventsQueryError::Validation {
            field: "page.cursor",
            details: "invalid pagination".to_owned(),
        }
    })?;
    let after = page
        .get("cursor")
        .and_then(Value::as_str)
        .map(|cursor| {
            decode_pagination_cursor::<(String, u64)>(cursor, &query_digest).map_err(|_| {
                EventsQueryError::Validation {
                    field: "page.cursor",
                    details: "invalid pagination".to_owned(),
                }
            })
        })
        .transpose()?;
    Ok((
        EventsFilter {
            search: input
                .get("search")
                .and_then(Value::as_str)
                .map(str::to_string),
            subject: input
                .get("subject")
                .and_then(Value::as_str)
                .map(str::to_string),
            owner_contract_id: input
                .get("ownerContractId")
                .and_then(Value::as_str)
                .map(str::to_string),
            owner_event_name: input
                .get("ownerEventName")
                .and_then(Value::as_str)
                .map(str::to_string),
            include_event_types: event_type_array(input, "includeEventTypes"),
            exclude_event_types: event_type_array(input, "excludeEventTypes"),
            publisher_deployment_id: input
                .get("publisherDeploymentId")
                .and_then(Value::as_str)
                .map(str::to_string),
            publisher_participant_id: input
                .get("publisherParticipantId")
                .and_then(Value::as_str)
                .map(str::to_string),
            resolution: string_array(input, "resolution"),
            verification_status: string_array(input, "verificationStatus"),
            integrity_exception_only: input
                .get("integrityExceptionOnly")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            since: input
                .get("window")
                .and_then(Value::as_str)
                .and_then(|window| {
                    window_config(window).and_then(|(seconds, _)| window_since(seconds))
                }),
            after,
            limit,
            sort_field: sort
                .and_then(|value| value.get("field"))
                .and_then(Value::as_str)
                .unwrap_or("eventTime")
                .to_string(),
            sort_direction: sort
                .and_then(|value| value.get("direction"))
                .and_then(Value::as_str)
                .unwrap_or("desc")
                .to_string(),
        },
        query_digest,
    ))
}

fn event_type_array(input: &Value, key: &str) -> Vec<EventTypeRef> {
    input
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(EventTypeRef {
                owner_contract_id: item.get("ownerContractId")?.as_str()?.to_string(),
                owner_event_name: item.get("ownerEventName")?.as_str()?.to_string(),
            })
        })
        .collect()
}

fn string_array(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn window_config(window: &str) -> Option<(i64, i64)> {
    match window {
        "15m" => Some((15 * 60, 60)),
        "1h" => Some((60 * 60, 5 * 60)),
        "6h" => Some((6 * 60 * 60, 30 * 60)),
        "24h" => Some((24 * 60 * 60, 60 * 60)),
        "7d" => Some((7 * 24 * 60 * 60, 6 * 60 * 60)),
        _ => None,
    }
}

fn window_since(seconds: i64) -> Option<i64> {
    let since = time::OffsetDateTime::now_utc() - time::Duration::seconds(seconds);
    i64::try_from(since.unix_timestamp_nanos()).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trellis_protocol::{encode_pagination_cursor, pagination_query_digest};

    use super::parse_event_filter;

    #[test]
    fn event_cursor_rejects_query_bound_mismatch() {
        let digest = pagination_query_digest(
            "events.Query",
            &json!({
                "search": null,
                "subject": null,
                "ownerContractId": null,
                "ownerEventName": null,
                "includeEventTypes": [],
                "excludeEventTypes": [],
                "publisherDeploymentId": null,
                "publisherParticipantId": null,
                "resolution": [],
                "verificationStatus": [],
                "integrityExceptionOnly": false,
                "window": null,
                "sort": { "field": "eventTime", "direction": "desc" }
            }),
        )
        .expect("digest");
        let cursor =
            encode_pagination_cursor(&digest, &("2026-01-01T00:00:00.1Z".to_owned(), 7_u64))
                .expect("cursor");

        assert!(parse_event_filter(&json!({
            "subject": "events.v1.other.Created",
            "page": { "cursor": cursor }
        }))
        .is_err());
    }
}
