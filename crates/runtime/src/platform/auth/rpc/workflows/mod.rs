mod deployments;
mod devices;
mod grants;
mod portals;
mod resources;
mod sessions;
mod users;

use super::{AuthRpcProcessor, AuthorizationStateError, ValidatedRequest, Value};

pub(super) async fn dispatch(
    processor: &AuthRpcProcessor,
    subject: &str,
    payload: &[u8],
    caller: ValidatedRequest,
) -> Result<Value, AuthorizationStateError> {
    if subject.starts_with("rpc.v1.core.Resources.") {
        resources::dispatch(processor, subject, payload, caller).await
    } else if subject.starts_with("rpc.v1.auth.Grants.")
        || subject == "rpc.v1.auth.Issuers.Revoke"
        || subject.starts_with("rpc.v1.auth.Participants.")
        || subject == "rpc.v1.auth.Deployments.Apply"
    {
        grants::dispatch(processor, subject, payload, caller).await
    } else if subject.starts_with("rpc.v1.auth.Sessions.")
        || subject.starts_with("rpc.v1.auth.Connections.")
    {
        sessions::dispatch(processor, subject, payload, caller).await
    } else if subject.starts_with("rpc.v1.auth.Portals.")
        || subject == "rpc.v1.auth.Capabilities.List"
    {
        portals::dispatch(processor, subject, payload, caller).await
    } else if subject.starts_with("rpc.v1.auth.Users.")
        || subject.starts_with("rpc.v1.auth.UserIdentities.")
        || subject.starts_with("rpc.v1.auth.CapabilityGroups.")
    {
        users::dispatch(processor, subject, payload, caller).await
    } else if subject.starts_with("rpc.v1.auth.Deployments.")
        || subject.starts_with("rpc.v1.auth.ServiceInstances.")
    {
        deployments::dispatch(processor, subject, payload, caller).await
    } else if subject.starts_with("rpc.v1.auth.Devices.")
        || subject.starts_with("rpc.v1.auth.DeviceUserAuthorities.")
    {
        devices::dispatch(processor, subject, payload, caller).await
    } else {
        Err(AuthorizationStateError::InvalidRecord(format!(
            "Auth RPC is not implemented by Rust: {subject}"
        )))
    }
}
