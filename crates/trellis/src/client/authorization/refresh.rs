use std::sync::Arc;

use serde_json::json;
use trellis_protocol::{
    AuthorizationContextRefreshSessionProofInput, AuthorizationPrincipalKind,
    NativeBootstrapSessionProofInput, SessionProofInput,
};

use super::super::{proof::new_request_id, SessionAuth, TrellisClientError};
use super::own_context::{system_now_millis, AuthorizationContextCache};
use super::types::{AuthorizationCredential, AuthorizationInstallation};

fn is_terminal_refresh_error(code: &str) -> bool {
    matches!(
        code,
        "session_not_found"
            | "session_expired"
            | "session_revoked"
            | "user_not_found"
            | "user_inactive"
            | "identity_not_found"
            | "identity_revoked"
            | "grant_binding_missing"
            | "grant_binding_revoked"
            | "grant_binding_expired"
            | "participant_not_installed"
            | "deployment_inactive"
            | "instance_inactive"
            | "device_inactive"
            | "activation_required"
            | "login_not_found"
            | "context_owner_mismatch"
            | "authority_revoked"
            | "authority_expired"
            | "authority_not_found"
            | "delegation_expired"
            | "context_refresh_mismatch"
            | "invalid_proof"
    )
}

/// Obtain or renew connection authority using only the owner credential and proof.
pub(crate) async fn refresh(
    cache: &AuthorizationContextCache,
    auth: &SessionAuth,
    promote: bool,
) -> Result<(String, bool), TrellisClientError> {
    if auth.session_key != cache.session_key {
        return Err(TrellisClientError::Bootstrap(
            "refresh signing key does not belong to this connection".into(),
        ));
    }
    let observed_digest = cache.stored_context_digest().ok();
    let _refresh = cache.lock_refresh().await;
    if observed_digest != cache.stored_context_digest().ok() {
        return Ok((cache.stored_context_digest()?, false));
    }
    let previous = cache.state_snapshot()?;
    let request_started_at = system_now_millis()?;
    let issued_at = cache.corrected_now_millis()?;
    let mut request = json!({"requestId": new_request_id(), "connectionId": cache.connection_id});
    if let Some(name) = &cache.name {
        request["name"] = json!(name);
    }
    let (route, input, signer) = match cache.credential.as_ref() {
        AuthorizationCredential::Native {
            kind,
            identity,
            package_evidence,
            participant_path,
            companion,
        } => {
            request["identityKeyId"] = json!(identity.key_id());
            request["sessionKey"] = json!(auth.session_key);
            request["iat"] = json!(issued_at);
            request["packageEvidence"] = serde_json::to_value(package_evidence)?;
            request["participantPath"] = json!(participant_path);
            request["packageDigest"] = json!(package_evidence.root_digest());
            if let Some(companion) = companion {
                let companion_connection_id = ulid::Ulid::new().to_string();
                let companion_request_id = ulid::Ulid::new().to_string();
                let (_, companion_session_key) = crate::auth::generate_session_keypair();
                let digest = trellis_protocol::digest_json(&json!({
                    "format": "trellis.device.user-companion.v1",
                    "origin": cache.http().origin(),
                    "identityKeyId": identity.key_id(),
                    "participantId": companion.participant_id,
                    "connectionId": companion_connection_id,
                    "requestId": companion_request_id,
                    "issuedAt": issued_at,
                    "sessionKey": companion_session_key,
                }))
                .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
                let digest = crate::client::proof::base64url_decode(&digest)?;
                request["companion"] = json!({
                    "connectionId": companion_connection_id,
                    "requestId": companion_request_id,
                    "issuedAt": issued_at,
                    "sessionKey": companion_session_key,
                    "proof": companion.installation.sign_bytes(&digest),
                });
            }
            let input = NativeBootstrapSessionProofInput {
                origin: cache.http().origin(),
                unsigned_request: request.clone(),
            };
            match kind {
                AuthorizationPrincipalKind::Service => (
                    "/bootstrap/service",
                    SessionProofInput::service_bootstrap(input),
                    identity.as_ref(),
                ),
                AuthorizationPrincipalKind::Device => (
                    "/bootstrap/device",
                    SessionProofInput::device_bootstrap(input),
                    identity.as_ref(),
                ),
                AuthorizationPrincipalKind::User => {
                    return Err(TrellisClientError::Bootstrap(
                        "user login cannot use a native credential".into(),
                    ))
                }
            }
        }
        AuthorizationCredential::User {
            login_session_id,
            installation,
        } => {
            request["loginSessionId"] = json!(login_session_id);
            request["issuedAt"] = json!(issued_at);
            request["sessionKey"] = json!(cache.session_key);
            request["currentContextDigest"] = json!(observed_digest);
            (
                "/auth/context/refresh",
                SessionProofInput::authorization_context_refresh(
                    AuthorizationContextRefreshSessionProofInput {
                        origin: cache.http().origin(),
                        session_public_key: installation.session_key.clone(),
                        unsigned_request: request.clone(),
                    },
                ),
                installation.as_ref(),
            )
        }
    };
    let input = input.map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
    request["proof"] = serde_json::to_value(signer.sign_session_proof(&input)?)?;
    let response = cache.http().post_json(route, &request).await?;
    let server_now = response["serverNow"]
        .as_i64()
        .filter(|now| (0..=9_007_199_254_740_991).contains(now))
        .ok_or_else(|| {
            TrellisClientError::Bootstrap("bootstrap omitted a safe server time".into())
        })?;
    let midpoint = request_started_at
        .checked_add(system_now_millis()?)
        .and_then(|sum| sum.checked_div(2))
        .ok_or_else(|| TrellisClientError::Bootstrap("bootstrap time overflow".into()))?;
    let mut runtime = response["runtime"].clone();
    runtime
        .as_object_mut()
        .ok_or_else(|| TrellisClientError::Bootstrap("bootstrap runtime is not an object".into()))?
        .insert("transports".into(), response["transports"].clone());
    let mut authorization = response
        .get("authorization")
        .filter(|value| !value.is_null())
        .cloned();
    if let (Some(object), Some(companion)) = (
        authorization
            .as_mut()
            .and_then(serde_json::Value::as_object_mut),
        response.get("companion").filter(|value| !value.is_null()),
    ) {
        object.insert("companion".to_owned(), companion.clone());
    }
    let installation = AuthorizationInstallation {
        context: serde_json::from_value(response["authorizationContext"].clone())?,
        routing: serde_json::from_value(response["routing"].clone())?,
        runtime: serde_json::from_value(runtime)?,
        api_bindings: serde_json::from_value(response["apiBindings"].clone())?,
        server_clock_offset_ms: server_now
            .checked_sub(midpoint)
            .ok_or_else(|| TrellisClientError::Bootstrap("bootstrap time overflow".into()))?,
        authorization,
    };
    let context_digest = if promote {
        cache.install_initial(installation)?;
        cache.retained_context_digest()?
    } else {
        cache.prepare(installation)?
    };
    Ok((
        context_digest,
        previous.runtime.as_ref() != Some(&cache.runtime_binding()?),
    ))
}

