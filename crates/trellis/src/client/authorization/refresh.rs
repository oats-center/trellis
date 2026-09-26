use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use trellis_protocol::{
    AuthorizationContextRefreshSessionProofInput, AuthorizationPrincipalKind,
    NativeBootstrapSessionProofInput, SessionProofInput,
};

use super::super::connection::{
    apply_native_authorization_refresh, AppliedNativeAuthorization,
    NativeAuthorizationRefreshContext,
};
use super::super::{proof::new_request_id, SessionAuth, TrellisClientError};
use super::own_context::{
    system_now_millis, AuthorizationContextCache, AuthorizationRefreshRequest,
};
use super::provider_cache::AuthorizationProviderCache;
use super::rotation::AuthorizationTransportRotation;
use super::types::{AuthorizationCredential, AuthorizationInstallation};

/// Authorization codes that report in-flight materialization rather than denial.
///
/// The server returns these while approved authority, dependencies, or resources
/// are still being materialized. They are retriable with a bounded backoff: a
/// participant's growing authority must never fail a caller.
pub(crate) fn is_retriable_authorization_code(code: &str) -> bool {
    matches!(
        code,
        "resource_pending" | "dependency_pending" | "authorization_pending"
    )
}

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

/// Stable, connection-owned collaborators for authorization-context refresh.
///
/// One connection owns exactly this set; grouping them keeps the physical
/// refresh transaction and the background refresh task from threading the same
/// eight independent parameters and makes the owned surface explicit.
pub(crate) struct AuthorizationRefreshRuntime {
    pub(crate) contexts: Arc<AuthorizationContextCache>,
    pub(crate) auth: Arc<SessionAuth>,
    pub(crate) nats: async_nats::Client,
    pub(crate) applied_native_authorization: Arc<tokio::sync::Mutex<AppliedNativeAuthorization>>,
    pub(crate) provider: AuthorizationProviderCache,
    pub(crate) rotation: Arc<AuthorizationTransportRotation>,
    pub(crate) live: Option<Arc<crate::live::manager::LiveSessionManager>>,
    pub(crate) timeout_ms: u64,
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

/// Retry one signed bootstrap/context refresh until materialization settles.
///
/// The server reports a retriable authorization code while approved authority,
/// dependencies, or resources are still being materialized; a participant's
/// growing authority must never fail a caller. Only those codes are retried,
/// with bounded backoff, under a single overall budget equal to the caller's
/// connection timeout. Terminal authorization errors and non-retriable
/// infrastructure errors return immediately, and exhausting the budget
/// reports authorization as unavailable rather than looping forever.
pub(crate) async fn refresh_until_materialized(
    contexts: &AuthorizationContextCache,
    auth: &SessionAuth,
    timeout_ms: u64,
) -> Result<(), TrellisClientError> {
    let mut retry_delay = Duration::from_millis(100);
    tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match contexts.refresh(auth).await {
                Err(TrellisClientError::BootstrapHttp { code, .. })
                    if is_retriable_authorization_code(&code) =>
                {
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                }
                result => {
                    result?;
                    break;
                }
            }
        }
        Ok::<(), TrellisClientError>(())
    })
    .await
    .map_err(|_| {
        TrellisClientError::AuthorizationUnavailable(
            "bootstrap resource materialization exceeded the connect budget".to_owned(),
        )
    })??;
    Ok(())
}

/// Install one prepared authorization candidate as transparent maintenance.
///
/// The physical NATS attachment may rotate to admit refreshed routing
/// credentials, but the logical connection is preserved. The predecessor
/// stays application-current until the candidate is admitted, its exact
/// revocation coverage is retained on the replacement attachment, and the
/// candidate is promoted. The planned-rotation marker is held across the whole
/// transaction so a failure never leaves a logically connected attachment that
/// is not actually usable.
pub(crate) async fn install_prepared_authorization(
    runtime: &AuthorizationRefreshRuntime,
) -> Result<String, TrellisClientError> {
    let (candidate_digest, _) = runtime.contexts.prepare_refresh(&runtime.auth).await?;
    let refreshed = AppliedNativeAuthorization::from_cache(&runtime.contexts)?;
    let mut applied = runtime.applied_native_authorization.lock().await;
    let rotates = applied.rotates_to(&refreshed);
    // Planned-rotation maintenance only applies while the physical attachment is
    // healthy. During an already-real outage the refreshed credential is still
    // installed, but the reconnect must take the ordinary recovery branch so the
    // connection and its Live manager are resumed.
    let planned =
        rotates && runtime.nats.connection_state() == async_nats::connection::State::Connected;
    if planned {
        runtime.provider.begin_planned_rotation();
    }
    let mut context = NativeAuthorizationRefreshContext {
        nats: &runtime.nats,
        applied: &mut applied,
        contexts: &runtime.contexts,
        rotation: &runtime.rotation,
        live: runtime.live.as_deref(),
        timeout_ms: runtime.timeout_ms,
    };
    if let Err(error) = apply_native_authorization_refresh(&mut context, refreshed, planned).await {
        runtime.provider.abandon_rotation();
        return Err(error);
    }
    if let Err(error) = runtime
        .provider
        .retain_own_context(&candidate_digest, runtime.provider.epoch())
        .await
    {
        runtime.provider.abandon_rotation();
        runtime.rotation.cancel();
        return Err(error);
    }
    if let Err(error) = runtime
        .provider
        .finalize_own_installation(&candidate_digest, true)
    {
        runtime.provider.abandon_rotation();
        runtime.rotation.cancel();
        return Err(error);
    }
    runtime.rotation.complete();
    Ok(candidate_digest)
}

/// Background own-context refresh on the retained NATS connection.
pub(crate) fn spawn_authorization_context_refresh_task(
    runtime: AuthorizationRefreshRuntime,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let delay = match runtime.contexts.refresh_delay() {
                Ok(delay) => delay,
                Err(error) => {
                    tracing::warn!(%error, "authorization context refresh stopped");
                    return;
                }
            };
            let request = tokio::select! {
                () = tokio::time::sleep(delay) => AuthorizationRefreshRequest {
                    context_digest: runtime.contexts.stored_context_digest().ok(),
                    refresh_credential: true,
                },
                request = runtime.contexts.wait_refresh_request() => request,
            };
            if request.context_digest.is_some()
                && request.context_digest != runtime.contexts.stored_context_digest().ok()
            {
                continue;
            }
            if !request.refresh_credential && request.context_digest.is_some() {
                if let Some(digest) = request.context_digest {
                    let epoch = runtime.provider.epoch();
                    match runtime.provider.retain_own_context(&digest, epoch).await {
                        Ok(()) => {
                            let promote = runtime.contexts.candidate_digest().is_ok();
                            match runtime.provider.finalize_own_installation(&digest, promote) {
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
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                runtime.contexts.request_coverage_reconciliation();
                continue;
            }
            match install_prepared_authorization(&runtime).await {
                Ok(_) => {}
                Err(TrellisClientError::BootstrapHttp { status, code })
                    if is_terminal_refresh_error(&code) =>
                {
                    tracing::warn!(status, "authorization context refresh rejected");
                    if let Err(error) = runtime.contexts.clear() {
                        tracing::warn!(%error, "failed to clear rejected authorization context");
                    }
                    let _ = runtime.nats.drain().await;
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "authorization context refresh will retry");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    runtime.contexts.request_refresh();
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
