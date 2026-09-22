use serde_json::{json, Value};
use trellis_runtime_apis::types::{
    ResourcesDestroyRequest, ResourcesInspectRequest, ResourcesQueryRequest,
};

use super::super::{now_millis, require_admin, AuthRpcProcessor, ValidatedRequest};
use crate::platform::auth::AuthorizationStateError;

pub(super) async fn dispatch(
    processor: &AuthRpcProcessor,
    subject: &str,
    payload: &[u8],
    caller: ValidatedRequest,
) -> Result<Value, AuthorizationStateError> {
    require_admin(&caller)?;
    let input: Value = serde_json::from_slice(payload)
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    let repository = processor.service.repository();
    match subject {
        "rpc.v1.core.Resources.Query" => {
            serde_json::from_value::<ResourcesQueryRequest>(input.clone())
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            repository
                .query_resources("core.Resources.Query", input)
                .await
        }
        "rpc.v1.core.Resources.Inspect" => {
            let request: ResourcesInspectRequest = serde_json::from_value(input)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            Ok(json!({"resource": repository.inspect_resource(request.resource_id.0).await?}))
        }
        "rpc.v1.core.Resources.Destroy" => {
            let request: ResourcesDestroyRequest = serde_json::from_value(input)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            let expected_revision =
                u64::try_from(request.expected_revision.0 .0).map_err(|_| {
                    AuthorizationStateError::InvalidRecord(
                        "expectedRevision must be positive".to_owned(),
                    )
                })?;
            if expected_revision == 0 {
                return Err(AuthorizationStateError::InvalidRecord(
                    "expectedRevision must be positive".to_owned(),
                ));
            }
            repository
                .begin_resource_destroy(
                    request.resource_id.0,
                    expected_revision,
                    request.confirm_physical_id.0,
                    now_millis()?,
                )
                .await
        }
        _ => Err(AuthorizationStateError::InvalidRecord(format!(
            "Core resource RPC is not implemented by Rust: {subject}"
        ))),
    }
}
