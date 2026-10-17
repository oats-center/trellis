//! Logical installation lifecycle. Trust and live authority belong to one shared owner.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use trellis_protocol::{SignedSessionAuthority, VerifiedSessionAuthority};

use super::super::{SessionAuth, TrellisClientError};
use super::provider_cache::{AuthorizationOwner, RuntimeAuthorizationTrust};
use super::types::{
    AuthorizationContextBundle, AuthorizationCredential, AuthorizationInstallation,
    AuthorizationRuntimeBinding, OwnTransportSnapshot,
};

#[derive(Clone, Debug)]
pub(crate) struct PreparedOwnCandidate {
    pub(crate) context_digest: String,
    pub(crate) instance: Arc<()>,
}

#[derive(Clone)]
struct Candidate {
    authority: Arc<VerifiedSessionAuthority>,
    transport: trellis_protocol::TransportAuthorizationV1,
    expected_digest: Option<String>,
    instance: Arc<()>,
    authorization: Option<serde_json::Value>,
    resources: crate::service::ServiceResourceBindings,
    actions: Vec<crate::generated::CatalogActionProjection>,
    capabilities: Vec<trellis_protocol::CapabilityAuthority>,
}

/// Own-authority installation controller shared by all retained connection handles.
/// It never owns a second trust cache, revocation mirror, or permission database.
#[derive(Clone)]
pub struct AuthorizationContextCache {
    pub(crate) owner: AuthorizationOwner,
    pub(crate) trust: RuntimeAuthorizationTrust,
    pub(crate) runtime: AuthorizationRuntimeBinding,
    credential: Arc<
        RwLock<(
            AuthorizationCredential,
            trellis_protocol::AuthorityIssuerKey,
        )>,
    >,
    installed: Arc<RwLock<Option<AuthorizationInstallation>>>,
    candidate: Arc<RwLock<Option<Candidate>>>,
    transition: Arc<Mutex<()>>,
    pub(crate) refresh: Arc<tokio::sync::Mutex<()>>,
    refresh_requested: Arc<tokio::sync::Notify>,
    availability: tokio::sync::watch::Sender<crate::generated::AvailabilitySnapshot>,
    transport_ready: Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) struct OwnTransitionGuard<'a> {
    _guard: std::sync::MutexGuard<'a, ()>,
}

