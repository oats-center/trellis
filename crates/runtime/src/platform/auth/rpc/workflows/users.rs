use super::super::{AuthRpcProcessor, AuthorizationStateError, ValidatedRequest, Value};

pub(super) async fn dispatch(
    processor: &AuthRpcProcessor,
    subject: &str,
    payload: &[u8],
    caller: ValidatedRequest,
) -> Result<Value, AuthorizationStateError> {
    match subject {
        "rpc.v1.auth.Users.Create" => processor.users_create(payload, &caller).await,
        "rpc.v1.auth.Users.Get" => processor.users_get(payload).await,
        "rpc.v1.auth.Users.Resolve" => processor.users_resolve(payload).await,
        "rpc.v1.auth.Users.List" => processor.users_list(payload).await,
        "rpc.v1.auth.Users.Update" => processor.users_update(payload, &caller).await,
        "rpc.v1.auth.Users.PasswordReset.Create" => {
            processor.password_reset_create(payload, &caller).await
        }
        "rpc.v1.auth.Users.Password.Change" => processor.password_change(payload, &caller).await,
        "rpc.v1.auth.Users.IdentityLink.Create" => {
            processor.identity_link_create(payload, &caller).await
        }
        "rpc.v1.auth.UserIdentities.List" => processor.user_identities_list(payload, &caller).await,
        "rpc.v1.auth.UserIdentities.Unlink" => {
            processor.user_identities_unlink(payload, &caller).await
        }
        "rpc.v1.auth.CapabilityGroups.List" => processor.capability_groups_list(payload).await,
        "rpc.v1.auth.CapabilityGroups.Get" => processor.capability_groups_get(payload).await,
        "rpc.v1.auth.CapabilityGroups.Put" => {
            processor.capability_groups_put(payload, &caller).await
        }
        "rpc.v1.auth.CapabilityGroups.Delete" => {
            processor.capability_groups_delete(payload, &caller).await
        }
        _ => unknown(subject),
    }
}

fn unknown(subject: &str) -> Result<Value, AuthorizationStateError> {
    Err(AuthorizationStateError::InvalidRecord(format!(
        "Auth RPC is not implemented by Rust: {subject}"
    )))
}
