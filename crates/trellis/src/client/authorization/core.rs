use std::sync::Arc;

use trellis_protocol::{
    verify_session_event, verify_session_request, AuthenticatedCaller, AuthenticatedCatalog,
    AuthorityIssuerKey, CatalogActionIdentity, OriginalEventPublication, ProtocolError,
    SessionAuthorityVerificationPolicy, SessionEvent, SessionEventProof,
    SessionEventVerificationInput, SessionRequest, SessionRequestProof,
    SessionRequestVerificationInput, VerifiedSessionAuthority,
};

/// Failure to authenticate a message or authorize its exact registered action.
#[derive(Debug, thiserror::Error)]
pub enum AuthorizationVerificationError {
    /// The proof, signed authority, catalog, or validity check failed.
    #[error("session proof rejected: {0}")]
    Protocol(Box<ProtocolError>),
    /// The catalog does not authorize the actual registered action.
    #[error("session capability does not authorize the registered action")]
    PermissionDenied,
}

impl From<ProtocolError> for AuthorizationVerificationError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(Box::new(error))
    }
}

/// Authenticated opening retained for dispatch and response signing.
/// Possession alone leaves `authorized_action` empty and cannot authorize a response.
#[derive(Clone, Debug)]
pub struct VerifiedAuthorizationRequest {
    authority: Arc<VerifiedSessionAuthority>,
    caller: AuthenticatedCaller,
    request: SessionRequest,
    digest: [u8; 32],
    authorized_action: Option<CatalogActionIdentity>,
    handler_authorized: bool,
}

impl VerifiedAuthorizationRequest {
    /// Exact action admitted by catalog membership or the payload-authorizing handler.
    /// `None` authenticates identity only and must not reserve Live or produce a reply.
    pub fn authorized_action(&self) -> Option<&CatalogActionIdentity> {
        self.authorized_action.as_ref()
    }

    pub(crate) fn handler_authorized(&self) -> bool {
        self.handler_authorized
    }

    pub(crate) fn authorize_handler(
        &mut self,
        action: CatalogActionIdentity,
    ) -> Result<(), AuthorizationVerificationError> {
        if self.request.action != action.name
            || self
                .authorized_action
                .as_ref()
                .is_some_and(|current| current != &action)
        {
            return Err(AuthorizationVerificationError::PermissionDenied);
        }
        self.authorized_action = Some(action);
        self.handler_authorized = true;
        Ok(())
    }
    /// Authenticated identity and whole capabilities, never header-derived identity.
    pub fn caller(&self) -> &AuthenticatedCaller {
        &self.caller
    }

    /// Signature-checked session authority used for this opening.
    pub fn authority(&self) -> &VerifiedSessionAuthority {
        &self.authority
    }

    /// Exact authenticated opening identity.
    pub fn request(&self) -> &SessionRequest {
        &self.request
    }

    /// Transcript digest to which a provider's success or error response must bind.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// Authenticated publication with explicit original broker chronology when historical.
#[derive(Clone, Debug)]
pub struct VerifiedAuthorizationEvent {
    authority: Arc<VerifiedSessionAuthority>,
    event: SessionEvent,
    original_publication: Option<OriginalEventPublication>,
}

impl VerifiedAuthorizationEvent {
    /// Authenticated publisher authority, not a handle usable for current RPC dispatch.
    pub fn authority(&self) -> &VerifiedSessionAuthority {
        &self.authority
    }

    /// Authenticated descriptor identity and publication subject.
    pub fn event(&self) -> &SessionEvent {
        &self.event
    }

    /// Original broker-assigned chronology supplied by the historical reader.
    pub fn original_publication(&self) -> Option<&OriginalEventPublication> {
        self.original_publication.as_ref()
    }
}

/// Proof and whole-capability verification shared by transport adapters.
/// Material resolution and sticky revocation ownership remain with the runtime owner.
#[derive(Clone, Debug, Default)]
pub struct AuthorizationVerificationCore;

/// Actual request transcript and authenticated catalog for one registered route.
#[derive(Debug)]
pub struct RequestVerificationInput<'a> {
    /// Signature-checked authority retained by the connected runtime owner.
    pub authority: Arc<VerifiedSessionAuthority>,
    /// Transcript assembled from actual transport and generated route metadata.
    pub request: &'a SessionRequest,
    /// Exact received bytes, before decoding or field projection.
    pub payload: &'a [u8],
    /// Received ephemeral-key signature.
    pub proof: &'a SessionRequestProof,
    /// Current clock and bounded authority policy.
    pub policy: &'a SessionAuthorityVerificationPolicy,
    /// Sticky logical-session retirement state from the runtime owner.
    pub known_revoked: bool,
    /// Current accepted catalog, authenticated once when its material was fetched.
    pub catalog: &'a AuthenticatedCatalog,
    /// Catalog signer authenticated through the pinned issuer rotation chain.
    pub catalog_issuer: &'a AuthorityIssuerKey,
    /// Exact action registered by the generated provider descriptor.
    pub action: &'a CatalogActionIdentity,
}