impl AuthorizationContextCache {
    pub(crate) async fn fetch_own_context(
        &self,
        auth: &SessionAuth,
        nats: &async_nats::Client,
        timeout: Duration,
    ) -> Result<super::refresh::OwnContextReply, TrellisClientError> {
        super::refresh::fetch_own_context(self, auth, nats, timeout).await
    }
    pub(crate) fn new(
        owner: AuthorizationOwner,
        trust: RuntimeAuthorizationTrust,
        runtime: AuthorizationRuntimeBinding,
        session_key: &str,
        credential: AuthorizationCredential,
    ) -> Result<Self, TrellisClientError> {
        if runtime.inbox_prefix != trellis_protocol::admission_inbox_prefix(session_key)? {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "runtime inbox mismatch".into(),
            ));
        }
        let native = runtime.transports.native.as_ref().ok_or_else(|| {
            TrellisClientError::TransportUnavailable("native transport not configured".into())
        })?;
        if native.nats_servers.is_empty() {
            return Err(TrellisClientError::TransportUnavailable(
                "native transport not configured".into(),
            ));
        }
        for endpoint in &native.nats_servers {
            endpoint
                .parse::<async_nats::ServerAddr>()
                .map_err(|error| TrellisClientError::TransportUnavailable(error.to_string()))?;
        }
        let admission_issuer = trust.issuer.clone();
        Ok(Self {
            owner,
            trust,
            runtime,
            credential: Arc::new(RwLock::new((credential, admission_issuer))),
            installed: Arc::default(),
            candidate: Arc::default(),
            transition: Arc::default(),
            refresh: Arc::default(),
            refresh_requested: Arc::default(),
            availability: tokio::sync::watch::channel(Default::default()).0,
            transport_ready: Arc::default(),
        })
    }

    pub(crate) fn lock_own_transition(&self) -> Result<OwnTransitionGuard<'_>, TrellisClientError> {
        Ok(OwnTransitionGuard {
            _guard: self.transition.lock().map_err(|_| unavailable())?,
        })
    }

    pub(crate) fn connect_token(&self, auth: &SessionAuth) -> Result<String, TrellisClientError> {
        let credential = self.credential.read().map_err(|_| unavailable())?;
        match &credential.0 {
            AuthorizationCredential::OAuth(admission) => {
                trellis_protocol::verify_oauth_admission(
                    &serde_json::to_vec(admission)?,
                    &credential.1,
                    &self.trust.instance_id,
                    &self.trust.account,
                    &auth.session_nkey()?,
                    super::super::proof::now_iat_seconds() as i64,
                    u32::MAX,
                )?;
                if admission.credential.runtime_id != self.runtime.runtime_id {
                    return Err(unavailable());
                }
                Ok(serde_json::to_string(admission)?)
            }
            AuthorizationCredential::Provisioned {
                admission,
                identity,
            } => {
                let mut admission = admission.clone();
                admission.runtime_id = self.runtime.runtime_id.clone();
                admission.ephemeral_nkey = auth.session_nkey()?;
                admission.issued_at = super::super::proof::now_iat_seconds() as i64;
                admission.expires_at = admission.issued_at.saturating_add(30);
                Ok(serde_json::to_string(
                    &trellis_protocol::sign_provisioned_admission(
                        admission,
                        identity.live_signing_key(),
                    )?,
                )?)
            }
        }
    }

    /// Retain a newly acquired OAuth CONNECT credential without changing logical
    /// application authority. The durable OAuth client owns HTTP token refresh.
    pub(crate) fn replace_oauth_admission(
        &self,
        auth: &SessionAuth,
        admission: trellis_protocol::SignedOAuthAdmission,
    ) -> Result<(), TrellisClientError> {
        let issuer = self.owner.current_trust()?.issuer;
        trellis_protocol::verify_oauth_admission(
            &serde_json::to_vec(&admission)?,
            &issuer,
            &self.trust.instance_id,
            &self.trust.account,
            &auth.session_nkey()?,
            super::super::proof::now_iat_seconds() as i64,
            u32::MAX,
        )?;
        if admission.credential.runtime_id != self.runtime.runtime_id {
            return Err(unavailable());
        }
        let mut credential = self.credential.write().map_err(|_| unavailable())?;
        let AuthorizationCredential::OAuth(current) = &credential.0 else {
            return Err(unavailable());
        };
        if current.credential.oauth_grant_id != admission.credential.oauth_grant_id {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "OAuth grant changed".into(),
            ));
        }
        *credential = (AuthorizationCredential::OAuth(admission), issuer);
        Ok(())
    }

    pub(crate) async fn prepare_authority(
        &self,
        transport: &async_nats::Client,
        signed: &SignedSessionAuthority,
        transport_policy: trellis_protocol::TransportAuthorizationV1,
        authorization: Option<serde_json::Value>,
    ) -> Result<PreparedOwnCandidate, TrellisClientError> {
        let authority = self.owner.prepare_own(transport, signed).await?;
        transport_policy.validate()?;
        if transport_policy.account != self.trust.account {
            return Err(unavailable());
        }
        if authority.authority().runtime_id != self.runtime.runtime_id
            || authority.authority().inbox_prefix != self.runtime.inbox_prefix
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "runtime assignment mismatch".into(),
            ));
        }
        let resources = if matches!(
            &authority.authority().binding,
            trellis_protocol::SessionBinding::Service { .. }
                | trellis_protocol::SessionBinding::Device { .. }
        ) {
            serde_json::from_value(
                authorization
                    .as_ref()
                    .and_then(|value| value.get("resourceRuntime"))
                    .cloned()
                    .ok_or_else(|| {
                        TrellisClientError::AuthorizationUnavailable(
                            "signed native resource bindings unavailable".into(),
                        )
                    })?,
            )?
        } else {
            crate::service::ServiceResourceBindings::default()
        };
        let mut actions = Vec::new();
        let mut capabilities = Vec::new();
        for reference in &authority.authority().apis {
            let material = self
                .owner
                .material(
                    transport,
                    super::registry::MaterialKind::Catalog,
                    &reference.catalog_snapshot_digest,
                )
                .await?;
            let super::registry::RegistryMaterial::Catalog(catalog) = material.as_ref() else {
                return Err(unavailable());
            };
            let issuer = self.owner.issuer(transport, &catalog.issuer_key_id).await?;
            catalog.verify(
                &issuer,
                &authority.authority().trellis_instance_id,
                &authority.authority().audience_nats_account,
            )?;
            if catalog.api_id != reference.api_id
                || catalog.generation != reference.generation
                || catalog.accepted_revision != reference.accepted_revision
            {
                return Err(unavailable());
            }
            for action in &catalog.actions {
                if catalog.authorizes(authority.signed(), &action.identity) {
                    actions.push(crate::generated::CatalogActionProjection {
                        api_id: reference.api_id.clone(),
                        action: action.identity.clone(),
                    });
                }
            }
            capabilities.extend(
                authority
                    .authority()
                    .capabilities
                    .iter()
                    .filter(|selected| {
                        catalog.capabilities.iter().any(|accepted| {
                            accepted.capability_id == selected.capability_id
                                && accepted.identity_generation == selected.identity_generation
                                && accepted.consent_revision == selected.consent_revision
                        })
                    })
                    .cloned(),
            );
        }
        self.owner.assert_prepared_current(&authority)?;
        let _transition = self.lock_own_transition()?;
        let candidate = Candidate {
            transport: transport_policy,
            expected_digest: self.stored_context_digest().ok(),
            instance: Arc::new(()),
            authority,
            authorization,
            resources,
            actions,
            capabilities,
        };
        let prepared = PreparedOwnCandidate {
            context_digest: candidate.authority.digest().to_owned(),
            instance: candidate.instance.clone(),
        };
        *self.candidate.write().map_err(|_| unavailable())? = Some(candidate);
        Ok(prepared)
    }

    pub(crate) fn promote_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
        expected_digest: &str,
        expected_instance: &Arc<()>,
    ) -> Result<(), TrellisClientError> {
        let mut slot = self.candidate.write().map_err(|_| unavailable())?;
        let candidate = slot.as_ref().ok_or_else(unavailable)?;
        if candidate.authority.digest() != expected_digest
            || !Arc::ptr_eq(&candidate.instance, expected_instance)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own candidate superseded".into(),
            ));
        }
        self.owner.promote_own(
            candidate.authority.clone(),
            candidate.expected_digest.as_deref(),
        )?;
        *self.installed.write().map_err(|_| unavailable())? = Some(AuthorizationInstallation {
            authority: candidate.authority.clone(),
            transport_authorization: candidate.transport.clone(),
            authorization: candidate.authorization.clone(),
        });
        let resources = candidate.resources.clone();
        let actions = candidate.actions.clone();
        let capabilities = candidate.capabilities.clone();
        let body = candidate.authority.authority();
        let state_owner = matches!(
            (body.principal_kind, &body.binding),
            (
                trellis_protocol::PrincipalKind::User,
                trellis_protocol::SessionBinding::Browser { .. }
                    | trellis_protocol::SessionBinding::Native { .. }
            ) | (
                trellis_protocol::PrincipalKind::Device,
                trellis_protocol::SessionBinding::Device { .. }
            )
        )
        .then(|| body.principal_id.clone());
        *slot = None;
        // Surface availability is projected separately from catalog authorization.
        // Every action still resolves against the owner's signed catalogs.
        let previous = self.availability.borrow().clone();
        let next = crate::generated::AvailabilitySnapshot::replacing(
            actions,
            capabilities,
            state_owner,
            resources,
            &previous,
        );
        self.availability
            .send_replace(crate::generated::AvailabilitySnapshot::with_usability(
                &next,
                self.transport_ready
                    .load(std::sync::atomic::Ordering::Acquire),
            ));
        Ok(())
    }

    /// Return server-owned native bootstrap evidence from the currently promoted
    /// signed reply. Callers deserialize the existing resource/bootstrap DTOs.
    pub(crate) fn service_bootstrap_authorization(
        &self,
    ) -> Result<serde_json::Value, TrellisClientError> {
        self.installed
            .read()
            .map_err(|_| unavailable())?
            .as_ref()
            .and_then(|installed| installed.authorization.clone())
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "native resource bootstrap unavailable".into(),
                )
            })
    }

    /// Withdraw all application usability without erasing refresh provenance.
    pub fn clear(&self) -> Result<(), TrellisClientError> {
        let transition = self.lock_own_transition()?;
        self.suspend_locked(&transition);
        *self.candidate.write().map_err(|_| unavailable())? = None;
        Ok(())
    }

    pub(crate) fn suspend(&self) {
        if let Ok(transition) = self.lock_own_transition() {
            self.suspend_locked(&transition);
        }
    }

    pub(crate) fn suspend_locked(&self, _transition: &OwnTransitionGuard<'_>) {
        let previous = self.availability.borrow().clone();
        self.availability
            .send_replace(crate::generated::AvailabilitySnapshot::suspended(&previous));
    }

    /// Retain usable authority only after installation, transport readiness, and live owner checks.
    pub fn context_digest(&self) -> Result<String, TrellisClientError> {
        if !self
            .transport_ready
            .load(std::sync::atomic::Ordering::Acquire)
            || !self.availability.borrow().is_usable()
        {
            return Err(unavailable());
        }
        self.owner.own_authority()?;
        self.retained_context_digest()
    }

    pub(crate) fn stored_context_digest(&self) -> Result<String, TrellisClientError> {
        self.installed
            .read()
            .map_err(|_| unavailable())?
            .as_ref()
            .map(|installation| installation.authority.digest().to_owned())
            .ok_or_else(unavailable)
    }

    pub(crate) fn retained_context_digest(&self) -> Result<String, TrellisClientError> {
        let authority = self.owner.staged_own_authority()?;
        if self.stored_context_digest()? != authority.digest() {
            return Err(unavailable());
        }
        Ok(authority.digest().to_owned())
    }

    pub(crate) fn maintenance_transport_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
    ) -> Result<(trellis_protocol::TransportAuthorizationV1, i64), TrellisClientError> {
        self.retained_context_digest()?;
        Ok((
            self.installed
                .read()
                .map_err(|_| unavailable())?
                .as_ref()
                .ok_or_else(unavailable)?
                .transport_authorization
                .clone(),
            self.corrected_now_seconds()?,
        ))
    }

    pub(crate) fn application_transport_locked(
        &self,
        transition: &OwnTransitionGuard<'_>,
    ) -> Result<Option<(trellis_protocol::TransportAuthorizationV1, i64)>, TrellisClientError> {
        self.owner.own_authority()?;
        let transport = self.maintenance_transport_locked(transition)?;
        Ok(self.availability.borrow().is_usable().then_some(transport))
    }

    pub(crate) fn own_transport_snapshot(
        &self,
    ) -> Result<OwnTransportSnapshot, TrellisClientError> {
        let authority = self.owner.staged_own_authority()?;
        self.retained_context_digest()?;
        Ok(OwnTransportSnapshot {
            context_digest: authority.digest().to_owned(),
            authorization_session_id: authority.authority().authorization_session_id.clone(),
            policy: self
                .installed
                .read()
                .map_err(|_| unavailable())?
                .as_ref()
                .ok_or_else(unavailable)?
                .transport_authorization
                .clone(),
        })
    }

    pub(crate) fn own_candidate_transport_snapshot_locked(
        &self,
        _transition: &OwnTransitionGuard<'_>,
        digest: &str,
        instance: &Arc<()>,
    ) -> Result<OwnCandidateTransportSnapshot, TrellisClientError> {
        let slot = self.candidate.read().map_err(|_| unavailable())?;
        let candidate = slot.as_ref().ok_or_else(unavailable)?;
        if candidate.authority.digest() != digest || !Arc::ptr_eq(&candidate.instance, instance) {
            return Err(unavailable());
        }
        self.owner.assert_prepared_current(&candidate.authority)?;
        let body = candidate.authority.authority();
        Ok(OwnCandidateTransportSnapshot {
            context_digest: digest.to_owned(),
            authorization_session_id: body.authorization_session_id.clone(),
            policy: candidate.transport.clone(),
            not_before: body.not_before,
            expires_at: body.expires_at,
            corrected_now_seconds: self.corrected_now_seconds()?,
            instance: candidate.instance.clone(),
        })
    }

    /// Return the currently verified own signed authority.
    pub fn bundle(&self) -> Result<AuthorizationContextBundle, TrellisClientError> {
        Ok(AuthorizationContextBundle {
            context: self.owner.own_authority()?.signed().clone(),
        })
    }

    pub(crate) fn installed_signed_context(
        &self,
    ) -> Result<Arc<SignedSessionAuthority>, TrellisClientError> {
        Ok(Arc::new(self.owner.own_authority()?.signed().clone()))
    }

    pub(crate) fn pinned_identity(
        &self,
    ) -> Result<crate::live::authority::PinnedPeerIdentity, TrellisClientError> {
        let authority = self.owner.own_authority()?;
        Ok(crate::live::authority::PinnedPeerIdentity::from_authority(
            &authority,
        ))
    }

    pub(crate) fn runtime_binding(
        &self,
    ) -> Result<AuthorizationRuntimeBinding, TrellisClientError> {
        Ok(self.runtime.clone())
    }

    pub(crate) fn corrected_now_seconds(&self) -> Result<i64, TrellisClientError> {
        Ok(system_now_millis()?.div_euclid(1000))
    }

    pub(crate) fn refresh_delay(&self) -> Result<Duration, TrellisClientError> {
        let installed = self.installed.read().map_err(|_| unavailable())?;
        let remaining = installed.as_ref().map_or(1, |authority| {
            authority
                .authority
                .authority()
                .expires_at
                .saturating_sub(self.owner.policy().now_unix_seconds)
                .saturating_sub(30)
        });
        Ok(Duration::from_secs(remaining.max(1) as u64))
    }

    pub(crate) fn request_refresh(&self) {
        self.refresh_requested.notify_one();
    }

    pub(crate) fn set_transport_ready(&self, ready: bool) {
        self.transport_ready
            .store(ready, std::sync::atomic::Ordering::Release);
        let usable = ready && self.retained_context_digest().is_ok();
        self.availability.send_modify(|installed| {
            *installed = crate::generated::AvailabilitySnapshot::with_usability(installed, usable);
        });
    }
    pub(crate) fn request_coverage_reconciliation(&self) {
        self.request_refresh();
    }
    pub(crate) async fn wait_refresh_request(&self) {
        self.refresh_requested.notified().await;
    }

    pub(crate) fn availability(&self) -> crate::generated::AvailabilitySnapshot {
        if self.context_digest().is_err() {
            return crate::generated::AvailabilitySnapshot::suspended(&self.availability.borrow());
        }
        self.availability.borrow().clone()
    }

    pub(crate) fn watch_availability(
        &self,
    ) -> tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot> {
        self.availability.subscribe()
    }
}

pub(crate) struct OwnCandidateTransportSnapshot {
    pub(crate) context_digest: String,
    pub(crate) authorization_session_id: String,
    pub(crate) policy: trellis_protocol::TransportAuthorizationV1,
    pub(crate) not_before: i64,
    pub(crate) expires_at: i64,
    pub(crate) corrected_now_seconds: i64,
    pub(crate) instance: Arc<()>,
}

fn unavailable() -> TrellisClientError {
    TrellisClientError::AuthorizationUnavailable(
        "logical authorization installation unavailable".into(),
    )
}

pub(super) fn system_now_millis() -> Result<i64, TrellisClientError> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| unavailable())?
            .as_millis(),
    )
    .map_err(|_| unavailable())
}
