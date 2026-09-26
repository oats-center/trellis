//! Retained live-authority guards for active observation sessions.
//!
//! A live session must remain authorized for its entire lifetime without a
//! network lookup per frame. This module wraps the existing provider cache's
//! retained lease and locally covered evidence in a typed check that fails
//! closed. It deliberately does **not** reuse the cache's permissive
//! `current_context_allows` shortcut: that helper converts an unknown-coverage
//! revocation query through `.ok().flatten()` and only checks
//! `health().is_ok()`, both of which can read as authorization success when the
//! exact coverage is actually unknown.
//!
//! A valid, identity-preserving refresh on the same transport epoch replaces
//! the retained lease without closing the session or resetting its sequence
//! and credit state. Every loss that can stop a quiet session (revocation,
//! lost coverage, expiry, epoch change) is observable through
//! [`LiveAuthorityGuard::subscribe_changes`] and through
//! [`LiveAuthorityGuard::check_now`].

use std::sync::RwLock;

use trellis_protocol::{
    AuthorizationVerificationPolicy, LiveErrorCode, PermissionAtom, ProtocolError,
};

use crate::client::{
    AuthorizationContextLease, AuthorizationProviderCache, AuthorizationVerificationCore,
    RequestVerificationInput, TrellisClientError,
};

/// Immutable identity tuple a live session pins for its whole lifetime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedPeerIdentity {
    /// Logical runtime connection id from the signed context.
    pub connection_id: String,
    /// Runtime signing public key.
    pub session_key: String,
    /// Principal id.
    pub principal_id: String,
    /// Participant id.
    pub participant_id: String,
    /// Deployment id, absent for undeployed principals.
    pub deployment_id: Option<String>,
    /// Instance id, absent for undeployed principals.
    pub instance_id: Option<String>,
}

/// Why a retained live authority is no longer usable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LiveAuthorityLost {
    /// The local transport or provider connection is not usable.
    TransportUnavailable,
    /// The local transport epoch changed since the guard was installed.
    EpochChanged,
    /// The exact revocation coverage entry is no longer retained.
    CoverageLost,
    /// The context digest has recorded revocation evidence.
    Revoked,
    /// Revocation coverage is unknown; the guard fails closed.
    CoverageUnknown,
    /// The context's signed validity window no longer covers the current time.
    Expired,
    /// The pinned identity tuple no longer matches the retained context.
    IdentityChanged,
    /// The installed API binding generation changed.
    BindingChanged,
    /// The exact required permission is no longer granted.
    PermissionLost,
}

impl LiveAuthorityLost {
    /// Return the terminal reason this loss maps to.
    #[must_use]
    pub(crate) fn end_reason(self) -> trellis_protocol::LiveEndReason {
        match self {
            Self::TransportUnavailable => trellis_protocol::LiveEndReason::LocalShutdown,
            Self::EpochChanged => trellis_protocol::LiveEndReason::Disconnected,
            Self::CoverageLost | Self::Revoked | Self::CoverageUnknown => {
                trellis_protocol::LiveEndReason::AuthorizationLost
            }
            Self::Expired => trellis_protocol::LiveEndReason::AuthorizationLost,
            Self::IdentityChanged | Self::BindingChanged => {
                trellis_protocol::LiveEndReason::BindingChanged
            }
            Self::PermissionLost => trellis_protocol::LiveEndReason::AuthorizationLost,
        }
    }
}

/// Extra evidence a retained guard must keep true besides the covered lease.
pub(crate) enum LiveGuardRequirement {
    /// Local consumer or admitted remote caller Subscribe/Observe atom.
    Observer(PermissionAtom),
    /// Publishing endpoint: installed identity, not the caller's Subscribe grant.
    LocalProvider,
    /// Consumer authenticating a remote provider's pinned identity.
    PeerProvider { expected: PinnedPeerIdentity },
}

/// One validated replacement lease that has not been installed yet.
///
/// The candidate's lease is fully resolved and covered; the caller may verify
/// one request against it before committing. Dropping an uncommitted candidate
/// releases its lease.
pub(crate) struct LiveAuthorityCandidate {
    lease: AuthorizationContextLease,
    identity: PinnedPeerIdentity,
    digest: String,
}

