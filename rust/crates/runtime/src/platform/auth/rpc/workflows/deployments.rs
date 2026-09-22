use super::super::{
    require_admin, AuthRpcProcessor, AuthorizationStateError, DeploymentProfileState,
    ProvisionedIdentityKind, RuntimeInstanceState, ValidatedRequest, Value,
};

pub(super) async fn dispatch(
    processor: &AuthRpcProcessor,
    subject: &str,
    payload: &[u8],
    caller: ValidatedRequest,
) -> Result<Value, AuthorizationStateError> {
    require_admin(&caller)?;
    match subject {
        "rpc.v1.auth.Deployments.Create" => processor.deployments_create(payload, &caller).await,
        "rpc.v1.auth.Deployments.List" => processor.deployments_list(payload).await,
        "rpc.v1.auth.Deployments.Get" => processor.deployments_get(payload, &caller).await,
        "rpc.v1.auth.Deployments.Enable" => {
            processor
                .deployments_set_state(payload, &caller, DeploymentProfileState::Active)
                .await
        }
        "rpc.v1.auth.Deployments.Disable" => {
            processor
                .deployments_set_state(payload, &caller, DeploymentProfileState::Disabled)
                .await
        }
        "rpc.v1.auth.Deployments.Remove" => {
            processor
                .deployments_set_state(payload, &caller, DeploymentProfileState::Removed)
                .await
        }
        "rpc.v1.auth.ServiceInstances.Provision" => {
            processor
                .service_instances_provision(payload, &caller)
                .await
        }
        "rpc.v1.auth.ServiceInstances.List" => processor.service_instances_list(payload).await,
        "rpc.v1.auth.ServiceInstances.Enable" => {
            processor
                .provisioned_instance_set_state(
                    payload,
                    &caller,
                    ProvisionedIdentityKind::Service,
                    RuntimeInstanceState::Active,
                )
                .await
        }
        "rpc.v1.auth.ServiceInstances.Disable" => {
            processor
                .provisioned_instance_set_state(
                    payload,
                    &caller,
                    ProvisionedIdentityKind::Service,
                    RuntimeInstanceState::Disabled,
                )
                .await
        }
        "rpc.v1.auth.ServiceInstances.Remove" => {
            processor
                .provisioned_instance_set_state(
                    payload,
                    &caller,
                    ProvisionedIdentityKind::Service,
                    RuntimeInstanceState::Revoked,
                )
                .await
        }
        _ => unknown(subject),
    }
}

fn unknown(subject: &str) -> Result<Value, AuthorizationStateError> {
    Err(AuthorizationStateError::InvalidRecord(format!(
        "Auth RPC is not implemented by Rust: {subject}"
    )))
}
