//! Router construction for the Jobs admin service.

use trellis_rs::service::{DeclaredRpcError, Router, ServerError};
use trellis_runtime_apis::apis::trellis_jobs_v1::rpc::{
    Cancel, DismissDLQ, GetKey, Inspect, ListDLQ, ListServices, Metrics, Query, ReplayDLQ, Retry,
    Summary,
};
use trellis_runtime_apis::types::{
    JobsCancelRequest, JobsDismissDLQRequest, JobsGetKeyRequest, JobsInspectRequest,
    JobsListDLQRequest, JobsListServicesRequest, JobsMetricsRequest, JobsQueryRequest,
    JobsReplayDLQRequest, JobsRetryRequest, JobsSummaryRequest,
};

use crate::query::{JobsQuery, JobsQueryError};

/// Build the Jobs admin RPC router backed by a SQL projection query adapter.
pub fn build_router_with_query(query: JobsQuery) -> Router {
    let mut router = Router::new();
    router.set_provider_deployment_id("dep_trellis_jobs_runtime");
    router.register_rpc::<ListServices, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsListServicesRequest| {
            let query = query.clone();
            async move { query.list_services(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<Query, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsQueryRequest| {
            let query = query.clone();
            async move { query.query_jobs(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<Summary, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsSummaryRequest| {
            let query = query.clone();
            async move { query.summary(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<Metrics, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsMetricsRequest| {
            let query = query.clone();
            async move { query.metrics(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<Inspect, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsInspectRequest| {
            let query = query.clone();
            async move { query.inspect(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<GetKey, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsGetKeyRequest| {
            let query = query.clone();
            async move { query.get_key(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<Cancel, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsCancelRequest| {
            let query = query.clone();
            async move { query.cancel_job(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<Retry, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsRetryRequest| {
            let query = query.clone();
            async move { query.retry_job(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<ListDLQ, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsListDLQRequest| {
            let query = query.clone();
            async move { query.list_dlq(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<ReplayDLQ, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsReplayDLQRequest| {
            let query = query.clone();
            async move { query.replay_dlq(&input).await.map_err(map_query_error) }
        }
    });
    router.register_rpc::<DismissDLQ, _, _>({
        let query = query.clone();
        move |_ctx, input: JobsDismissDLQRequest| {
            let query = query.clone();
            async move { query.dismiss_dlq(&input).await.map_err(map_query_error) }
        }
    });
    router
}

fn map_query_error(error: JobsQueryError) -> ServerError {
    match error {
        JobsQueryError::JobNotFound { key } => ServerError::DeclaredRpc(DeclaredRpcError::new(
            "NotFoundError",
            format!("Job '{key}' not found"),
            [
                ("resource", serde_json::json!("Job")),
                ("jobId", serde_json::json!(key)),
            ],
        )),
        JobsQueryError::JobStateConflict {
            key,
            expected,
            actual,
        } => ServerError::DeclaredRpc(DeclaredRpcError::new(
            "ValidationError",
            format!("Job '{key}' is in state '{actual}', expected {expected}"),
            [
                ("field", serde_json::json!("state")),
                ("jobKey", serde_json::json!(key)),
                ("expected", serde_json::json!(expected)),
                ("actual", serde_json::json!(actual)),
            ],
        )),
        JobsQueryError::Validation { field, details } => {
            ServerError::DeclaredRpc(DeclaredRpcError::new(
                "ValidationError",
                format!("Invalid {field}: {details}"),
                [
                    ("field", serde_json::json!(field)),
                    ("details", serde_json::json!(details)),
                ],
            ))
        }
        JobsQueryError::ConvertWireModel { model, details } => {
            ServerError::DeclaredRpc(DeclaredRpcError::new(
                "ValidationError",
                format!("Invalid {model}: {details}"),
                [
                    ("field", serde_json::json!(model)),
                    ("details", serde_json::json!(details)),
                ],
            ))
        }
        other => ServerError::Nats(format!("jobs RPC query failed: {other}")),
    }
}