/// One retained live-authority guard over an existing provider-cache lease.
pub(crate) struct LiveAuthorityGuard {
    cache: AuthorizationProviderCache,
    lease: RwLock<AuthorizationContextLease>,
    expected_epoch: RwLock<u64>,
    digest: RwLock<String>,
    /// Whether this guard tracks the connection's own refreshable authority.
    tracks_local: bool,
    identity: PinnedPeerIdentity,
    permission: Option<PermissionAtom>,
    peer: Option<PinnedPeerIdentity>,
}

impl LiveAuthorityGuard {
    /// Retain a live guard for one exact digest, permission, and pinned identity.
    ///
    /// # Errors
    ///
    /// Returns [`LiveAuthorityLost`] when the cache cannot currently retain the
    /// exact covered evidence the session needs.
    pub(crate) async fn retain(
        cache: &AuthorizationProviderCache,
        digest: &str,
        requirement: LiveGuardRequirement,
    ) -> Result<Self, LiveAuthorityLost> {
        let expected_epoch = cache.epoch();
        let lease = retain_live_lease(cache, digest, expected_epoch).await?;
        let identity = pinned_identity(&lease);
        let tracks_local = cache.current_local_context_digest().as_deref() == Some(digest);
        let (permission, peer) = match requirement {
            LiveGuardRequirement::Observer(permission) => {
                if !lease.allows(&permission) {
                    return Err(LiveAuthorityLost::PermissionLost);
                }
                (Some(permission), None)
            }
            LiveGuardRequirement::LocalProvider => (None, None),
            LiveGuardRequirement::PeerProvider { expected } => {
                if identity != expected {
                    return Err(LiveAuthorityLost::IdentityChanged);
                }
                (None, Some(expected))
            }
        };
        Ok(Self {
            cache: cache.clone(),
            lease: RwLock::new(lease),
            expected_epoch: RwLock::new(expected_epoch),
            digest: RwLock::new(digest.to_owned()),
            tracks_local,
            identity,
            permission,
            peer,
        })
    }

    /// Return the pinned identity this session retains.
    #[must_use]
    pub(crate) fn identity(&self) -> &PinnedPeerIdentity {
        &self.identity
    }

    /// Return the retained context digest.
    #[must_use]
    pub(crate) fn context_digest(&self) -> String {
        self.lease
            .read()
            .map_or_else(|_| String::new(), |lease| lease.context_digest().to_owned())
    }

    /// Return the retained context expiry in Unix seconds.
    #[must_use]
    pub(crate) fn expires_at_seconds(&self) -> i64 {
        self.lease
            .read()
            .map_or(0, |lease| lease.signed_context().unsigned.expires_at)
    }

    /// Return whether this guard is inside a planned credential rotation whose
    /// replacement coverage is not yet established.
    ///
    /// The logical connection and its session remain alive; the guard pauses new
    /// decisions until it rebinds onto the replacement physical attachment.
    #[must_use]
    pub(crate) fn maintenance(&self) -> bool {
        if self.lease.read().is_err() {
            return false;
        }
        let Ok(epoch) = self.expected_epoch.read() else {
            return false;
        };
        self.cache.maintenance_for(*epoch)
    }

    /// Reconcile this guard across a planned physical credential rotation.
    ///
    /// Returns `Ok(())` when the guard is usable (including after a successful
    /// rebind) and the precise terminal loss otherwise. A rebind waits, within a
    /// bounded budget, for the rotation's coverage to settle, then re-resolves
    /// the guard's exact digest on the replacement attachment, re-validates the
    /// pinned identity and role requirement, and only then releases the
    /// predecessor lease.
    pub(crate) async fn reconcile(&self) -> Result<(), LiveAuthorityLost> {
        if !self.maintenance() {
            return self.check_now();
        }
        let settled = self
            .cache
            .wait_rotation_settled(std::time::Duration::from_secs(30))
            .await;
        let expected = *self
            .expected_epoch
            .read()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
        if !self.cache.maintenance_for(expected) {
            return self.check_now();
        }
        if !settled {
            return Err(LiveAuthorityLost::CoverageLost);
        }
        self.rebind(self.cache.epoch()).await
    }

    /// Rebind onto the current transport epoch after an ordinary reconnect.
    ///
    /// Unlike [`Self::reconcile`], this is not gated on a planned rotation: a
    /// long-lived provider guard must adopt the replacement physical attachment
    /// before it can admit new sessions. The predecessor lease is released only
    /// after the replacement evidence is retained and validated.
    ///
    /// # Errors
    ///
    /// Returns the precise [`LiveAuthorityLost`] when the replacement coverage is
    /// unavailable or does not preserve the pinned identity or permission.
    pub(crate) async fn rebind_current_epoch(&self) -> Result<(), LiveAuthorityLost> {
        let generation = self.cache.epoch();
        let expected = *self
            .expected_epoch
            .read()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
        if generation == expected {
            return self.check_now();
        }
        self.rebind(generation).await
    }

