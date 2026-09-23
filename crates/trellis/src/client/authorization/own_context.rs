use std::sync::{Arc, Mutex, RwLock};

use trellis_protocol::{verify_authorization_context, AuthorizationContextPurpose};

use super::super::{SessionAuth, TrellisClientError};
use super::bootstrap_http::{persisted_signed_context, BootstrapHttp};
use super::types::{
    AuthorizationContextBundle, AuthorizationCredential, AuthorizationInstallation,
    AuthorizationRuntimeBinding, CachedAuthorizationState, CurrentContext,
};

#[derive(Clone, Debug)]
pub(crate) struct AuthorizationRefreshRequest {
    pub(crate) context_digest: Option<String>,
    pub(crate) refresh_credential: bool,
}

#[derive(Clone, Debug)]
struct PreparedInstallation {
    current: CurrentContext,
    runtime: AuthorizationRuntimeBinding,
    routing: super::types::AuthorizationRoutingMaterial,
    api_bindings: std::collections::BTreeMap<String, super::types::AuthorizationApiBinding>,
    server_clock_offset_ms: i64,
    authorization: Option<serde_json::Value>,
    availability: crate::generated::AvailabilitySnapshot,
}

/// Process-local context, route credential, and refresh scheduling for one connection.
/// Native credentials never require a writable authorization-state directory.
#[derive(Clone)]
pub struct AuthorizationContextCache {
    http: BootstrapHttp,
    pub(crate) credential: Arc<AuthorizationCredential>,
    pub(crate) connection_id: String,
    pub(crate) participant_id: String,
    pub(crate) name: Option<String>,
    pub(crate) session_key: String,
    state: Arc<RwLock<CachedAuthorizationState>>,
    availability: tokio::sync::watch::Sender<crate::generated::AvailabilitySnapshot>,
    refresh: Arc<tokio::sync::Mutex<()>>,
    refresh_requested: Arc<tokio::sync::Notify>,
    refresh_request: Arc<Mutex<Option<AuthorizationRefreshRequest>>>,
    candidate: Arc<RwLock<Option<PreparedInstallation>>>,
    revoked_digest: Arc<Mutex<Option<String>>>,
    transition: Arc<Mutex<()>>,
}

/// One short synchronization boundary for final own-installation transitions.
///
/// The guard is private to the SDK and shared by this cache's clones: it
/// orders installation, promotion, resumption, invalidation, and suspension so
/// local publication can never overwrite consumed invalidation evidence.
pub(crate) struct OwnTransitionGuard<'a> {
    _guard: std::sync::MutexGuard<'a, ()>,
}

impl AuthorizationContextCache {
    pub(crate) fn new_with_insecure_origin(
        trellis_url: &str,
        participant_id: String,
        connection_id: String,
        session_key: String,
        credential: AuthorizationCredential,
        name: Option<String>,
        allow_insecure_origin: bool,
    ) -> Result<Self, TrellisClientError> {
        let (availability, _) = tokio::sync::watch::channel(Default::default());
        Ok(Self {
            http: BootstrapHttp::new(trellis_url, allow_insecure_origin)?,
            credential: Arc::new(credential),
            connection_id,
            participant_id,
            name,
            session_key,
            state: Arc::new(RwLock::new(CachedAuthorizationState::default())),
            availability,
            refresh: Arc::new(tokio::sync::Mutex::new(())),
            refresh_requested: Arc::new(tokio::sync::Notify::new()),
            refresh_request: Arc::new(Mutex::new(None)),
            candidate: Arc::new(RwLock::new(None)),
            revoked_digest: Arc::new(Mutex::new(None)),
            transition: Arc::new(Mutex::new(())),
        })
    }

