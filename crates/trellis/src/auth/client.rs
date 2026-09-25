use super::{AdminSessionState, TrellisAuthError};
use crate::client::{SessionAuth, UserConnectOptions, UserSessionCredentials};
use crate::generated::Client;

/// Connect an authenticated admin client from stored session state.
pub async fn connect_admin_client_async(
    state: &AdminSessionState,
) -> Result<Client, TrellisAuthError> {
    Ok(Client::connect_user(UserConnectOptions::new(
        &state.trellis_url,
        5_000,
        UserSessionCredentials {
            login_session_id: &state.login_session_id,
            session_key_seed_base64url: &state.session_seed,
        },
        &state.participant_id,
    ))
    .await?)
}

/// Derive the public session key for a base64url-encoded session seed.
pub fn session_public_key(seed_base64url: &str) -> Result<String, TrellisAuthError> {
    Ok(SessionAuth::from_seed_base64url(seed_base64url)?.session_key)
}
