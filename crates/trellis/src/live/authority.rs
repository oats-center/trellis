//! Retained stream authority from the connected runtime's shared owner.

use crate::client::AuthorizationOwner;
use std::sync::{Arc, RwLock};
use trellis_protocol::{
    CatalogActionIdentity, LiveErrorCode, SessionBinding, SessionRequest, SessionRequestProof,
    SignedCatalogSnapshot, SignedProviderCertificate, VerifiedSessionAuthority,
};

/// Immutable runtime identity pinned for a Live session's lifetime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedPeerIdentity {
    /// Signed runtime ID, carried as `connectionId` by Live/Transfer.
    pub connection_id: String,
    /// Signed ephemeral public key, carried as `sessionKey` by Live/Transfer.
    pub session_key: String,
    /// Authenticated principal.
    pub principal_id: String,
    /// Actual application, service or device binding.
    pub binding: SessionBinding,
}
impl PinnedPeerIdentity {
    /// Project immutable coordinates from verified authority.
    pub(crate) fn from_authority(authority: &VerifiedSessionAuthority) -> Self {
        let body = authority.authority();
        Self {
            connection_id: body.runtime_id.clone(),
            session_key: body.session_public_key.clone(),
            principal_id: body.principal_id.clone(),
            binding: body.binding.clone(),
        }
    }
    /// Only provisioned services and devices have participant identities.
    pub(crate) fn participant_id(&self) -> &str {
        match &self.binding {
            SessionBinding::Service { participant_id, .. }
            | SessionBinding::Device { participant_id, .. } => participant_id,
            _ => "",
        }
    }
    pub(crate) fn deployment_id(&self) -> Option<&str> {
        match &self.binding {
            SessionBinding::Service { deployment_id, .. }
            | SessionBinding::Device { deployment_id, .. } => Some(deployment_id),
            _ => None,
        }
    }
    pub(crate) fn instance_id(&self) -> Option<&str> {
        match &self.binding {
            SessionBinding::Service { instance_id, .. }
            | SessionBinding::Device { instance_id, .. } => Some(instance_id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LiveAuthorityLost {
    TransportUnavailable,
    EpochChanged,
    CoverageLost,
    Revoked,
    CoverageUnknown,
    Expired,
    IdentityChanged,
    BindingChanged,
    PermissionLost,
}
impl LiveAuthorityLost {
    pub(crate) fn end_reason(self) -> trellis_protocol::LiveEndReason {
        match self {
            Self::TransportUnavailable => trellis_protocol::LiveEndReason::LocalShutdown,
            Self::EpochChanged => trellis_protocol::LiveEndReason::Disconnected,
            Self::IdentityChanged | Self::BindingChanged => {
                trellis_protocol::LiveEndReason::BindingChanged
            }
            _ => trellis_protocol::LiveEndReason::AuthorizationLost,
        }
    }
}

/// Descriptor-derived action identity, never a raw grant.
#[derive(Clone)]
pub(crate) struct LiveAction {
    pub api_id: String,
    pub identity: CatalogActionIdentity,
}
pub(crate) enum LiveGuardRequirement {
    Observer(LiveAction),
    LocalProvider,
    PeerProvider {
        expected: PinnedPeerIdentity,
        action: LiveAction,
    },
}
struct RetainedAuthority {
    authority: Arc<VerifiedSessionAuthority>,
    catalog: Option<Arc<SignedCatalogSnapshot>>,
    certificate: Option<Arc<SignedProviderCertificate>>,
}
pub(crate) struct LiveAuthorityCandidate {
    retained: RetainedAuthority,
}
pub(crate) struct LiveAuthorityGuard {
    owner: AuthorizationOwner,
    transport: async_nats::Client,
    retained: RwLock<RetainedAuthority>,
    tracks_local: bool,
    identity: PinnedPeerIdentity,
    requirement: LiveGuardRequirement,
}
impl LiveAuthorityGuard {
    pub(crate) async fn retain(
        owner: &AuthorizationOwner,
        transport: &async_nats::Client,
        digest: &str,
        requirement: LiveGuardRequirement,
    ) -> Result<Self, LiveAuthorityLost> {
        let own = owner
            .own_authority()
            .map_err(|_| LiveAuthorityLost::CoverageLost)?;
        let tracks_local = own.digest() == digest;
        let authority = if tracks_local {
            own
        } else {
            owner
                .resolve_authority(transport, digest)
                .await
                .map_err(|_| LiveAuthorityLost::CoverageLost)?
                .verified()
        };
        let identity = PinnedPeerIdentity::from_authority(&authority);
        let guard = Self {
            owner: owner.clone(),
            transport: transport.clone(),
            retained: RwLock::new(RetainedAuthority {
                authority,
                catalog: None,
                certificate: None,
            }),
            tracks_local,
            identity,
            requirement,
        };
        let candidate = guard.prepare_replacement(digest).await?;
        guard.commit_replacement(candidate)?;
        Ok(guard)
    }
    pub(crate) fn identity(&self) -> &PinnedPeerIdentity {
        &self.identity
    }
    pub(crate) fn action(&self) -> Option<&LiveAction> {
        match &self.requirement {
            LiveGuardRequirement::Observer(action)
            | LiveGuardRequirement::PeerProvider { action, .. } => Some(action),
            LiveGuardRequirement::LocalProvider => None,
        }
    }
    pub(crate) fn context_digest(&self) -> String {
        self.retained.read().map_or_else(
            |_| String::new(),
            |retained| retained.authority.digest().to_owned(),
        )
    }
    pub(crate) fn expires_at_seconds(&self) -> i64 {
        self.retained.read().map_or(0, |retained| {
            retained.certificate.as_ref().map_or(
                retained.authority.authority().expires_at,
                |certificate| {
                    certificate
                        .expires_at
                        .min(retained.authority.authority().expires_at)
                },
            )
        })
    }
    pub(crate) async fn reconcile(&self) -> Result<(), LiveAuthorityLost> {
        if self.tracks_local {
            let own = self
                .owner
                .own_authority()
                .map_err(|_| LiveAuthorityLost::CoverageLost)?;
            if own.digest() != self.context_digest() {
                self.commit_replacement(self.prepare_replacement(own.digest()).await?)?;
            }
        }
        self.check_now()
    }
    pub(crate) async fn rebind_current_epoch(&self) -> Result<(), LiveAuthorityLost> {
        self.reconcile().await
    }
    pub(crate) fn check_now(&self) -> Result<(), LiveAuthorityLost> {
        let retained = self
            .retained
            .read()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)?;
        self.owner
            .assert_current(&retained.authority)
            .map_err(|_| LiveAuthorityLost::Revoked)?;
        if let Some(catalog) = &retained.catalog {
            self.owner
                .assert_issuer_current(&catalog.issuer_key_id)
                .map_err(|_| LiveAuthorityLost::Revoked)?;
        }
        if let Some(certificate) = &retained.certificate {
            self.owner
                .assert_issuer_current(&certificate.issuer_key_id)
                .map_err(|_| LiveAuthorityLost::Revoked)?;
        }
        if PinnedPeerIdentity::from_authority(&retained.authority) != self.identity {
            return Err(LiveAuthorityLost::IdentityChanged);
        }
        if let LiveGuardRequirement::Observer(action) = &self.requirement {
            if !retained.catalog.as_ref().is_some_and(|catalog| {
                catalog.authorizes(retained.authority.signed(), &action.identity)
            }) {
                return Err(LiveAuthorityLost::PermissionLost);
            }
        }
        if retained.certificate.as_ref().is_some_and(|certificate| {
            certificate.expires_at <= self.owner.policy().now_unix_seconds
        }) {
            return Err(LiveAuthorityLost::Expired);
        }
        Ok(())
    }
    pub(crate) async fn prepare_replacement(
        &self,
        digest: &str,
    ) -> Result<LiveAuthorityCandidate, LiveAuthorityLost> {
        let authority = if self.tracks_local {
            let own = self
                .owner
                .own_authority()
                .map_err(|_| LiveAuthorityLost::CoverageLost)?;
            if own.digest() != digest {
                return Err(LiveAuthorityLost::BindingChanged);
            }
            own
        } else {
            self.owner
                .resolve_authority(&self.transport, digest)
                .await
                .map_err(|_| LiveAuthorityLost::CoverageLost)?
                .verified()
        };
        if PinnedPeerIdentity::from_authority(&authority) != self.identity {
            return Err(LiveAuthorityLost::IdentityChanged);
        }
        let mut retained = RetainedAuthority {
            authority,
            catalog: None,
            certificate: None,
        };
        match &self.requirement {
            LiveGuardRequirement::Observer(action) => {
                retained.catalog = Some(
                    self.owner
                        .authorize_action(
                            &self.transport,
                            &retained.authority,
                            &action.api_id,
                            &action.identity,
                        )
                        .await
                        .map_err(|_| LiveAuthorityLost::PermissionLost)?,
                );
            }
            LiveGuardRequirement::PeerProvider { expected, action } => {
                if expected != &self.identity {
                    return Err(LiveAuthorityLost::IdentityChanged);
                }
                retained.certificate = Some(
                    self.owner
                        .authorize_provider(
                            &self.transport,
                            &retained.authority,
                            &action.api_id,
                            &action.identity,
                        )
                        .await
                        .map_err(|_| LiveAuthorityLost::PermissionLost)?,
                );
            }
            LiveGuardRequirement::LocalProvider => {}
        }
        self.owner
            .assert_current(&retained.authority)
            .map_err(|_| LiveAuthorityLost::Revoked)?;
        Ok(LiveAuthorityCandidate { retained })
    }
    pub(crate) fn commit_replacement(
        &self,
        candidate: LiveAuthorityCandidate,
    ) -> Result<(), LiveAuthorityLost> {
        self.owner
            .assert_current(&candidate.retained.authority)
            .map_err(|_| LiveAuthorityLost::Revoked)?;
        if let Some(catalog) = &candidate.retained.catalog {
            self.owner
                .assert_issuer_current(&catalog.issuer_key_id)
                .map_err(|_| LiveAuthorityLost::Revoked)?;
        }
        if let Some(certificate) = &candidate.retained.certificate {
            self.owner
                .assert_issuer_current(&certificate.issuer_key_id)
                .map_err(|_| LiveAuthorityLost::Revoked)?;
            if certificate.expires_at <= self.owner.policy().now_unix_seconds {
                return Err(LiveAuthorityLost::Expired);
            }
        }
        if PinnedPeerIdentity::from_authority(&candidate.retained.authority) != self.identity {
            return Err(LiveAuthorityLost::IdentityChanged);
        }
        *self
            .retained
            .write()
            .map_err(|_| LiveAuthorityLost::CoverageUnknown)? = candidate.retained;
        Ok(())
    }
    pub(crate) fn subscribe_changes(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.owner.subscribe_changes()
    }
    pub(crate) async fn verify_control_request(
        &self,
        subject: &str,
        reply: &str,
        payload: &[u8],
        headers: &async_nats::HeaderMap,
    ) -> Result<(), LiveErrorCode> {
        self.reconcile()
            .await
            .map_err(|_| LiveErrorCode::PermissionDenied)?;
        if header_once(headers, "session-key")? != self.identity.session_key {
            return Err(LiveErrorCode::PermissionDenied);
        }
        let digest = header_once(headers, "authorization-context")?;
        let candidate = self
            .prepare_replacement(&digest)
            .await
            .map_err(|_| LiveErrorCode::PermissionDenied)?;
        let LiveGuardRequirement::Observer(action) = &self.requirement else {
            return Err(LiveErrorCode::PermissionDenied);
        };
        let api = candidate
            .retained
            .authority
            .authority()
            .apis
            .iter()
            .find(|api| api.api_id == action.api_id)
            .ok_or(LiveErrorCode::PermissionDenied)?;
        let request = SessionRequest {
            authority_digest: digest,
            api_id: action.api_id.clone(),
            api_generation: api.generation,
            accepted_revision: api.accepted_revision,
            action: action.identity.name.clone(),
            subject: subject.to_owned(),
            reply_subject: Some(reply.to_owned()),
            request_id: header_once(headers, "request-id")?,
            issued_at: header_once(headers, "iat")?
                .parse()
                .map_err(|_| LiveErrorCode::InvalidRequest)?,
        };
        let proof = SessionRequestProof::parse(header_once(headers, "proof")?)
            .map_err(|_| LiveErrorCode::InvalidRequest)?;
        self.owner
            .verify_request(&self.transport, &request, payload, &proof, &action.identity)
            .await
            .map_err(|_| LiveErrorCode::PermissionDenied)?;
        self.commit_replacement(candidate)
            .map_err(|_| LiveErrorCode::PermissionDenied)
    }
}
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
#[cfg(test)]
mod tests {
    use super::header_once;
    #[test]
    fn duplicate_security_headers_are_rejected() {
        let mut headers = async_nats::HeaderMap::new();
        headers.append("session-key", "one");
        assert!(header_once(&headers, "session-key").is_ok());
        headers.append("session-key", "two");
        assert!(header_once(&headers, "session-key").is_err());
    }
}