    /// Acquire the shared own-installation transition boundary.
    pub(crate) fn lock_own_transition(&self) -> Result<OwnTransitionGuard<'_>, TrellisClientError> {
        Ok(OwnTransitionGuard {
            _guard: self.transition.lock().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable("own transition lock poisoned".into())
            })?,
        })
    }

    fn replace_installation(
        &self,
        installation: AuthorizationInstallation,
        promote: bool,
    ) -> Result<(), TrellisClientError> {
        let transition = self.lock_own_transition()?;
        self.replace_installation_locked(&transition, installation, promote)
    }

    fn replace_installation_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
        installation: AuthorizationInstallation,
        promote: bool,
    ) -> Result<(), TrellisClientError> {
        let AuthorizationInstallation {
            context: bundle,
            routing,
            runtime,
            api_bindings,
            server_clock_offset_ms,
            authorization,
        } = installation;
        let now = system_now_millis()?
            .checked_add(server_clock_offset_ms)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
            .div_euclid(1000);
        let signed = persisted_signed_context(&bundle)?;
        let policy = bundle
            .policy
            .verification_policy(now)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let verified = verify_authorization_context(
            &bundle.issuer,
            &signed,
            &policy,
            AuthorizationContextPurpose::Live,
        )
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let context = &signed.unsigned;
        let credential_matches = match self.credential.as_ref() {
            AuthorizationCredential::Native { kind, identity, .. } => {
                context.principal_kind == *kind
                    && context.identity_key_id.as_deref() == Some(identity.key_id().as_str())
                    && context.login_session_id.is_none()
            }
            AuthorizationCredential::User {
                login_session_id, ..
            } => {
                context.principal_kind == trellis_protocol::AuthorizationPrincipalKind::User
                    && context.login_session_id.as_deref() == Some(login_session_id.as_str())
                    && context.identity_key_id.is_none()
            }
        };
        if !credential_matches
            || context.connection_id != self.connection_id
            || context.session_key != self.session_key
            || context.participant_id != self.participant_id
            || runtime.connection_id != context.connection_id
            || runtime.login_session_id != context.login_session_id
            || runtime.participant_id != context.participant_id
            || runtime.inbox_prefix != context.inbox_prefix
        {
            return Err(TrellisClientError::Bootstrap("bootstrap assignment does not match the credential, participant, and runtime connection".into()));
        }
        let native = runtime.transports.native.as_ref().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "bootstrap did not offer a native NATS transport".into(),
            )
        })?;
        if native.nats_servers.is_empty()
            || routing.bootstrap_jwt.is_empty()
            || routing.bootstrap_jwt_expires_at <= now
        {
            return Err(TrellisClientError::Bootstrap(
                "bootstrap transport or route credential is unavailable".into(),
            ));
        }
        for endpoint in &native.nats_servers {
            endpoint
                .parse::<async_nats::ServerAddr>()
                .map_err(|error| {
                    TrellisClientError::Bootstrap(format!("invalid NATS endpoint: {error}"))
                })?;
        }
        let refresh_at = trellis_protocol::authorization_context_refresh_at(
            verified.context_digest(),
            context.issued_at,
            context.not_before,
            context.expires_at,
            bundle.policy.refresh_lead_seconds,
            bundle.policy.refresh_jitter_seconds,
        )
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let resources = authorization
            .as_ref()
            .and_then(|value| value.get("resourceRuntime"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let availability = crate::generated::AvailabilitySnapshot::replacing(
            context.grants.permissions().to_vec(),
            resources,
            &self.availability.borrow(),
        );
        let current = CurrentContext {
            context_digest: verified.context_digest().to_owned(),
            not_before: context.not_before,
            expires_at: context.expires_at,
            refresh_at,
            bundle,
        };
        if !promote {
            // A planned refresh keeps the active installation usable until the
            // candidate is promoted after transport reauthorization.
            let mut candidate = self.candidate.write().map_err(|_| {
                TrellisClientError::Bootstrap("context candidate lock poisoned".into())
            })?;
            *candidate = Some(PreparedInstallation {
                current,
                runtime,
                routing,
                api_bindings,
                server_clock_offset_ms,
                authorization,
                availability,
            });
            tracing::info!(
                context_digest = verified.context_digest(),
                "prepared verified authorization candidate"
            );
            return Ok(());
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?;
        *state = CachedAuthorizationState {
            current: Some(current),
            runtime: Some(runtime),
            routing: Some(routing),
            api_bindings,
            server_clock_offset_ms,
            authorization,
        };
        drop(state);
        *self.candidate.write().map_err(|_| {
            TrellisClientError::Bootstrap("context candidate lock poisoned".into())
        })? = None;
        self.availability.send_replace(availability);
        tracing::info!(
            context_digest = verified.context_digest(),
            promoted = true,
            "installed verified authorization context"
        );
        Ok(())
    }

    pub(crate) fn install_initial(
        &self,
        installation: AuthorizationInstallation,
    ) -> Result<(), TrellisClientError> {
        self.replace_installation(installation, true)
    }

    pub(crate) fn prepare(
        &self,
        installation: AuthorizationInstallation,
    ) -> Result<String, TrellisClientError> {
        self.replace_installation(installation, false)?;
        self.candidate_digest()
    }

    pub(crate) fn candidate_digest(&self) -> Result<String, TrellisClientError> {
        let transition = self.lock_own_transition()?;
        self.candidate_digest_locked(&transition)
    }

    pub(crate) fn candidate_digest_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
    ) -> Result<String, TrellisClientError> {
        self.candidate
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context candidate lock poisoned".into()))?
            .as_ref()
            .map(|candidate| candidate.current.context_digest.clone())
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate is unavailable".into(),
                )
            })
    }

    /// Return the runtime, digest, and route JWT that the next transport
    /// reauthorization must present: the candidate when one is prepared.
    pub(crate) fn applied_transport(
        &self,
    ) -> Result<(AuthorizationRuntimeBinding, String, String), TrellisClientError> {
        if let Some(candidate) = self
            .candidate
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context candidate lock poisoned".into()))?
            .as_ref()
        {
            return Ok((
                candidate.runtime.clone(),
                candidate.current.context_digest.clone(),
                candidate.routing.bootstrap_jwt.clone(),
            ));
        }
        let state = self.state_snapshot()?;
        Ok((
            state.runtime.ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization runtime unavailable".into())
            })?,
            state
                .current
                .ok_or_else(|| {
                    TrellisClientError::Bootstrap("authorization context unavailable".into())
                })?
                .context_digest,
            state
                .routing
                .ok_or_else(|| {
                    TrellisClientError::Bootstrap("authorization routing JWT unavailable".into())
                })?
                .bootstrap_jwt,
        ))
    }

    pub(crate) fn promote_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
        expected_digest: &str,
    ) -> Result<(), TrellisClientError> {
        let now = {
            let candidate = self.candidate.read().map_err(|_| {
                TrellisClientError::Bootstrap("context candidate lock poisoned".into())
            })?;
            let prepared = candidate.as_ref().ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate is unavailable".into(),
                )
            })?;
            if prepared.current.context_digest != expected_digest {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate changed before promotion".into(),
                ));
            }
            system_now_millis()?
                .checked_add(prepared.server_clock_offset_ms)
                .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
                .div_euclid(1000)
        };
        let prepared = {
            let mut candidate = self.candidate.write().map_err(|_| {
                TrellisClientError::Bootstrap("context candidate lock poisoned".into())
            })?;
            let prepared = candidate.take().ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate is unavailable".into(),
                )
            })?;
            if prepared.current.context_digest != expected_digest {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate changed before promotion".into(),
                ));
            }
            prepared
        };
        if prepared.current.not_before > now || prepared.current.expires_at <= now {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate is no longer current".into(),
            ));
        }
        let PreparedInstallation {
            current,
            runtime,
            routing,
            api_bindings,
            server_clock_offset_ms,
            authorization,
            availability,
        } = prepared;
        let mut state = self
            .state
            .write()
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?;
        if state.current.is_none() {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context was cleared before promotion".into(),
            ));
        }
        *state = CachedAuthorizationState {
            current: Some(current),
            runtime: Some(runtime),
            routing: Some(routing),
            api_bindings,
            server_clock_offset_ms,
            authorization,
        };
        drop(state);
        self.availability.send_replace(availability);
        tracing::info!(
            context_digest = self.retained_context_digest().ok(),
            "promoted authorization installation"
        );
        Ok(())
    }

    /// Discard this connection's context and route without revoking its credential.
    pub fn clear(&self) -> Result<(), TrellisClientError> {
        let transition = self.lock_own_transition()?;
        self.clear_locked(&transition)
    }

    fn clear_locked(&self, _transition: &OwnTransitionGuard<'_>) -> Result<(), TrellisClientError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?;
        state.current = None;
        state.routing = None;
        state.api_bindings.clear();
        drop(state);
        *self.candidate.write().map_err(|_| {
            TrellisClientError::Bootstrap("context candidate lock poisoned".into())
        })? = None;
        let suspended =
            crate::generated::AvailabilitySnapshot::suspended(&self.availability.borrow().clone());
        self.availability.send_replace(suspended);
        Ok(())
    }

    pub(crate) fn suspend(&self) {
        if let Ok(transition) = self.lock_own_transition() {
            self.suspend_locked(&transition);
        }
    }

    /// Withdraw application usability while the transition gate is held.
    pub(crate) fn suspend_locked(&self, _transition: &OwnTransitionGuard<'_>) {
        let suspended =
            crate::generated::AvailabilitySnapshot::suspended(&self.availability.borrow().clone());
        self.availability.send_replace(suspended);
        tracing::info!(
            context_digest = self.context_digest().ok(),
            "suspended authorization installation"
        );
    }

    /// Return the digest of the currently valid context used for request proofs.
    pub fn context_digest(&self) -> Result<String, TrellisClientError> {
        if !self.availability.borrow().is_usable() {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization installation is suspended".into(),
            ));
        }
        self.retained_context_digest()
    }

    /// Return the installed context identity without time or usability checks.
    ///
    /// Refresh ownership and coverage reconciliation must still recognize an
    /// expired-but-retained predecessor; time checks belong on use paths.
    pub(crate) fn stored_context_digest(&self) -> Result<String, TrellisClientError> {
        let state = self
            .state
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?;
        state
            .current
            .as_ref()
            .map(|current| current.context_digest.clone())
            .ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization context is not installed".into())
            })
    }

    pub(crate) fn retained_context_digest(&self) -> Result<String, TrellisClientError> {
        let state = self
            .state
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?;
        let now = system_now_millis()?
            .checked_add(state.server_clock_offset_ms)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
            .div_euclid(1000);
        state
            .current
            .as_ref()
            .filter(|current| current.not_before <= now && current.expires_at > now)
            .map(|current| current.context_digest.clone())
            .ok_or_else(|| TrellisClientError::Bootstrap("authorization context expired".into()))
    }

    /// Publish usability again for the retained installation without changing
    /// context or resource identity, after coverage was reinitialized.
    pub(crate) fn resume_availability_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
        expected_digest: &str,
    ) -> Result<(), TrellisClientError> {
        let state = self.state_snapshot()?;
        let current = state.current.as_ref().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "authorization context is unavailable".into(),
            )
        })?;
        if current.context_digest != expected_digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization installation changed before resumption".into(),
            ));
        }
        let now = system_now_millis()?
            .checked_add(state.server_clock_offset_ms)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
            .div_euclid(1000);
        if current.not_before > now || current.expires_at <= now {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context is no longer current".into(),
            ));
        }
        let permissions = persisted_signed_context(&current.bundle)?
            .unsigned
            .grants
            .permissions()
            .to_vec();
        let resources = state
            .authorization
            .as_ref()
            .and_then(|value| value.get("resourceRuntime"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let previous = self.availability.borrow().clone();
        self.availability
            .send_replace(crate::generated::AvailabilitySnapshot::replacing(
                permissions,
                resources,
                &previous,
            ));
        tracing::info!(
            context_digest = expected_digest,
            "resumed authorization installation coverage"
        );
        Ok(())
    }

    /// Record locally observed revocation evidence for transport presentation.
    pub(crate) fn mark_revoked(&self, digest: &str) {
        if let Ok(mut revoked) = self.revoked_digest.lock() {
            if revoked.as_deref() != Some(digest) {
                *revoked = Some(digest.to_owned());
            }
        }
    }

    fn is_revoked(&self, digest: &str) -> bool {
        self.revoked_digest
            .lock()
            .is_ok_and(|revoked| revoked.as_deref() == Some(digest))
    }

    /// Return the locally observed revocation evidence digest, if any.
    pub(crate) fn revocation_marker(&self) -> Option<String> {
        self.revoked_digest
            .lock()
            .ok()
            .and_then(|revoked| revoked.clone())
    }

    /// Discard a matching private candidate while the transition gate is held.
    pub(crate) fn invalidate_candidate_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
        digest: &str,
    ) -> bool {
        if let Ok(mut candidate) = self.candidate.write() {
            if candidate
                .as_ref()
                .is_some_and(|candidate| candidate.current.context_digest == digest)
            {
                *candidate = None;
                return true;
            }
        }
        false
    }

    /// Return the current verified context and its online issuer entry.
    pub fn bundle(&self) -> Result<AuthorizationContextBundle, TrellisClientError> {
        self.state_snapshot()?
            .current
            .map(|current| current.bundle)
            .ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization context unavailable".into())
            })
    }

    /// Renew through the credential's proof-bound native bootstrap or user refresh route.
    pub async fn refresh(&self, auth: &SessionAuth) -> Result<bool, TrellisClientError> {
        super::refresh::refresh(self, auth, true)
            .await
            .map(|(_, changed)| changed)
    }

    pub(crate) async fn prepare_refresh(
        &self,
        auth: &SessionAuth,
    ) -> Result<(String, bool), TrellisClientError> {
        super::refresh::refresh(self, auth, false).await
    }

    pub(crate) async fn lock_refresh(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.refresh.lock().await
    }

    pub(crate) fn request_refresh(&self) {
        self.request_reconciliation(true);
    }

    pub(crate) fn request_coverage_reconciliation(&self) {
        self.request_reconciliation(false);
    }

    fn request_reconciliation(&self, refresh_credential: bool) {
        if let Ok(mut requested) = self.refresh_request.lock() {
            let digest = self.stored_context_digest().ok();
            match requested.as_mut() {
                Some(request) => request.refresh_credential |= refresh_credential,
                None => {
                    *requested = Some(AuthorizationRefreshRequest {
                        context_digest: digest,
                        refresh_credential,
                    });
                }
            }
            self.refresh_requested.notify_one();
        }
    }

    pub(crate) async fn wait_refresh_request(&self) -> AuthorizationRefreshRequest {
        self.refresh_requested.notified().await;
        self.refresh_request
            .lock()
            .ok()
            .and_then(|mut requested| requested.take())
            .unwrap_or(AuthorizationRefreshRequest {
                context_digest: self.stored_context_digest().ok(),
                refresh_credential: false,
            })
    }

    pub(crate) fn refresh_delay(&self) -> Result<std::time::Duration, TrellisClientError> {
        let now = self.corrected_now_seconds()?;
        let state = self.state_snapshot()?;
        let Some(current) = state.current.as_ref() else {
            return Ok(std::time::Duration::from_secs(1));
        };
        let route_refresh = state.routing.as_ref().map_or(now, |route| {
            route
                .bootstrap_jwt_expires_at
                .saturating_sub(i64::from(current.bundle.policy.refresh_lead_seconds))
        });
        Ok(std::time::Duration::from_secs(
            u64::try_from(
                current
                    .refresh_at
                    .min(route_refresh)
                    .saturating_sub(now)
                    .max(5),
            )
            .map_err(|_| TrellisClientError::Bootstrap("context refresh delay overflow".into()))?,
        ))
    }

    pub(crate) fn transport_credentials(&self) -> Result<(String, String), TrellisClientError> {
        let candidate = {
            let candidate = self.candidate.read().map_err(|_| {
                TrellisClientError::Bootstrap("context candidate lock poisoned".into())
            })?;
            candidate.as_ref().map(|candidate| {
                (
                    candidate.routing.bootstrap_jwt.clone(),
                    candidate.routing.bootstrap_jwt_expires_at,
                    candidate.current.context_digest.clone(),
                    candidate.current.not_before,
                    candidate.current.expires_at,
                    candidate.server_clock_offset_ms,
                )
            })
        };
        if let Some((jwt, jwt_expires_at, digest, not_before, expires_at, offset)) = candidate {
            let now = system_now_millis()?
                .checked_add(offset)
                .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
                .div_euclid(1000);
            if jwt_expires_at <= now || not_before > now || expires_at <= now {
                return Err(TrellisClientError::Bootstrap(
                    "authorization candidate credential expired".into(),
                ));
            }
            if self.is_revoked(&digest) {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate is revoked".into(),
                ));
            }
            return Ok((jwt, digest));
        }
        let (routing_jwt, routing_expires_at, digest, not_before, expires_at, offset) = {
            let state = self
                .state
                .read()
                .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?;
            let current = state.current.as_ref().ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization context is not installed".into())
            })?;
            let routing = state.routing.as_ref().ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization routing JWT unavailable".into())
            })?;
            (
                routing.bootstrap_jwt.clone(),
                routing.bootstrap_jwt_expires_at,
                current.context_digest.clone(),
                current.not_before,
                current.expires_at,
                state.server_clock_offset_ms,
            )
        };
        let now = system_now_millis()?
            .checked_add(offset)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
            .div_euclid(1000);
        if routing_expires_at <= now {
            return Err(TrellisClientError::Bootstrap(
                "authorization routing JWT expired".into(),
            ));
        }
        if not_before > now || expires_at <= now {
            return Err(TrellisClientError::Bootstrap(
                "authorization context expired".into(),
            ));
        }
        if self.is_revoked(&digest) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context is revoked".into(),
            ));
        }
        Ok((routing_jwt, digest))
    }

    pub(crate) fn runtime_binding(
        &self,
    ) -> Result<AuthorizationRuntimeBinding, TrellisClientError> {
        self.state_snapshot()?.runtime.ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization runtime unavailable".into())
        })
    }

    /// Return this connection owner's pinned identity tuple.
    ///
    /// # Errors
    ///
    /// Returns an error when no installed context exists.
    pub(crate) fn pinned_identity(
        &self,
    ) -> Result<crate::live::authority::PinnedPeerIdentity, TrellisClientError> {
        let state = self.state_snapshot()?;
        let current = state.current.ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "no installed authorization context".to_owned(),
            )
        })?;
        let signed = trellis_protocol::parse_authorization_context(&current.bundle.context)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        Ok(crate::live::authority::PinnedPeerIdentity::from_signed(
            &signed,
        ))
    }

    pub(crate) fn provider_deployment_id(
        &self,
        api_id: &str,
    ) -> Result<String, TrellisClientError> {
        self.state_snapshot()?
            .api_bindings
            .get(api_id)
            .map(|binding| binding.provider_deployment_id.clone())
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(format!(
                    "bootstrap did not bind API '{api_id}' to a provider deployment"
                ))
            })
    }

    pub(crate) fn corrected_now_seconds(&self) -> Result<i64, TrellisClientError> {
        self.corrected_now_millis().map(|now| now.div_euclid(1000))
    }

    pub(crate) fn corrected_now_millis(&self) -> Result<i64, TrellisClientError> {
        let offset = self
            .state
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?
            .server_clock_offset_ms;
        system_now_millis()?
            .checked_add(offset)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))
    }

    pub(crate) fn state_snapshot(&self) -> Result<CachedAuthorizationState, TrellisClientError> {
        self.state
            .read()
            .map(|state| state.clone())
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))
    }

    pub(crate) fn availability(&self) -> crate::generated::AvailabilitySnapshot {
        self.availability.borrow().clone()
    }

    pub(crate) fn watch_availability(
        &self,
    ) -> tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot> {
        self.availability.subscribe()
    }

    pub(crate) fn http(&self) -> &BootstrapHttp {
        &self.http
    }
}

