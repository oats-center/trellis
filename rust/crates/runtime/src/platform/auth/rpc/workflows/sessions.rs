use super::super::{AuthRpcProcessor, AuthorizationStateError, ValidatedRequest, Value};

pub(super) async fn dispatch(
    processor: &AuthRpcProcessor,
    subject: &str,
    payload: &[u8],
    caller: ValidatedRequest,
) -> Result<Value, AuthorizationStateError> {
    match subject {
        "rpc.v1.auth.Sessions.Me" => processor.sessions_me(caller).await,
        "rpc.v1.auth.Sessions.List" => processor.sessions_list(payload, &caller).await,
        "rpc.v1.auth.Sessions.Revoke" => processor.sessions_revoke(payload, Some(&caller)).await,
        "rpc.v1.auth.Sessions.Logout" => processor.sessions_logout(payload, &caller).await,
        "rpc.v1.auth.Connections.List" => processor.connections_list(payload, caller).await,
        "rpc.v1.auth.Connections.Kick" => processor.connections_kick(payload, &caller).await,
        _ => unknown(subject),
    }
}

fn unknown(subject: &str) -> Result<Value, AuthorizationStateError> {
    Err(AuthorizationStateError::InvalidRecord(format!(
        "Auth RPC is not implemented by Rust: {subject}"
    )))
}
