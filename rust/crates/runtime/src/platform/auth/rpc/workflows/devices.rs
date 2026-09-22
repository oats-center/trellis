use super::super::{
    AuthRpcProcessor, AuthorizationStateError, ProvisionedIdentityKind, RuntimeInstanceState,
    ValidatedRequest, Value,
};

pub(super) async fn dispatch(
    processor: &AuthRpcProcessor,
    subject: &str,
    payload: &[u8],
    caller: ValidatedRequest,
) -> Result<Value, AuthorizationStateError> {
    match subject {
        "rpc.v1.auth.Devices.Provision" => processor.devices_provision(payload, &caller).await,
        "rpc.v1.auth.Devices.List" => processor.devices_list(payload).await,
        "rpc.v1.auth.DeviceUserAuthorities.List" => {
            processor.device_user_authorities_list(payload).await
        }
        "rpc.v1.auth.DeviceUserAuthorities.Revoke" => {
            processor
                .device_user_authorities_revoke(payload, &caller)
                .await
        }
        "rpc.v1.auth.Devices.Enable" => {
            processor
                .provisioned_instance_set_state(
                    payload,
                    &caller,
                    ProvisionedIdentityKind::Device,
                    RuntimeInstanceState::Active,
                )
                .await
        }
        "rpc.v1.auth.Devices.Disable" => {
            processor
                .provisioned_instance_set_state(
                    payload,
                    &caller,
                    ProvisionedIdentityKind::Device,
                    RuntimeInstanceState::Disabled,
                )
                .await
        }
        "rpc.v1.auth.Devices.Remove" => {
            processor
                .provisioned_instance_set_state(
                    payload,
                    &caller,
                    ProvisionedIdentityKind::Device,
                    RuntimeInstanceState::Revoked,
                )
                .await
        }
        "rpc.v1.auth.DeviceUserAuthorities.Reviews.List" => {
            processor.activation_reviews_list(payload).await
        }
        "rpc.v1.auth.DeviceUserAuthorities.Reviews.Decide" => {
            processor.activation_reviews_decide(payload, &caller).await
        }
        _ => unknown(subject),
    }
}

fn unknown(subject: &str) -> Result<Value, AuthorizationStateError> {
    Err(AuthorizationStateError::InvalidRecord(format!(
        "Auth RPC is not implemented by Rust: {subject}"
    )))
}
