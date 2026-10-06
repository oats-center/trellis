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
    /// Stable identity of this exact prepared candidate instance.
    ///
    /// Two independently prepared candidates can be signed-identical (same
    /// context digest) yet carry different route/clock companion material. This
    /// handle is allocated fresh per preparation, survives clones of the prepared
    /// installation, and is compared by pointer identity, so a private stage can
    /// prove the instance it opened against is still the installed candidate. It
    /// is deliberately distinct from the provider cache's per-attempt pending pin.
    instance: Arc<()>,
}

/// Exact identity of one freshly prepared private own candidate.
///
/// Returned by [`AuthorizationContextCache::prepare`] so the installer threads
/// the *originating* instance through coverage preparation and promotion. Two
/// independently prepared candidates can be signed-identical (same context
/// digest) yet carry different companion route/clock material; the instance
/// pointer, not the digest, is what fences a replaced candidate.
#[derive(Clone, Debug)]
pub(crate) struct PreparedOwnCandidate {
    /// Signed context digest this candidate was prepared for.
    pub(crate) context_digest: String,
    /// Exact prepared instance identity, compared by pointer.
    pub(crate) instance: Arc<()>,
}

/// Outcome of one internal authorization-context refresh request.
#[derive(Clone, Debug)]
pub(crate) struct AuthorizationRefreshOutcome {
    /// Whether the effective runtime binding changed across the request.
    pub(crate) runtime_changed: bool,
    /// The exact private candidate this request prepared, when it prepared one.
    ///
    /// `None` for an in-place install (a promotion) and when the installed
    /// context changed underneath the request so nothing was prepared. A caller
    /// that required a preparation must fail closed rather than adopt whatever
    /// candidate happens to occupy the slot later.
    pub(crate) prepared: Option<PreparedOwnCandidate>,
}

