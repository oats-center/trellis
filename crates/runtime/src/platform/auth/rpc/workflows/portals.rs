use super::super::{AuthRpcProcessor, AuthorizationStateError, ValidatedRequest, Value};

pub(super) async fn dispatch(
    processor: &AuthRpcProcessor,
    subject: &str,
    payload: &[u8],
    caller: ValidatedRequest,
) -> Result<Value, AuthorizationStateError> {
    match subject {
        "rpc.v1.auth.Portals.List" => processor.portals_list(payload).await,
        "rpc.v1.auth.Portals.Get" => processor.portals_get(payload).await,
        "rpc.v1.auth.Portals.Put" => processor.portals_put(payload, &caller).await,
        "rpc.v1.auth.Portals.Remove" => processor.portals_remove(payload, &caller).await,
        "rpc.v1.auth.Portals.LoginSettings.Get" => processor.portal_settings_get(payload).await,
        "rpc.v1.auth.Portals.LoginSettings.Update" => {
            processor.portal_settings_update(payload, &caller).await
        }
        "rpc.v1.auth.Portals.Routes.Put" => processor.portal_route_put(payload, &caller).await,
        "rpc.v1.auth.Portals.Routes.Remove" => {
            processor.portal_route_remove(payload, &caller).await
        }
        "rpc.v1.auth.Portals.GrantOverrides.List" => {
            processor.portal_grant_overrides_list(payload).await
        }
        "rpc.v1.auth.Portals.GrantOverrides.Put" => {
            processor.portal_grant_overrides_put(payload, &caller).await
        }
        "rpc.v1.auth.Portals.GrantOverrides.Remove" => {
            processor
                .portal_grant_overrides_remove(payload, &caller)
                .await
        }
        "rpc.v1.auth.Capabilities.List" => processor.capabilities_list(payload).await,
        _ => unknown(subject),
    }
}

fn unknown(subject: &str) -> Result<Value, AuthorizationStateError> {
    Err(AuthorizationStateError::InvalidRecord(format!(
        "Auth RPC is not implemented by Rust: {subject}"
    )))
}
