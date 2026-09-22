use std::str::FromStr;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Number, Value};
use trellis_rs::service::ServerError;

pub(crate) fn generated_input(
    input: impl Serialize,
    integer_fields: &[&str],
) -> Result<Value, ServerError> {
    let mut value = serde_json::to_value(input)?;
    convert_integer_fields(&mut value, integer_fields, false);
    Ok(value)
}

pub(crate) fn generated_output<T: DeserializeOwned>(
    mut value: Value,
    integer_fields: &[&str],
) -> Result<T, ServerError> {
    convert_integer_fields(&mut value, integer_fields, true);
    serde_json::from_value(value).map_err(|error| {
        ServerError::Nats(format!("failed to encode generated Events value: {error}"))
    })
}

fn convert_integer_fields(value: &mut Value, integer_fields: &[&str], encode: bool) {
    match value {
        Value::Array(values) => {
            for value in values {
                convert_integer_fields(value, integer_fields, encode);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if integer_fields.contains(&key.as_str()) {
                    convert_integer_value(value, encode);
                } else {
                    convert_integer_fields(value, integer_fields, encode);
                }
            }
        }
        _ => {}
    }
}

fn convert_integer_value(value: &mut Value, encode: bool) {
    match value {
        Value::Array(values) => {
            for value in values {
                convert_integer_value(value, encode);
            }
        }
        Value::Number(number) if encode => *value = Value::String(number.to_string()),
        Value::String(text) if !encode => {
            if let Ok(number) = Number::from_str(text) {
                *value = Value::Number(number);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trellis_runtime_apis::apis::trellis_events_v1::rpc;

    use super::generated_output;
    use crate::EventsStore;

    #[test]
    fn generated_query_output_uses_cursor_contract() {
        let output: rpc::QueryOutput =
            generated_output(json!({ "items": [], "page": { "nextCursor": null } }), &[])
                .expect("convert generated output");

        assert!(output.items.is_empty());
        assert!(output.page.next_cursor.is_none());
    }

    #[test]
    fn integer_array_fields_are_encoded_for_generated_uint64() {
        let mut value = json!({ "transitionReferences": [7, 9] });
        super::convert_integer_fields(&mut value, &["transitionReferences"], true);
        assert_eq!(value, json!({ "transitionReferences": ["7", "9"] }));
    }

    #[test]
    fn final_dead_letter_summary_and_diagnostics_shapes_deserialize() {
        let summary = json!({
            "deadLetterId": "dead-letter-1",
            "deliveries": 5,
            "generation": 1,
            "lastError": "boom",
            "originalSequence": 42,
            "originalStream": "trellis",
            "resourceId": "consumer-resource",
            "revision": 3,
            "state": "replaying",
            "updatedAt": "2026-09-10T00:00:00Z",
        });
        let output: rpc::DeadLettersReplayOutput = generated_output(
            json!({ "deadLetter": summary }),
            &["originalSequence", "revision"],
        )
        .expect("convert dead-letter summary");
        assert_eq!(output.dead_letter.resource_id, "consumer-resource");

        let diagnostics: rpc::DiagnosticsOutput = generated_output(
            json!({
                "asOf": "2026-09-10T00:00:00Z",
                "completeSince": null,
                "gapDetected": false,
                "lastStreamSequence": 42,
                "retainedFrom": null,
                "revision": 40,
            }),
            &["lastStreamSequence", "revision"],
        )
        .expect("convert diagnostics");
        assert_eq!(diagnostics.last_stream_sequence.0, 42);
    }

    #[test]
    fn projected_metrics_match_final_summary_contract() {
        let store = EventsStore::open_in_memory().expect("open store");
        let output: rpc::MetricsOutput = generated_output(
            store.metrics(None, None).expect("query metrics"),
            &[
                "authUnavailable",
                "count",
                "dead",
                "dismissed",
                "integrityExceptions",
                "invalidSignature",
                "malformed",
                "missingProof",
                "missingSession",
                "outsideSessionWindow",
                "payloadSizeBytes",
                "replayPending",
                "replaying",
                "resolved",
                "subjectDenied",
                "total",
                "uniqueSubjects",
                "unresolved",
                "verified",
            ],
        )
        .expect("convert metrics");
        assert_eq!(output.summary.total.0, 0);
        assert_eq!(output.summary.dead_letters_by_state.dead.0, 0);
    }
}
