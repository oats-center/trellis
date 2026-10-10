use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use trellis_protocol::{
    AuthorizationContextRefreshSessionProofInput, NativeBootstrapSessionProofInput, PrincipalKind,
    SessionProofInput,
};

use super::super::connection::{validate_native_runtime_refresh, AppliedNativeAuthorization};
use super::super::{proof::new_request_id, SessionAuth, TrellisClientError};
use super::own_context::{
    system_now_millis, AuthorizationContextCache, AuthorizationRefreshOutcome,
    AuthorizationRefreshRequest, OwnTransitionGuard,
};
use super::provider_cache::AuthorizationProviderCache;
use super::registry::PinnedOwnCandidateSource;
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
    /// Connection-owned automatic transport generations, notified after every
    /// promotion so an effective policy change can open the newest generation.
    pub(crate) generations: crate::client::TransportGenerationManager,
}

/// Obtain or renew connection authority using only the owner credential and proof.
pub(crate) async fn refresh(
    cache: &AuthorizationContextCache,
    auth: &SessionAuth,
    promote: bool,
) -> Result<AuthorizationRefreshOutcome, TrellisClientError> {
    if auth.session_key != cache.session_key {
        return Err(TrellisClientError::Bootstrap(
            "refresh signing key does not belong to this connection".into(),
        ));
    }
    let observed_digest = cache.stored_context_digest().ok();
    let _refresh = cache.lock_refresh().await;
    if observed_digest != cache.stored_context_digest().ok() {
        // The installed context changed underneath the request. Surface an
        // unavailable install as an error; otherwise prepare nothing and report
        // no runtime change, so a caller that required a preparation fails closed
        // rather than adopting whatever candidate a concurrent operation left.
        cache.stored_context_digest()?;
        return Ok(AuthorizationRefreshOutcome {
            runtime_changed: false,
            prepared: None,
        });
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
        } => {
            request["identityKeyId"] = json!(identity.key_id());
            request["sessionKey"] = json!(auth.session_key);
            request["iat"] = json!(issued_at);
            request["packageEvidence"] = serde_json::to_value(package_evidence)?;
            request["participantPath"] = json!(participant_path);
            request["packageDigest"] = json!(package_evidence.root_digest());
            let input = NativeBootstrapSessionProofInput {
                origin: cache.http().origin(),
                unsigned_request: request.clone(),
            };
            match kind {
                PrincipalKind::Service => (
                    "/bootstrap/service",
                    SessionProofInput::service_bootstrap(input),
                    identity.as_ref(),
                ),
                PrincipalKind::Device => (
                    "/bootstrap/device",
                    SessionProofInput::device_bootstrap(input),
                    identity.as_ref(),
                ),
                PrincipalKind::User => {
                    return Err(TrellisClientError::Bootstrap(
                        "user login cannot use a native credential".into(),
                    ));
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
    let prepared = if promote {
        cache.install_initial(installation)?;
        None
    } else {
        Some(cache.prepare(installation)?)
    };
    Ok(AuthorizationRefreshOutcome {
        runtime_changed: previous.runtime.as_ref() != Some(&cache.runtime_binding()?),
        prepared,
    })
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

/// Promote a verified authorization candidate through one exact scoped
/// own-coverage warm.
///
/// The candidate's initial revocation coverage is established on exactly one
/// pinned attachment chosen once by
/// [`crate::client::TransportGenerationManager::prepare_own_coverage`]: an
/// already-admitted safe carrier when one exists (no extra socket), otherwise a
/// private provisional stage opened for the candidate. Promotion is then a single
/// guarded commit that validates the exact prepared instance, the exact
/// per-attempt pending pin, and the covered entry while the own-installation
/// transition is held. A staged candidate additionally runs that commit inside the
/// manager's state guard and parks the provisional generation as a draining
/// survivor, so ordinary adoption installs intake and promotes the same socket.
/// Any failure or dropped future releases only this attempt's resources.
pub(crate) async fn install_prepared_authorization(
    runtime: &AuthorizationRefreshRuntime,
) -> Result<String, TrellisClientError> {
    // A connection that has latched a terminal cause must not report a
    // successful authorization install, and must not make a refreshed context
    // current. The latch is monotonic, so checking it before the prepare step and
    // again before the promotion (which is what makes a context current) fences a
    // concurrent terminal from being reported as success.
    if let Some(cause) = runtime.generations.terminal() {
        return Err(cause.client_error());
    }
    let candidate = runtime.contexts.prepare_refresh(&runtime.auth).await?;
    let candidate_digest = candidate.context_digest.clone();
    if let Some(cause) = runtime.generations.terminal() {
        return Err(cause.client_error());
    }
    // Scope this attempt's pending candidate pin: any pre-promotion failure or
    // dropped future releases exactly this pin, never the healthy installed
    // lease or a newer candidate's pending slot.
    let mut retention = runtime
        .provider
        .begin_own_candidate_retention(&candidate_digest);
    // Prepare the single pinned coverage source, raced against the logical
    // terminal so a withheld registry admission or CONNECT can never block
    // shutdown: cancellation drops any provisional stage and releases reconcile.
    let preparation = tokio::select! {
        biased;
        cause = runtime.generations.wait_terminal() => return Err(cause.client_error()),
        preparation = runtime.generations.prepare_own_coverage(&candidate) => preparation?,
    };
    if let Some(cause) = runtime.generations.terminal() {
        return Err(cause.client_error());
    }
    let instance = preparation.instance;
    let stage = preparation.stage;
    // The refreshed runtime is read from the exact guarded candidate snapshot the
    // preparation validated against this operation's originating identity, never
    // from an unguarded candidate-first cache getter that could observe a
    // concurrently prepared signed-identical replacement.
    let refreshed = AppliedNativeAuthorization {
        runtime: preparation.runtime,
    };
    let source = PinnedOwnCandidateSource::pinned(preparation.lease, preparation.clock_offset_ms);
    // Establish the candidate's initial revocation coverage on exactly that pinned
    // source under the candidate's own corrected clock, again raced against the
    // logical terminal.
    tokio::select! {
        biased;
        cause = runtime.generations.wait_terminal() => return Err(cause.client_error()),
        result = runtime
            .provider
            .retain_own_candidate_context(&mut retention, &source) => result?,
    }
    if let Some(cause) = runtime.generations.terminal() {
        return Err(cause.client_error());
    }
    // From here on there is no await until the guarded promotion has committed
    // and the promoted authorization is applied, so cancellation can never leave
    // successfully promoted authority without its applied record or adoption
    // notification. The applied mutex is acquired first and retained across the
    // stable-session validation, the own-installation promotion and exact
    // pending-to-installed swap, and the applied-record write, which also orders
    // concurrent installs so an older one cannot overwrite a newer applied record.
    let mut applied = runtime.applied_native_authorization.lock().await;
    // Recheck the terminal latch under this final commit boundary: a terminal
    // latched while the applied mutex was contended must not be reported as a
    // successful install. The same latch is rechecked again inside the commit
    // closure, under the own-installation transition, so a terminal that lands
    // after this point (for example while the non-reentrant transition is
    // contended) still cannot promote or make a context current.
    if let Some(cause) = runtime.generations.terminal() {
        return Err(cause.client_error());
    }
    // Validate the refreshed runtime keeps the same stable session. The recorded
    // applied authorization is updated only after a successful promotion, from
    // this exact candidate, and never mutates the original baseline socket.
    validate_native_runtime_refresh(&applied.runtime, &refreshed.runtime)?;
    // The guarded commit: promotion and the exact pending-to-installed swap run
    // under the own-installation transition. A staged candidate runs them inside
    // the manager's state guard and parks the provisional generation so ordinary
    // adoption installs its intake and promotes it on the same socket. The
    // non-`Send` transition is taken and released entirely within this block so it
    // is never held across an await.
    {
        let transition = runtime.contexts.lock_own_transition()?;
        let token = retention.token().clone();
        let commit = |transition: &OwnTransitionGuard<'_>| {
            // Final terminal fence under the held own-installation transition: a
            // terminal latched after the applied-mutex recheck — notably while
            // this non-reentrant transition was contended — must not be reported
            // as a successful install or made current. The staged path additionally
            // fences the terminal inside the manager's state guard.
            if let Some(cause) = runtime.generations.terminal() {
                return Err(cause.client_error());
            }
            runtime.provider.finalize_own_installation_locked(
                transition,
                &candidate_digest,
                true,
                Some(&instance),
                Some(&token),
            )
        };
        match stage {
            Some(staged) => staged.finish(&transition, || commit(&transition))?,
            None => commit(&transition)?,
        }
    }
    *applied = refreshed;
    drop(applied);
    // Synchronous success tail: no possible async suspension can skip the
    // adoption notification or the applied-record commit.
    retention.disarm();
    // Wake the single adoption worker; it decides whether the new policy needs a
    // wider generation or is a routine renewal with no physical change.
    runtime.generations.authorization_promoted();
    Ok(candidate_digest)
}

/// Background own-context refresh on the retained NATS connection.
pub(crate) fn spawn_authorization_context_refresh_task(
    runtime: AuthorizationRefreshRuntime,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Stop once the logical connection latches an authoritative terminal
            // cause: a dead authority must not be retried every refresh interval.
            if runtime.generations.terminal().is_some() {
                tracing::info!(
                    "authorization context refresh stopped: logical connection is terminal"
                );
                return;
            }
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
                _ = runtime.generations.wait_terminal() => {
                    tracing::info!(
                        "authorization context refresh stopped: logical connection is terminal"
                    );
                    return;
                }
            };
            if request.context_digest.is_some()
                && request.context_digest != runtime.contexts.stored_context_digest().ok()
            {
                continue;
            }
            // Coverage-only requests can remain continuously pending during an
            // outage. They must not beat the scheduled credential timer on every
            // iteration and starve renewal past the installed signed window.
            let refresh_credential = request.refresh_credential
                || match runtime.contexts.credential_refresh_due() {
                    Ok(due) => due,
                    Err(error) => {
                        tracing::warn!(%error, "authorization context refresh stopped");
                        return;
                    }
                };
            if !refresh_credential && request.context_digest.is_some() {
                if let Some(digest) = request.context_digest {
                    match runtime.provider.retain_own_context(&digest).await {
                        Ok(()) => {
                            // Resume coverage for the installed context only. A
                            // private candidate is promoted solely by the exact
                            // scoped candidate installation path, never by a
                            // digest-only resume of an unrelated pending candidate.
                            match runtime.provider.finalize_own_installation(&digest, false) {
                                Ok(()) => {
                                    continue;
                                }
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
            // Bind the attempt to the installed context so a terminal result is
            // only committed for the authorization it actually describes.
            let originating_digest = runtime.contexts.stored_context_digest().ok();
            match install_prepared_authorization(&runtime).await {
                Ok(_) => {}
                Err(TrellisClientError::BootstrapHttp { status, code })
                    if is_terminal_refresh_error(&code) =>
                {
                    tracing::warn!(status, "authorization context refresh rejected");
                    // Positive stale-result guard: latch a terminal cause only
                    // when the refused result positively describes the still
                    // installed originating context. An absent or superseded
                    // context is not this logical connection's terminal cause,
                    // and the conditional clear never discards a newer install.
                    let Some(origin) = originating_digest.as_deref() else {
                        continue;
                    };
                    let terminal = crate::client::LogicalTerminalCause::Authorization(code.clone());
                    // The conditional clear and the terminal latch are published
                    // while the own-transition guard is held, so a concurrent
                    // public refresh cannot install a valid new context in the
                    // window between them (it takes the same guard).
                    match runtime.contexts.clear_if_installed(origin, || {
                        runtime.generations.publish_terminal(terminal.clone());
                    }) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(error) => {
                            tracing::warn!(%error, "failed to clear rejected authorization context");
                            continue;
                        }
                    }
                    let _ = runtime.nats.drain().await;
                    return;
                }
                Err(error) => {
                    // A terminal latched during the attempt (for example the
                    // promotion fence returning `AuthorizationUnavailable`) must
                    // end the loop now, not retry on the refresh interval.
                    if runtime.generations.terminal().is_some() {
                        tracing::info!(
                            "authorization context refresh stopped: logical connection is terminal"
                        );
                        return;
                    }
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
