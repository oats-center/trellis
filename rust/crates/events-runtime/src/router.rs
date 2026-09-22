//! Router construction for the built-in Events runtime.

use trellis_rs::service::{DeclaredRpcError, Router, ServerError};
use trellis_runtime_apis::apis::trellis_events_v1::rpc;

use crate::query::{EventsQuery, EventsQueryError};
use crate::wire::{generated_input, generated_output};

/// Build an Events RPC router backed by a SQL projection query adapter.
pub fn build_router_with_query(query: EventsQuery) -> Router {
    let mut router = Router::new();
    router.set_provider_deployment_id("dep_trellis_events_runtime");
    router.register_rpc::<rpc::Query, _, _>({
        let query = query.clone();
        move |_ctx, input| {
            let query = query.clone();
            async move {
                let input = generated_input(input, &["limit"])?;
                let output = query.query_events(&input).await.map_err(map_query_error)?;
                generated_output(
                    output,
                    &["headerCount", "payloadSizeBytes", "streamSequence"],
                )
            }
        }
    });
    router.register_rpc::<rpc::Inspect, _, _>({
        let query = query.clone();
        move |_ctx, input| {
            let query = query.clone();
            async move {
                let input = generated_input(input, &["streamSequence"])?;
                let output = query.inspect_event(&input).await.map_err(map_query_error)?;
                generated_output(
                    output,
                    &["headerCount", "payloadSizeBytes", "streamSequence"],
                )
            }
        }
    });
    router.register_rpc::<rpc::Metrics, _, _>({
        let query = query.clone();
        move |_ctx, input| {
            let query = query.clone();
            async move {
                let input = generated_input(input, &[])?;
                let output = query.metrics(&input).await.map_err(map_query_error)?;
                generated_output(
                    output,
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
            }
        }
    });
    router.register_rpc::<rpc::Diagnostics, _, _>({
        let query = query.clone();
        move |_ctx, _input| {
            let query = query.clone();
            async move {
                generated_output(
                    query.diagnostics().await.map_err(map_query_error)?,
                    &["lastStreamSequence", "revision"],
                )
            }
        }
    });
    router.register_rpc::<rpc::ConsumersQuery, _, _>({
        let query = query.clone();
        move |_ctx, input| {
            let query = query.clone();
            async move {
                let input = generated_input(input, &["limit"])?;
                let output = query
                    .query_consumers(&input)
                    .await
                    .map_err(map_query_error)?;
                generated_output(
                    output,
                    &[
                        "ackPending",
                        "ackWaitMs",
                        "maxDeliver",
                        "pending",
                        "redelivered",
                        "waitingPulls",
                    ],
                )
            }
        }
    });
    router.register_rpc::<rpc::ConsumersInspect, _, _>({
        let query = query.clone();
        move |_ctx, input| {
            let query = query.clone();
            async move {
                let input = generated_input(input, &[])?;
                let output = query
                    .inspect_consumer(&input)
                    .await
                    .map_err(map_query_error)?;
                generated_output(
                    output,
                    &[
                        "ackPending",
                        "ackWaitMs",
                        "maxDeliver",
                        "pending",
                        "redelivered",
                        "waitingPulls",
                    ],
                )
            }
        }
    });
    router
}

pub(crate) fn map_query_error(error: EventsQueryError) -> ServerError {
    match error {
        EventsQueryError::EventNotFound {
            event_id,
            stream_sequence,
        } => {
            let id = event_id
                .clone()
                .unwrap_or_else(|| stream_sequence.unwrap_or_default().to_string());
            ServerError::DeclaredRpc(DeclaredRpcError::new(
                "NotFoundError",
                "Event not found",
                [
                    ("type", serde_json::json!("NotFoundError")),
                    ("id", serde_json::json!(id)),
                    ("message", serde_json::json!("Event not found")),
                    (
                        "context",
                        serde_json::json!({
                            "eventId": event_id,
                            "streamSequence": stream_sequence,
                        }),
                    ),
                ],
            ))
        }
        EventsQueryError::ConsumerNotFound(name) => {
            ServerError::DeclaredRpc(DeclaredRpcError::new(
                "NotFoundError",
                format!("Consumer '{name}' not found"),
                [
                    ("type", serde_json::json!("NotFoundError")),
                    ("id", serde_json::json!(name)),
                    (
                        "message",
                        serde_json::json!(format!("Consumer '{name}' not found")),
                    ),
                    ("context", serde_json::json!({ "resourceId": name })),
                ],
            ))
        }
        EventsQueryError::Validation { field, details } => {
            ServerError::DeclaredRpc(DeclaredRpcError::new(
                "ValidationError",
                format!("Invalid {field}: {details}"),
                [
                    ("field", serde_json::json!(field)),
                    ("details", serde_json::json!(details)),
                ],
            ))
        }
        other => ServerError::Nats(format!("Events RPC query failed: {other}")),
    }
}