    /// Retain, validate and install coverage for `generation`, releasing the predecessor on success.
    async fn rebind(&self, generation: u64) -> Result<(), LiveAuthorityLost> {
        let digest = if self.tracks_local {
            self.cache
                .current_local_context_digest()
                .ok_or(LiveAuthorityLost::CoverageLost)?
        } else {
            self.digest
                .read()
                .map(|digest| digest.clone())
                .unwrap_or_default()
        };
        if digest.is_empty() {
            return Err(LiveAuthorityLost::CoverageLost);
        }
        let lease = retain_live_lease(&self.cache, &digest, generation).await?;
        let identity = pinned_identity(&lease);
        if identity != self.identity {
            return Err(LiveAuthorityLost::IdentityChanged);
        }
        if let Some(expected_peer) = &self.peer {
            if &identity != expected_peer {
                return Err(LiveAuthorityLost::IdentityChanged);
            }
        }
        if let Some(permission) = &self.permission {
            if !lease.allows(permission) {
                return Err(LiveAuthorityLost::PermissionLost);
            }
        }
        {
            let mut slot = self
                .lease
                .write()
                .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
            let old = std::mem::replace(&mut *slot, lease);
            drop(slot);
            drop(old);
        }
        if let Ok(mut epoch) = self.expected_epoch.write() {
            *epoch = generation;
        }
        if let Ok(mut stored) = self.digest.write() {
            *stored = digest;
        }
        Ok(())
    }

    /// Perform the synchronous local authority check.
    ///
    /// Uses only retained local state: no network read, no cold context
    /// resolution, no reverification of the opening request's old proof.
    ///
    /// # Errors
    ///
    /// Returns the precise [`LiveAuthorityLost`] reason, never a lossy boolean.
    pub(crate) fn check_now(&self) -> Result<(), LiveAuthorityLost> {
        if self.maintenance() {
            return Ok(());
        }
        let expected_epoch = *self
            .expected_epoch
            .read()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
        let lease = self
            .lease
            .read()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
        let health = self
            .cache
            .health()
            .map_err(|_| LiveAuthorityLost::TransportUnavailable)?;
        if !health.healthy {
            return Err(LiveAuthorityLost::TransportUnavailable);
        }
        if self.cache.epoch() != expected_epoch || lease.epoch() != expected_epoch {
            return Err(LiveAuthorityLost::EpochChanged);
        }
        if !self.cache.live_guard_entry_is_covered(&lease) {
            return Err(LiveAuthorityLost::CoverageLost);
        }
        match self.cache.revocation_time(lease.context_digest()) {
            // Recorded revocation evidence.
            Ok(Some(_)) => return Err(LiveAuthorityLost::Revoked),
            // Unknown coverage must fail closed, not read as cleared.
            Err(_) => return Err(LiveAuthorityLost::CoverageUnknown),
            Ok(None) => {}
        }
        lease
            .assert_current(
                &self
                    .cache
                    .policy()
                    .map_err(|_| LiveAuthorityLost::CoverageUnknown)?,
            )
            .map_err(|_| LiveAuthorityLost::Expired)?;
        if pinned_identity(&lease) != self.identity {
            return Err(LiveAuthorityLost::IdentityChanged);
        }
        if let Some(expected) = &self.peer {
            if &self.identity != expected {
                return Err(LiveAuthorityLost::IdentityChanged);
            }
        }
        if let Some(permission) = &self.permission {
            if !lease.allows(permission) {
                return Err(LiveAuthorityLost::PermissionLost);
            }
        }
        Ok(())
    }

    /// Resolve and validate a replacement lease without installing it.
    ///
    /// The replacement must preserve the pinned identity, role requirement and
    /// transport epoch; otherwise it is discarded and the current lease stays
    /// in force.
    ///
    /// # Errors
    ///
    /// Returns the typed reason the replacement is not usable.
    pub(crate) async fn prepare_replacement(
        &self,
        digest: &str,
    ) -> Result<LiveAuthorityCandidate, LiveAuthorityLost> {
        let expected_epoch = *self
            .expected_epoch
            .read()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
        let lease = retain_live_lease(&self.cache, digest, expected_epoch).await?;
        let identity = pinned_identity(&lease);
        if identity != self.identity {
            return Err(LiveAuthorityLost::IdentityChanged);
        }
        if let Some(expected) = &self.peer {
            if &identity != expected {
                return Err(LiveAuthorityLost::IdentityChanged);
            }
        }
        if let Some(permission) = &self.permission {
            if !lease.allows(permission) {
                return Err(LiveAuthorityLost::PermissionLost);
            }
        }
        Ok(LiveAuthorityCandidate {
            lease,
            identity,
            digest: digest.to_owned(),
        })
    }