pub(super) fn system_now_millis() -> Result<i64, TrellisClientError> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?
            .as_millis(),
    )
    .map_err(|_| TrellisClientError::Bootstrap("context time overflow".into()))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use ed25519_dalek::SigningKey;
    use trellis_protocol::{
        sign_authorization_context, AuthorizationIssuerKey, AuthorizationIssuerState,
        AuthorizationPrincipalKind, GrantOwnerKind, GrantSet, UnsignedAuthorizationContext,
        AUTHORIZATION_CONTEXT_FORMAT_V1,
    };

    use super::*;
    use crate::client::authorization::types::{
        AuthorizationContextBundle, AuthorizationContextPolicy, AuthorizationNativeTransport,
        AuthorizationRegistryBinding, AuthorizationRoutingMaterial, AuthorizationRuntimeBinding,
        AuthorizationRuntimeTransports,
    };
    use crate::client::proof::{base64url_encode, sha256};

    pub(crate) mod test_support {
        use trellis_protocol::{parse_authorization_context, SignedAuthorizationContext};

        use super::*;

        /// Server-corrected seconds used by the real signed test contexts.
        pub(crate) fn now_seconds() -> i64 {
            system_now_millis().unwrap().div_euclid(1_000)
        }

        fn issuer_key_id(issuer: &SigningKey) -> String {
            base64url_encode(&sha256(issuer.verifying_key().as_bytes()))
        }

        pub(crate) fn installation(
            issuer: &SigningKey,
            session: &SessionAuth,
            connection_id: &str,
            grant_revision: u64,
            now: i64,
        ) -> AuthorizationInstallation {
            let signed = sign_authorization_context(
                UnsignedAuthorizationContext {
                    format: AUTHORIZATION_CONTEXT_FORMAT_V1.to_owned(),
                    issuer_key_id: issuer_key_id(issuer),
                    principal_id: "usr_test".to_owned(),
                    principal_kind: AuthorizationPrincipalKind::User,
                    participant_id: "test.Caller".to_owned(),
                    owner_kind: GrantOwnerKind::User,
                    owner_id: "usr_test".to_owned(),
                    grant_revision,
                    identity_key_id: None,
                    login_session_id: Some("login-test".to_owned()),
                    connection_id: connection_id.to_owned(),
                    session_key: session.session_key.clone(),
                    deployment_id: None,
                    instance_id: None,
                    inbox_prefix: "_INBOX.test".to_owned(),
                    issued_at: now - 60,
                    not_before: now - 60,
                    expires_at: now + 3_600,
                    grants: GrantSet::new(vec![]),
                    platform_privileges: vec![],
                    extensions: Default::default(),
                    critical: vec![],
                },
                issuer,
            )
            .unwrap();
            let bundle = AuthorizationContextBundle {
                context: serde_json::to_value(signed).unwrap(),
                issuer: AuthorizationIssuerKey {
                    key_id: issuer_key_id(issuer),
                    public_key: base64url_encode(issuer.verifying_key().as_bytes()),
                    state: AuthorizationIssuerState::Active,
                },
                authorization_registry: AuthorizationRegistryBinding {
                    context_bucket: "test-contexts".to_owned(),
                },
                policy: AuthorizationContextPolicy {
                    allowed_clock_skew_seconds: 30,
                    maximum_context_lifetime_seconds: 86_400,
                    maximum_context_bytes: 1_048_576,
                    maximum_permissions: 1_024,
                    refresh_lead_seconds: 60,
                    refresh_jitter_seconds: 5,
                },
            };
            AuthorizationInstallation {
                context: bundle,
                routing: AuthorizationRoutingMaterial {
                    bootstrap_jwt: "route-jwt".to_owned(),
                    bootstrap_jwt_expires_at: now + 3_600,
                },
                runtime: AuthorizationRuntimeBinding {
                    connection_id: connection_id.to_owned(),
                    login_session_id: Some("login-test".to_owned()),
                    participant_id: "test.Caller".to_owned(),
                    inbox_prefix: "_INBOX.test".to_owned(),
                    transports: AuthorizationRuntimeTransports {
                        native: Some(AuthorizationNativeTransport {
                            nats_servers: vec!["nats://127.0.0.1:4222".to_owned()],
                        }),
                        websocket: None,
                    },
                },
                api_bindings: BTreeMap::new(),
                server_clock_offset_ms: 0,
                authorization: None,
            }
        }

        /// Parse the installed signed context for provider-entry fixtures.
        pub(crate) fn signed_context(
            installation: &AuthorizationInstallation,
        ) -> SignedAuthorizationContext {
            parse_authorization_context(&installation.context.context).unwrap()
        }

        /// A cache with one installed real signed context.
        pub(crate) struct OwnContextFixture {
            pub(crate) cache: AuthorizationContextCache,
            pub(crate) issuer: SigningKey,
            pub(crate) session: SessionAuth,
            pub(crate) connection_id: String,
            pub(crate) digest: String,
            pub(crate) signed: SignedAuthorizationContext,
            pub(crate) issuer_key: AuthorizationIssuerKey,
        }

        pub(crate) fn own_context_fixture(grant_revision: u64) -> OwnContextFixture {
            let now = now_seconds();
            let issuer = SigningKey::from_bytes(&[7; 32]);
            let seed = base64url_encode(&[9; 32]);
            let session = SessionAuth::from_seed_base64url(&seed).unwrap();
            let connection_id = "01JY0000000000000000000003".to_owned();
            let cache = AuthorizationContextCache::new_with_insecure_origin(
                "http://127.0.0.1:1/",
                "test.Caller".to_owned(),
                connection_id.clone(),
                session.session_key.clone(),
                AuthorizationCredential::User {
                    login_session_id: "login-test".to_owned(),
                    installation: Arc::new(SessionAuth::from_seed_base64url(&seed).unwrap()),
                },
                None,
                false,
            )
            .unwrap();
            let installation = installation(&issuer, &session, &connection_id, grant_revision, now);
            let signed = signed_context(&installation);
            let issuer_key = installation.context.issuer.clone();
            cache.install_initial(installation).unwrap();
            let digest = cache.retained_context_digest().unwrap();
            OwnContextFixture {
                cache,
                issuer,
                session,
                connection_id,
                digest,
                signed,
                issuer_key,
            }
        }
    }

    use self::test_support::{installation, own_context_fixture};

    #[test]
    fn candidate_promotion_completes_without_recursive_state_lock() {
        let fixture = own_context_fixture(1);
        let cache = fixture.cache.clone();
        let installed = fixture.digest.clone();
        assert_eq!(cache.context_digest().unwrap(), installed);

        let candidate = cache
            .prepare(installation(
                &fixture.issuer,
                &fixture.session,
                &fixture.connection_id,
                2,
                test_support::now_seconds(),
            ))
            .unwrap();
        assert_ne!(candidate, installed);
        assert_eq!(
            cache.retained_context_digest().unwrap(),
            installed,
            "a prepared candidate stays private"
        );

        // Run promotion under a watchdog: a recursive state lock would block a
        // synchronous guard and never complete this call.
        let cache_for_thread = cache.clone();
        let expected = candidate.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let transition = cache_for_thread
                .lock_own_transition()
                .map_err(|error| error.to_string())?;
            let result = cache_for_thread
                .promote_locked(&transition, &expected)
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
            Ok::<(), String>(())
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("candidate promotion failed: {error}"),
            Err(_) => panic!("candidate promotion did not complete"),
        }
        let _ = thread.join();
        assert_eq!(cache.retained_context_digest().unwrap(), candidate);
        assert_eq!(cache.context_digest().unwrap(), candidate);
        assert!(cache.candidate_digest().is_err());
    }
}