/// Actual event identity and accepted publish action, independent of subject parsing.
#[derive(Debug)]
pub struct EventVerificationInput<'a> {
    /// Signature-checked publisher authority retained by the runtime owner.
    pub authority: Arc<VerifiedSessionAuthority>,
    /// Actual received descriptor identity and publication values.
    pub event: &'a SessionEvent,
    /// Exact publication bytes.
    pub payload: &'a [u8],
    /// Received publisher signature.
    pub proof: &'a SessionEventProof,
    /// Current clock and bounded authority policy.
    pub policy: &'a SessionAuthorityVerificationPolicy,
    /// Sticky retirement state; historical verification also requires a verified cutoff.
    pub known_revoked: bool,
    /// Authenticated catalog containing the exact publish action.
    pub catalog: &'a AuthenticatedCatalog,
    /// Catalog signer authenticated through pinned issuer trust.
    pub catalog_issuer: &'a AuthorityIssuerKey,
    /// Registered event publish action.
    pub action: &'a CatalogActionIdentity,
    /// Original trusted broker metadata, mandatory for historical publisher authority.
    pub original_publication: Option<&'a OriginalEventPublication>,
    /// Exact issuer-signed implementation evidence, when the publisher is a provider.
    pub provider: Option<(
        &'a trellis_protocol::AuthenticatedProviderCertificate,
        &'a AuthorityIssuerKey,
    )>,
}

impl AuthorizationVerificationCore {
    /// Construct the stateless verifier; connected owners supply authenticated material.
    pub fn new() -> Self {
        Self
    }

    /// Authenticate exact bytes and project identity without granting an action.
    /// Handler-authorized routes must separately check their payload-bound rights.
    pub fn verify_possession(
        &self,
        authority: Arc<VerifiedSessionAuthority>,
        request: &SessionRequest,
        payload: &[u8],
        proof: &SessionRequestProof,
        policy: &SessionAuthorityVerificationPolicy,
        known_revoked: bool,
    ) -> Result<VerifiedAuthorizationRequest, AuthorizationVerificationError> {
        let verified = verify_session_request(SessionRequestVerificationInput {
            authority: &authority,
            request,
            raw_payload: payload,
            proof,
            policy,
            known_revoked,
        })?;
        let caller = verified.caller();
        let digest = *verified.digest();
        Ok(VerifiedAuthorizationRequest {
            authority,
            caller,
            request: request.clone(),
            digest,
            authorized_action: None,
            handler_authorized: false,
        })
    }

    /// Authenticate exact request bytes and check current whole-capability membership.
    pub fn verify_request(
        &self,
        input: RequestVerificationInput<'_>,
    ) -> Result<VerifiedAuthorizationRequest, AuthorizationVerificationError> {
        let mut verified = self.verify_possession(
            input.authority.clone(),
            input.request,
            input.payload,
            input.proof,
            input.policy,
            input.known_revoked,
        )?;
        let body = input.authority.authority();
        input.catalog.check(
            input.catalog_issuer,
            &body.trellis_instance_id,
            &body.audience_nats_account,
        )?;
        if input.request.api_id != input.catalog.api_id
            || input.request.action != input.action.name
            || !input
                .catalog
                .authorizes(input.authority.signed(), input.action)
        {
            return Err(AuthorizationVerificationError::PermissionDenied);
        }
        verified.authorized_action = Some(input.action.clone());
        Ok(verified)
    }

    /// Authenticate live or historical event bytes and the exact publish capability.
    /// Consumer read authorization is a separate operation against its current authority.
    pub fn verify_event(
        &self,
        input: EventVerificationInput<'_>,
    ) -> Result<VerifiedAuthorizationEvent, AuthorizationVerificationError> {
        verify_session_event(SessionEventVerificationInput {
            authority: &input.authority,
            event: input.event,
            raw_payload: input.payload,
            proof: input.proof,
            policy: input.policy,
            original_publication: input.original_publication,
            known_revoked: input.known_revoked,
        })?;
        let body = input.authority.authority();
        input.catalog.check(
            input.catalog_issuer,
            &body.trellis_instance_id,
            &body.audience_nats_account,
        )?;
        let mut authorized = input
            .catalog
            .authorizes(input.authority.signed(), input.action);
        if !authorized {
            if let Some((certificate, issuer)) = input.provider {
                let event_time = time::OffsetDateTime::parse(
                    &input.event.event_time,
                    &time::format_description::well_known::Rfc3339,
                )
                .map_err(|_| AuthorizationVerificationError::PermissionDenied)?
                .unix_timestamp();
                authorized = certificate.authorizes_event_publication(
                    issuer,
                    input.authority.signed(),
                    input.catalog,
                    input.action,
                    event_time,
                )?;
                if let Some(publication) = input.original_publication {
                    let published = time::OffsetDateTime::parse(
                        &publication.published_at,
                        &time::format_description::well_known::Rfc3339,
                    )
                    .map_err(|_| AuthorizationVerificationError::PermissionDenied)?
                    .unix_timestamp();
                    authorized &= certificate.authorizes_event_publication(
                        issuer,
                        input.authority.signed(),
                        input.catalog,
                        input.action,
                        published,
                    )?;
                }
            }
        }
        if input.event.api_id != input.catalog.api_id
            || input.event.action != input.action.name
            || input.action.kind != trellis_protocol::ApiSurfaceKind::Event
            || input.action.direction != trellis_protocol::PermissionAction::Publish
            || !authorized
        {
            return Err(AuthorizationVerificationError::PermissionDenied);
        }
        Ok(VerifiedAuthorizationEvent {
            authority: input.authority,
            event: input.event.clone(),
            original_publication: input.original_publication.cloned(),
        })
    }
}