    /// Atomically install a prepared replacement on the same transport epoch.
    ///
    /// The old covered lease is released only after the new one is installed.
    /// A committed terminal session never resurrects: callers must fence before
    /// replacing when their own terminal state is already recorded.
    ///
    /// # Errors
    ///
    /// Returns [`LiveAuthorityLost`] when the epoch or identity moved.
    pub(crate) fn commit_replacement(
        &self,
        candidate: LiveAuthorityCandidate,
    ) -> Result<(), LiveAuthorityLost> {
        let expected_epoch = *self
            .expected_epoch
            .read()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
        if self.cache.epoch() != expected_epoch || candidate.lease.epoch() != expected_epoch {
            return Err(LiveAuthorityLost::EpochChanged);
        }
        if candidate.identity != self.identity {
            return Err(LiveAuthorityLost::IdentityChanged);
        }
        let digest = candidate.digest.clone();
        let mut slot = self
            .lease
            .write()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
        let old = std::mem::replace(&mut *slot, candidate.lease);
        drop(slot);
        drop(old);
        if let Ok(mut stored) = self.digest.write() {
            *stored = digest;
        }
        Ok(())
    }

    /// Subscribe to local changes that can invalidate this guard.
    ///
    /// A receiver observes revocation, lost coverage and transport-epoch
    /// movement without waiting for the next frame. Lagged receivers must
    /// re-run [`Self::check_now`].
    #[must_use]
    pub(crate) fn subscribe_changes(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.cache.subscribe_live_changes()
    }