/// One exact freshly prepared own candidate read as an immutable transport
/// snapshot for own-coverage initialization.
///
/// Only the private prepared candidate is exposed: never the promoted
/// installation, and never a retained predecessor policy that may already be
/// revoked. The candidate's signed policy and own corrected clock decide whether
/// an already-admitted generation may carry the candidate's initial coverage.
#[derive(Clone, Debug)]
pub(crate) struct OwnCandidateTransportSnapshot {
    pub(crate) context_digest: String,
    pub(crate) policy: trellis_protocol::TransportAuthorizationV1,
    pub(crate) runtime: AuthorizationRuntimeBinding,
    pub(crate) routing_jwt: String,
    pub(crate) not_before: i64,
    pub(crate) expires_at: i64,
    pub(crate) corrected_now_seconds: i64,
    /// Immutable server-clock offset captured from the exact prepared instance.
    ///
    /// The warm path threads this offset rather than a captured "now", so
    /// signature and window verification recompute a fresh corrected time at each
    /// use and a timestamp never freezes across an await.
    pub(crate) server_clock_offset_ms: i64,
    /// Captured CONNECT route-credential expiry (Unix seconds) for the new-socket
    /// admission path. Reuse of an already-admitted carrier never revalidates it.
    pub(crate) routing_jwt_expires_at: i64,
    /// Exact prepared instance this snapshot was read from, for pointer identity.
    pub(crate) instance: Arc<()>,
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
    pub(crate) fn new(
        trellis_url: &str,
        participant_id: String,
        connection_id: String,
        session_key: String,
        credential: AuthorizationCredential,
        name: Option<String>,
    ) -> Result<Self, TrellisClientError> {
        let (availability, _) = tokio::sync::watch::channel(Default::default());
        Ok(Self {
            http: BootstrapHttp::new(trellis_url)?,
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
            signed: Arc::new(signed.clone()),
            context_digest: verified.context_digest().to_owned(),
            not_before: context.not_before,
            expires_at: context.expires_at,
            refresh_at,
            bundle,
        };
        if !promote {
            // A prepared refresh keeps the active installation usable until the
            // candidate is promoted in place on the current attachment.
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
                instance: Arc::new(()),
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

    /// Prepare a private candidate and return its exact identity.
    ///
    /// The replace and the identity read share one own-transition guard, so
    /// there is no unlock gap in which an independently prepared, signed-identical
    /// replacement could become the current candidate: the returned instance
    /// belongs to the candidate this call actually prepared.
    pub(crate) fn prepare(
        &self,
        installation: AuthorizationInstallation,
    ) -> Result<PreparedOwnCandidate, TrellisClientError> {
        let transition = self.lock_own_transition()?;
        self.replace_installation_locked(&transition, installation, false)?;
        let candidate = self
            .candidate
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context candidate lock poisoned".into()))?;
        let prepared = candidate.as_ref().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "authorization candidate is unavailable".into(),
            )
        })?;
        Ok(PreparedOwnCandidate {
            context_digest: prepared.current.context_digest.clone(),
            instance: prepared.instance.clone(),
        })
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

    /// Promote the exact prepared candidate instance while the caller holds the
    /// own-installation transition.
    ///
    /// `expected_instance` is the pointer identity of the prepared instance the
    /// caller warmed coverage against. An independently prepared signed-identical
    /// replacement shares the digest but carries fresh companion route/clock
    /// material, so digest equality alone must never substitute it: promotion
    /// fails closed unless the exact instance still matches.
    pub(crate) fn promote_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
        expected_digest: &str,
        expected_instance: &Arc<()>,
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
            if !Arc::ptr_eq(&prepared.instance, expected_instance) {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate instance changed before promotion".into(),
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
            if !Arc::ptr_eq(&prepared.instance, expected_instance) {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate instance changed before promotion".into(),
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
            instance: _,
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

    /// Discard the installed context only when it still matches `expected_digest`,
    /// invoking `on_cleared` while the own-transition guard is still held.
    ///
    /// Holding the guard across the callback linearizes the conditional clear
    /// with a terminal-latch publication: a public refresh that promotes a new
    /// context must take the same guard, so it can neither interleave between the
    /// digest check and the clear nor invalidate the latched terminal. Returns
    /// whether the context was actually cleared.
    ///
    /// # Errors
    ///
    /// Returns [`TrellisClientError::Bootstrap`] when the state lock is poisoned.
    pub fn clear_if_installed(
        &self,
        expected_digest: &str,
        on_cleared: impl FnOnce(),
    ) -> Result<bool, TrellisClientError> {
        let transition = self.lock_own_transition()?;
        if self.stored_context_digest().ok().as_deref() != Some(expected_digest) {
            return Ok(false);
        }
        self.clear_locked(&transition)?;
        on_cleared();
        Ok(true)
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
        let permissions = current.signed.unsigned.grants.permissions().to_vec();
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

    /// Read installed application authority and its clock under the own transition.
    /// Suspended coverage is retryable; invalid signed authority is not. Already
    /// admitted sockets do not need a still-valid CONNECT credential.
    pub(crate) fn application_transport_locked(
        &self,
        transition: &OwnTransitionGuard<'_>,
    ) -> Result<Option<(trellis_protocol::TransportAuthorizationV1, i64)>, TrellisClientError> {
        let snapshot = self.maintenance_transport_locked(transition)?;
        Ok(self.availability.borrow().is_usable().then_some(snapshot))
    }

    /// Read installed signed authority and its clock for admitted-socket maintenance.
    /// Coverage may be suspended, but the signed window, revocation evidence, and
    /// corrected clock must remain valid under the own-installation transition.
    pub(crate) fn maintenance_transport_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
    ) -> Result<(trellis_protocol::TransportAuthorizationV1, i64), TrellisClientError> {
        let state = self
            .state
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?;
        let current = state.current.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context is not installed".into())
        })?;
        let now = system_now_millis()?
            .checked_add(state.server_clock_offset_ms)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
            .div_euclid(1000);
        if current.not_before > now || current.expires_at <= now {
            return Err(TrellisClientError::Bootstrap(
                "authorization context expired".into(),
            ));
        }
        if self
            .revoked_digest
            .lock()
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "revocation cache lock poisoned".into(),
                )
            })?
            .as_deref()
            == Some(current.context_digest.as_str())
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context is revoked".into(),
            ));
        }
        Ok((current.signed.unsigned.transport_authorization.clone(), now))
    }

    /// Renew through the credential's proof-bound native bootstrap or user refresh route.
    pub async fn refresh(&self, auth: &SessionAuth) -> Result<bool, TrellisClientError> {
        super::refresh::refresh(self, auth, true)
            .await
            .map(|outcome| outcome.runtime_changed)
    }

    /// Prepare a private candidate and return its exact identity.
    ///
    /// Fails closed when the installed context changed while the request waited,
    /// so nothing was prepared; a caller must never adopt whatever candidate a
    /// concurrent operation left in the slot.
    pub(crate) async fn prepare_refresh(
        &self,
        auth: &SessionAuth,
    ) -> Result<PreparedOwnCandidate, TrellisClientError> {
        super::refresh::refresh(self, auth, false)
            .await?
            .prepared
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "authorization refresh prepared no candidate".into(),
                )
            })
    }

    pub(crate) async fn lock_refresh(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.refresh.lock().await
    }

    pub(crate) fn request_refresh(&self) {
        self.request_reconciliation(true);
    }

    /// Next-connect routing credential for the NATS authenticator.
    ///
    /// Missing or expired material asks the shared refresh owner to prepare a
    /// replacement and still fails the attempt, so the bounded reconnect loop
    /// retries with the next verified snapshot. It never waits for refresh
    /// completion or for the explicit transport-refresh owner, which would be
    /// waiting on the very admission this attempt is authenticating.
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
        let Some(remaining) = self.refresh_remaining_seconds()? else {
            return Ok(std::time::Duration::from_secs(1));
        };
        Ok(std::time::Duration::from_secs(
            u64::try_from(remaining.max(5)).map_err(|_| {
                TrellisClientError::Bootstrap("context refresh delay overflow".into())
            })?,
        ))
    }

    /// Coverage retries must not postpone an already-due credential renewal.
    pub(crate) fn credential_refresh_due(&self) -> Result<bool, TrellisClientError> {
        Ok(self
            .refresh_remaining_seconds()?
            .is_none_or(|remaining| remaining <= 0))
    }

    fn refresh_remaining_seconds(&self) -> Result<Option<i64>, TrellisClientError> {
        let state = self.state_snapshot()?;
        let now = system_now_millis()?
            .checked_add(state.server_clock_offset_ms)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
            .div_euclid(1000);
        let Some(current) = state.current.as_ref() else {
            return Ok(None);
        };
        let route_refresh = state.routing.as_ref().map_or(now, |route| {
            route
                .bootstrap_jwt_expires_at
                .saturating_sub(i64::from(current.bundle.policy.refresh_lead_seconds))
        });
        Ok(Some(
            current.refresh_at.min(route_refresh).saturating_sub(now),
        ))
    }

    /// Validate the promoted context's routing credential and return it with the
    /// exact digest it belongs to.
    fn validated_promoted_transport(
        &self,
        state: &CachedAuthorizationState,
    ) -> Result<(String, String), TrellisClientError> {
        let current = state.current.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization context is not installed".into())
        })?;
        let routing = state.routing.as_ref().ok_or_else(|| {
            TrellisClientError::Bootstrap("authorization routing JWT unavailable".into())
        })?;
        let now = system_now_millis()?
            .checked_add(state.server_clock_offset_ms)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
            .div_euclid(1000);
        if routing.bootstrap_jwt_expires_at <= now {
            return Err(TrellisClientError::Bootstrap(
                "authorization routing JWT expired".into(),
            ));
        }
        if current.not_before > now || current.expires_at <= now {
            return Err(TrellisClientError::Bootstrap(
                "authorization context expired".into(),
            ));
        }
        if self.is_revoked(&current.context_digest) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context is revoked".into(),
            ));
        }
        Ok((
            routing.bootstrap_jwt.clone(),
            current.context_digest.clone(),
        ))
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
        Ok(crate::live::authority::PinnedPeerIdentity::from_signed(
            &current.signed,
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

    /// Reads only the verified, promoted context; private candidates stay private.
    pub(crate) fn installed_signed_context(
        &self,
    ) -> Result<Arc<trellis_protocol::SignedAuthorizationContext>, TrellisClientError> {
        self.state
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context cache lock poisoned".into()))?
            .current
            .as_ref()
            .map(|current| Arc::clone(&current.signed))
            .ok_or_else(|| {
                TrellisClientError::Bootstrap("authorization context unavailable".into())
            })
    }

    /// Read the promoted context's digest, transport policy, runtime, and route
    /// credential from one snapshot so a transport generation correlates its
    /// CONNECT credential with the policy it records as admitted.
    pub(crate) fn own_transport_snapshot(
        &self,
    ) -> Result<super::types::OwnTransportSnapshot, TrellisClientError> {
        let state = self.state_snapshot()?;
        let (routing_jwt, context_digest) = self.validated_promoted_transport(&state)?;
        let current = state.current.ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable("authorization context unavailable".into())
        })?;
        let context = &current.signed;
        let runtime = state.runtime.ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable("authorization runtime unavailable".into())
        })?;
        Ok(super::types::OwnTransportSnapshot {
            context_digest,
            policy: context.unsigned.transport_authorization.clone(),
            runtime,
            routing_jwt,
        })
    }

    /// Read the freshly prepared own candidate as one coherent transport snapshot
    /// for own-coverage initialization and promotion.
    ///
    /// The caller holds the own-installation transition (a checked API
    /// requirement: taking it again would deadlock on the non-reentrant lock).
    /// `expected_instance` must be the exact prepared instance this operation
    /// created, so an independently prepared, signed-identical replacement with
    /// fresh route/clock companion material is never substituted for it.
    ///
    /// Only the private prepared candidate for `expected_digest` and
    /// `expected_instance` is exposed: the promoted installation and any retained
    /// predecessor policy are never used, because the retained policy may be the
    /// already-revoked predecessor. Fails closed when no matching candidate is
    /// prepared, the candidate is the locally revoked digest, the candidate's
    /// signed validity window is not current under its own corrected clock, or its
    /// identity no longer matches this connection. Reads and validates only; never
    /// mutates.
    ///
    /// # Errors
    ///
    /// Returns [`TrellisClientError::AuthorizationUnavailable`] for a missing,
    /// changed, revoked, or expired candidate, and
    /// [`TrellisClientError::Bootstrap`] when the candidate cannot be parsed or its
    /// identity does not match this connection.
    pub(crate) fn own_candidate_transport_snapshot_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
        expected_digest: &str,
        expected_instance: &Arc<()>,
    ) -> Result<OwnCandidateTransportSnapshot, TrellisClientError> {
        self.own_candidate_snapshot(expected_digest, expected_instance)
    }

    fn own_candidate_snapshot(
        &self,
        expected_digest: &str,
        expected_instance: &Arc<()>,
    ) -> Result<OwnCandidateTransportSnapshot, TrellisClientError> {
        let candidate = self
            .candidate
            .read()
            .map_err(|_| TrellisClientError::Bootstrap("context candidate lock poisoned".into()))?;
        let prepared = candidate.as_ref().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "authorization candidate is unavailable".into(),
            )
        })?;
        if prepared.current.context_digest != expected_digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate changed before coverage initialization".into(),
            ));
        }
        if !Arc::ptr_eq(expected_instance, &prepared.instance) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate instance changed before coverage initialization".into(),
            ));
        }
        if self.is_revoked(expected_digest) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate is revoked".into(),
            ));
        }
        let now = system_now_millis()?
            .checked_add(prepared.server_clock_offset_ms)
            .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
            .div_euclid(1000);
        if prepared.current.not_before > now || prepared.current.expires_at <= now {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate is no longer current".into(),
            ));
        }
        let signed = &prepared.current.signed;
        let context = &signed.unsigned;
        if context.connection_id != self.connection_id
            || context.session_key != self.session_key
            || context.participant_id != self.participant_id
            || prepared.runtime.connection_id != context.connection_id
            || prepared.runtime.login_session_id != context.login_session_id
            || prepared.runtime.participant_id != context.participant_id
            || prepared.runtime.inbox_prefix != context.inbox_prefix
        {
            return Err(TrellisClientError::Bootstrap(
                "authorization candidate does not match this connection".into(),
            ));
        }
        Ok(OwnCandidateTransportSnapshot {
            context_digest: prepared.current.context_digest.clone(),
            policy: context.transport_authorization.clone(),
            runtime: prepared.runtime.clone(),
            routing_jwt: prepared.routing.bootstrap_jwt.clone(),
            not_before: prepared.current.not_before,
            expires_at: prepared.current.expires_at,
            corrected_now_seconds: now,
            server_clock_offset_ms: prepared.server_clock_offset_ms,
            routing_jwt_expires_at: prepared.routing.bootstrap_jwt_expires_at,
            instance: prepared.instance.clone(),
        })
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
                    transport_authorization: trellis_protocol::TransportAuthorizationV1 {
                        format: trellis_protocol::TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
                        account: "AACCOUNT".to_owned(),
                        publish_allow: vec![],
                        subscribe_allow: vec![],
                        response: None,
                        hard_expires_at: None,
                    },
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
            let cache = AuthorizationContextCache::new(
                "http://127.0.0.1:1/",
                "test.Caller".to_owned(),
                connection_id.clone(),
                session.session_key.clone(),
                AuthorizationCredential::User {
                    login_session_id: "login-test".to_owned(),
                    installation: Arc::new(SessionAuth::from_seed_base64url(&seed).unwrap()),
                },
                None,
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
    fn maintenance_accepts_suspended_signed_authority_but_rejects_revocation() {
        let fixture = own_context_fixture(1);
        let cache = &fixture.cache;
        let transition = cache.lock_own_transition().unwrap();
        assert!(cache
            .application_transport_locked(&transition)
            .unwrap()
            .is_some());
        cache.suspend_locked(&transition);
        let (policy, _) = cache.maintenance_transport_locked(&transition).unwrap();
        assert_eq!(policy, fixture.signed.unsigned.transport_authorization);
        assert!(cache
            .application_transport_locked(&transition)
            .unwrap()
            .is_none());

        cache.mark_revoked(&fixture.digest);
        assert!(matches!(
            cache.maintenance_transport_locked(&transition),
            Err(TrellisClientError::AuthorizationUnavailable(message))
                if message == "authorization context is revoked"
        ));
        assert!(cache.application_transport_locked(&transition).is_err());
    }

    #[test]
    fn maintenance_rejects_invalid_signed_windows_and_corrected_clock_overflow() {
        // Move the corrected clock, not the installed signed window: the signed
        // transport policy has no hard expiry and cannot replace context validity.
        for (offset_ms, expected) in [
            (-3_600_000, "authorization context expired"),
            (7_200_000, "authorization context expired"),
            (i64::MAX, "context time overflow"),
        ] {
            let fixture = own_context_fixture(1);
            let cache = &fixture.cache;
            let transition = cache.lock_own_transition().unwrap();
            cache.suspend_locked(&transition);
            cache.state.write().unwrap().server_clock_offset_ms = offset_ms;
            assert!(matches!(
                cache.maintenance_transport_locked(&transition),
                Err(TrellisClientError::Bootstrap(message)) if message == expected
            ));
            assert!(cache.application_transport_locked(&transition).is_err());
        }
    }

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
        assert_ne!(candidate.context_digest, installed);
        assert_eq!(
            cache.retained_context_digest().unwrap(),
            installed,
            "a prepared candidate stays private"
        );

        // Run promotion under a watchdog: a recursive state lock would block a
        // synchronous guard and never complete this call.
        let cache_for_thread = cache.clone();
        let expected = candidate.context_digest.clone();
        // The exact identity this preparation returned must be what the promotion
        // validates; an independently prepared signed-identical replacement would
        // not match.
        let expected_instance = candidate.instance.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let transition = cache_for_thread
                .lock_own_transition()
                .map_err(|error| error.to_string())?;
            let result = cache_for_thread
                .promote_locked(&transition, &expected, &expected_instance)
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
        assert_eq!(
            cache.retained_context_digest().unwrap(),
            candidate.context_digest
        );
        assert_eq!(cache.context_digest().unwrap(), candidate.context_digest);
        assert!(cache.candidate_digest().is_err());
    }

    /// The exact identity returned by `prepare` fences both source selection and
    /// the guarded promotion: an independently prepared signed-identical
    /// replacement shares the digest but carries fresh companion route material,
    /// so it is never substituted for the originating instance, and the promotion
    /// that does succeed retains the second candidate's route material.
    #[test]
    fn candidate_instance_identity_is_exact_not_digest_equality() {
        let fixture = own_context_fixture(1);
        let cache = fixture.cache.clone();
        let now = test_support::now_seconds();
        // Same signed context (identical digest) with different companion route
        // material, prepared as two independent instances.
        let prepared = |route: &str| {
            let mut installation = installation(
                &fixture.issuer,
                &fixture.session,
                &fixture.connection_id,
                2,
                now,
            );
            installation.routing.bootstrap_jwt = route.to_owned();
            installation
        };
        // Use the exact identity each preparation returned; never re-read the
        // slot, which is exactly the digest-only substitution this fences.
        let first = cache.prepare(prepared("route-jwt-superseded")).unwrap();
        let second = cache.prepare(prepared("route-jwt-live")).unwrap();
        assert_eq!(
            first.context_digest, second.context_digest,
            "signed-identical candidates share a digest"
        );
        assert!(
            !Arc::ptr_eq(&first.instance, &second.instance),
            "each prepared candidate is a distinct instance"
        );

        // Holding the real own-installation transition, the superseded instance is
        // rejected even though the digest still matches, and the live instance is
        // accepted with the live companion route material.
        let transition = cache.lock_own_transition().unwrap();
        assert!(
            cache
                .own_candidate_transport_snapshot_locked(
                    &transition,
                    &first.context_digest,
                    &first.instance,
                )
                .is_err(),
            "a superseded prepared instance must not be accepted by digest alone"
        );
        assert!(
            cache
                .own_candidate_transport_snapshot_locked(
                    &transition,
                    &second.context_digest,
                    &first.instance,
                )
                .is_err(),
            "a mismatched instance identity must be rejected"
        );
        let live = cache
            .own_candidate_transport_snapshot_locked(
                &transition,
                &second.context_digest,
                &second.instance,
            )
            .unwrap();
        assert_eq!(live.context_digest, second.context_digest);
        assert_eq!(live.routing_jwt, "route-jwt-live");

        // The guarded promotion uses the same exact identity: the superseded
        // candidate's returned instance is refused, the second succeeds, and the
        // promoted installation retains the second companion route.
        assert!(
            cache
                .promote_locked(&transition, &second.context_digest, &first.instance)
                .is_err(),
            "promotion must reject a replaced candidate's originating identity"
        );
        assert!(
            cache.candidate_digest_locked(&transition).is_ok(),
            "a refused promotion leaves the candidate private"
        );
        cache
            .promote_locked(&transition, &second.context_digest, &second.instance)
            .unwrap();
        assert_eq!(
            cache.own_transport_snapshot().unwrap().routing_jwt,
            "route-jwt-live",
            "the promoted installation retains the candidate's companion route"
        );
        assert!(
            cache.candidate_digest_locked(&transition).is_err(),
            "a successful promotion consumes the private candidate"
        );
    }
}