/// Background own-context refresh on the retained NATS connection.
pub(crate) fn spawn_authorization_context_refresh_task(
    contexts: Arc<AuthorizationContextCache>,
    auth: Arc<SessionAuth>,
    nats: async_nats::Client,
    applied_native_authorization: Arc<
        tokio::sync::Mutex<super::super::connection::AppliedNativeAuthorization>,
    >,
    provider: super::AuthorizationProviderCache,
    timeout_ms: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let delay = match contexts.refresh_delay() {
                Ok(delay) => delay,
                Err(error) => {
                    tracing::warn!(%error, "authorization context refresh stopped");
                    return;
                }
            };
            let request = tokio::select! {
                () = tokio::time::sleep(delay) => super::own_context::AuthorizationRefreshRequest {
                    context_digest: contexts.stored_context_digest().ok(),
                    refresh_credential: true,
                },
                request = contexts.wait_refresh_request() => request,
            };
            if request.context_digest.is_some()
                && request.context_digest != contexts.stored_context_digest().ok()
            {
                continue;
            }
            let mut applied = applied_native_authorization.lock().await;
            if !request.refresh_credential && request.context_digest.is_some() {
                if let Some(digest) = request.context_digest {
                    let epoch = provider.epoch();
                    match provider.retain_own_context(&digest, epoch).await {
                        Ok(()) => {
                            let promote = contexts.candidate_digest().is_ok();
                            match provider.finalize_own_installation(&digest, promote) {
                                Ok(()) => continue,
                                Err(error) => {
                                    tracing::warn!(%error, "authorization coverage publication failed")
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "authorization coverage reconciliation will retry")
                        }
                    }
                }
                drop(applied);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                contexts.request_coverage_reconciliation();
                continue;
            }
            match contexts.prepare_refresh(&auth).await {
                Ok((candidate_digest, _)) => {
                    let refreshed =
                        match super::super::connection::AppliedNativeAuthorization::from_cache(
                            &contexts,
                        ) {
                            Ok(state) => state,
                            Err(error) => {
                                tracing::warn!(%error, "refreshed native runtime is invalid");
                                continue;
                            }
                        };
                    if let Err(error) =
                        super::super::connection::apply_native_authorization_refresh(
                            &nats,
                            &mut applied,
                            refreshed,
                            timeout_ms,
                        )
                        .await
                    {
                        tracing::warn!(%error, "native connection refresh will retry");
                        contexts.request_refresh();
                    } else if let Err(error) = provider
                        .retain_own_context(&candidate_digest, provider.epoch())
                        .await
                    {
                        tracing::warn!(%error, "refreshed own-context coverage is unavailable");
                        contexts.request_refresh();
                    } else if let Err(error) =
                        provider.finalize_own_installation(&candidate_digest, true)
                    {
                        tracing::warn!(%error, "refreshed authorization promotion failed");
                        contexts.request_refresh();
                    }
                }
                Err(TrellisClientError::BootstrapHttp { status, code })
                    if is_terminal_refresh_error(&code) =>
                {
                    tracing::warn!(status, "authorization context refresh rejected");
                    if let Err(error) = contexts.clear() {
                        tracing::warn!(%error, "failed to clear rejected authorization context");
                    }
                    let _ = nats.drain().await;
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "authorization context refresh will retry");
                    drop(applied);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    contexts.request_refresh();
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::is_terminal_refresh_error;

    #[test]
    fn refresh_terminality_uses_exact_machine_codes() {
        assert!(is_terminal_refresh_error("session_revoked"));
        assert!(is_terminal_refresh_error("grant_binding_revoked"));
        assert!(is_terminal_refresh_error("context_refresh_mismatch"));
        assert!(is_terminal_refresh_error("login_not_found"));
        assert!(is_terminal_refresh_error("context_owner_mismatch"));
        assert!(!is_terminal_refresh_error("required_resources_unavailable"));
        assert!(!is_terminal_refresh_error("dependency_pending"));
        assert!(!is_terminal_refresh_error("resource_pending"));
        assert!(!is_terminal_refresh_error("authorization_pending"));
        assert!(!is_terminal_refresh_error("session_revoked later"));
    }
}