    /// Authenticate one owner-control request against this retained caller.
    ///
    /// The pipeline is: current coverage, singleton headers, pinned session
    /// key, caller reply boundary, then proof verification. An
    /// identity-preserving caller context on the same transport epoch is
    /// validated and committed before its control is applied.
    ///
    /// # Errors
    ///
    /// Unverified, foreign-reply, and mismatched-identity requests fail so the
    /// caller can drop them without reflection.
    pub(crate) async fn verify_control_request(
        &self,
        subject: &str,
        reply: &str,
        payload: &[u8],
        headers: &async_nats::HeaderMap,
    ) -> Result<(), LiveErrorCode> {
        // Reconcile rather than a synchronous check: a planned credential
        // rotation pauses control decisions until exact coverage is rebound on
        // the replacement attachment instead of applying them from a stale lease.
        self.reconcile()
            .await
            .map_err(|_| LiveErrorCode::PermissionDenied)?;
        let session_key = header_once(headers, "session-key")?;
        if session_key != self.identity.session_key {
            return Err(LiveErrorCode::PermissionDenied);
        }
        let context_digest = header_once(headers, "authorization-context")?;
        let proof = header_once(headers, "proof")?;
        let request_id = header_once(headers, "request-id")?;
        let iat = header_once(headers, "iat")?
            .parse::<i64>()
            .map_err(|_| LiveErrorCode::InvalidRequest)?;
        let (current_digest, prefix) = {
            let lease = self
                .lease
                .read()
                .map_err(|_| LiveErrorCode::PermissionDenied)?;
            (
                lease.context_digest().to_owned(),
                lease.inbox_prefix().to_owned(),
            )
        };
        if reply != prefix && !reply.starts_with(&format!("{prefix}.")) {
            return Err(LiveErrorCode::InvalidSubject);
        }
        let policy = self
            .cache
            .policy()
            .map_err(|_| LiveErrorCode::AuthorizationUnavailable)?;
        let candidate = if context_digest != current_digest {
            Some(
                self.prepare_replacement(&context_digest)
                    .await
                    .map_err(|_| LiveErrorCode::PermissionDenied)?,
            )
        } else {
            None
        };
        let verified = match candidate.as_ref() {
            Some(candidate) => self.verify_request_against(
                &candidate.lease,
                &policy,
                subject,
                reply,
                payload,
                iat,
                &request_id,
                &proof,
                &context_digest,
            ),
            None => {
                let lease = self
                    .lease
                    .read()
                    .map_err(|_| LiveErrorCode::PermissionDenied)?;
                self.verify_request_against(
                    &lease,
                    &policy,
                    subject,
                    reply,
                    payload,
                    iat,
                    &request_id,
                    &proof,
                    &context_digest,
                )
            }
        };
        verified?;
        if let Some(candidate) = candidate {
            self.commit_replacement(candidate)
                .map_err(|_| LiveErrorCode::PermissionDenied)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn verify_request_against(
        &self,
        lease: &AuthorizationContextLease,
        policy: &AuthorizationVerificationPolicy,
        subject: &str,
        reply: &str,
        payload: &[u8],
        iat: i64,
        request_id: &str,
        proof: &str,
        context_digest: &str,
    ) -> Result<(), LiveErrorCode> {
        let required = match &self.permission {
            Some(permission) => std::slice::from_ref(permission),
            None => &[],
        };
        AuthorizationVerificationCore::new()
            .verify_request(RequestVerificationInput {
                context: lease,
                context_digest,
                subject,
                payload,
                iat,
                request_id,
                reply_subject: Some(reply),
                proof,
                policy,
                required_permissions: required,
            })
            .map_err(|_| LiveErrorCode::PermissionDenied)?;
        Ok(())
    }
}

/// Retain one covered lease, mapping cache failures to typed authority loss.
async fn retain_live_lease(
    cache: &AuthorizationProviderCache,
    digest: &str,
    expected_epoch: u64,
) -> Result<AuthorizationContextLease, LiveAuthorityLost> {
    cache
        .retain_live_guard_lease(digest, expected_epoch)
        .await
        .map_err(|error| match error {
            TrellisClientError::AuthorizationUnavailable(_) => LiveAuthorityLost::CoverageLost,
            _ => LiveAuthorityLost::CoverageUnknown,
        })
}

impl PinnedPeerIdentity {
    /// Project one signed context into its immutable identity tuple.
    #[must_use]
    pub(crate) fn from_signed(signed: &trellis_protocol::SignedAuthorizationContext) -> Self {
        Self {
            connection_id: signed.unsigned.connection_id.clone(),
            session_key: signed.unsigned.session_key.clone(),
            principal_id: signed.unsigned.principal_id.clone(),
            participant_id: signed.unsigned.participant_id.clone(),
            deployment_id: signed.unsigned.deployment_id.clone(),
            instance_id: signed.unsigned.instance_id.clone(),
        }
    }
}

fn pinned_identity(lease: &AuthorizationContextLease) -> PinnedPeerIdentity {
    PinnedPeerIdentity::from_signed(lease.signed_context())
}

/// Extract exactly one non-empty value for a security header.
///
/// Duplicate or ambiguous headers are rejected; a `get()` that ignores
/// additional values is not singleton validation.
fn header_once(headers: &async_nats::HeaderMap, name: &str) -> Result<String, LiveErrorCode> {
    let mut values = headers.get_all(name);
    let value = values
        .next()
        .ok_or(LiveErrorCode::InvalidRequest)?
        .to_string();
    if values.next().is_some() || value.is_empty() {
        return Err(LiveErrorCode::InvalidRequest);
    }
    Ok(value)
}

/// Map a protocol error into the closest authority-loss reason.
#[must_use]
pub(crate) fn authority_loss_from_protocol(error: &ProtocolError) -> LiveAuthorityLost {
    match error {
        ProtocolError::Live { message, .. } if message.contains("revoked") => {
            LiveAuthorityLost::Revoked
        }
        ProtocolError::Authorization { .. } => LiveAuthorityLost::PermissionLost,
        _ => LiveAuthorityLost::CoverageUnknown,
    }
}

#[cfg(test)]
mod tests {
    use super::header_once;

    #[test]
    fn duplicate_security_headers_are_rejected() {
        let mut headers = async_nats::HeaderMap::new();
        headers.append("session-key", "one");
        assert!(header_once(&headers, "session-key").is_ok());
        headers.append("session-key", "two");
        assert!(
            header_once(&headers, "session-key").is_err(),
            "a duplicated security header must not be accepted"
        );
    }

    #[test]
    fn missing_and_empty_security_headers_are_rejected() {
        let headers = async_nats::HeaderMap::new();
        assert!(header_once(&headers, "session-key").is_err());
        let mut headers = async_nats::HeaderMap::new();
        headers.append("session-key", "");
        assert!(header_once(&headers, "session-key").is_err());
    }
}
