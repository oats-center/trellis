use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use trellis_protocol::{
    verify_authorization_event, verify_authorization_request, AuthorizationEventProof,
    AuthorizationEventPublisher, AuthorizationEventVerificationInput, AuthorizationRequestProof,
    AuthorizationRequestVerificationInput, AuthorizationVerificationPolicy, PermissionAtom,
    ProtocolError, VerifiedAuthorizationContext, VerifiedAuthorizationEventProof,
    VerifiedAuthorizationRequestProof,
};

/// Typed caller projection produced after a local authorization proof verifies.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct VerifiedCaller {
    /// Session public key presented with the request.
    pub session_key: String,
    /// Signed reply-inbox prefix bound to the session.
    pub inbox_prefix: String,
    /// Digest of the complete signed authorization context.
    pub context_digest: String,
    /// Signed SDK-owned live connection identity.
    pub connection_id: String,
    /// Durable login identity for user connections only.
    pub login_session_id: Option<String>,
    /// Signed principal identity.
    pub principal_id: String,
    /// Signed principal class.
    pub principal_kind: trellis_protocol::AuthorizationPrincipalKind,
    /// Installed participant assignment.
    pub participant_id: String,
    /// Server-assigned meta-authority, independent of ordinary permission atoms.
    pub platform_privileges: Vec<trellis_protocol::PlatformPrivilege>,
    /// Signed deployment identity, when present.
    pub deployment_id: Option<String>,
    /// Signed runtime instance identity, when present.
    pub instance_id: Option<String>,
}

/// Failure returned by the shared local authorization verification core.
#[derive(Debug, thiserror::Error)]
pub enum AuthorizationVerificationError {
    /// The presented digest does not identify the verified context.
    #[error("authorization context digest does not match verified context")]
    ContextDigestMismatch,
    /// The protocol proof, permission, or validity check failed.
    #[error("authorization proof rejected: {0}")]
    Protocol(Box<ProtocolError>),
}

impl From<ProtocolError> for AuthorizationVerificationError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(Box::new(error))
    }
}

/// Verified request proof and its local caller projection.
#[derive(Clone, Debug)]
pub struct VerifiedAuthorizationRequest {
    request: VerifiedAuthorizationRequestProof,
    caller: VerifiedCaller,
}

impl VerifiedAuthorizationRequest {
    /// Return the verified caller projection.
    pub fn caller(&self) -> &VerifiedCaller {
        &self.caller
    }

    /// Return the verified signed authorization context.
    pub fn context(&self) -> &VerifiedAuthorizationContext {
        self.request.context()
    }
}

/// Verified event proof and its protocol publisher projection.
#[derive(Clone, Debug)]
pub struct VerifiedAuthorizationEvent {
    event: VerifiedAuthorizationEventProof,
}

impl VerifiedAuthorizationEvent {
    /// Return the verified publisher projection.
    pub fn publisher(&self) -> &AuthorizationEventPublisher {
        self.event.publisher()
    }

    /// Return the verified signed authorization context.
    pub fn context(&self) -> &VerifiedAuthorizationContext {
        self.event.context()
    }
}

/// Shared local verifier used by both the platform and connected Rust SDKs.
///
/// Resolution of trust/context records stays with each provider cache. Once a
/// verified context is available, this core owns proof parsing, exact
/// permission checks, and caller projection.
#[derive(Clone, Debug, Default)]
pub struct AuthorizationVerificationCore {}

/// Borrowed transport and authority inputs for shared request verification.
#[derive(Clone, Copy, Debug)]
pub struct RequestVerificationInput<'a> {
    /// Resolved and cryptographically verified authorization context.
    pub context: &'a VerifiedAuthorizationContext,
    /// Context digest presented by the transport.
    pub context_digest: &'a str,
    /// Exact routed subject.
    pub subject: &'a str,
    /// Exact received payload bytes.
    pub payload: &'a [u8],
    /// Signed proof issue time.
    pub iat: i64,
    /// Signed request identifier.
    pub request_id: &'a str,
    /// Actual reply subject.
    pub reply_subject: Option<&'a str>,
    /// Encoded request proof.
    pub proof: &'a str,
    /// Verification policy.
    pub policy: &'a AuthorizationVerificationPolicy,
    /// Required exact permissions.
    pub required_permissions: &'a [PermissionAtom],
}

/// Borrowed transport and authority inputs for shared event verification.
#[derive(Clone, Copy, Debug)]
pub struct EventVerificationInput<'a> {
    /// Resolved and cryptographically verified authorization context.
    pub context: &'a VerifiedAuthorizationContext,
    /// Context digest presented by the transport.
    pub context_digest: &'a str,
    /// Exact published subject.
    pub subject: &'a str,
    /// Signed canonical event descriptor identity.
    pub descriptor_identity: &'a str,
    /// Exact received payload bytes.
    pub payload: &'a [u8],
    /// Signed event identifier.
    pub event_id: &'a str,
    /// Signed event time.
    pub event_time: &'a str,
    /// Encoded event proof.
    pub proof: &'a str,
    /// Verification policy.
    pub policy: &'a AuthorizationVerificationPolicy,
    /// Context revocation time, when present.
    pub revoked_at: Option<i64>,
}

impl AuthorizationVerificationCore {
    /// Create an empty local verification core.
    pub fn new() -> Self {
        Self::default()
    }

    /// Verify one context-bound request proof.
    pub fn verify_request(
        &self,
        input: RequestVerificationInput<'_>,
    ) -> Result<VerifiedAuthorizationRequest, AuthorizationVerificationError> {
        let RequestVerificationInput {
            context,
            context_digest,
            subject,
            payload,
            iat,
            request_id,
            reply_subject,
            proof,
            policy,
            required_permissions,
        } = input;
        self.check_context_binding(context, context_digest)?;
        let proof = AuthorizationRequestProof::parse(proof.to_owned())?;
        let request = verify_authorization_request(AuthorizationRequestVerificationInput {
            context,
            subject,
            reply_subject,
            raw_payload: payload,
            iat,
            request_id,
            proof: &proof,
            policy,
            required_permissions,
        })?;
        Ok(VerifiedAuthorizationRequest {
            caller: project_caller(request.context()),
            request,
        })
    }

    /// Verify one context-bound event proof.
    pub fn verify_event(
        &self,
        input: EventVerificationInput<'_>,
    ) -> Result<VerifiedAuthorizationEvent, AuthorizationVerificationError> {
        let EventVerificationInput {
            context,
            context_digest,
            subject,
            descriptor_identity,
            payload,
            event_id,
            event_time,
            proof,
            policy,
            revoked_at,
        } = input;
        self.check_context_binding(context, context_digest)?;
        let proof = AuthorizationEventProof::parse(proof.to_owned())?;
        let event = verify_authorization_event(AuthorizationEventVerificationInput {
            context,
            subject,
            descriptor_identity,
            raw_payload: payload,
            event_id,
            event_time,
            proof: &proof,
            policy,
            revoked_at,
        })?;
        Ok(VerifiedAuthorizationEvent { event })
    }

    fn check_context_binding(
        &self,
        context: &VerifiedAuthorizationContext,
        context_digest: &str,
    ) -> Result<(), AuthorizationVerificationError> {
        if context.context_digest() != context_digest {
            return Err(AuthorizationVerificationError::ContextDigestMismatch);
        }
        Ok(())
    }
}

fn project_caller(context: &VerifiedAuthorizationContext) -> VerifiedCaller {
    VerifiedCaller {
        session_key: URL_SAFE_NO_PAD.encode(context.session_key()),
        inbox_prefix: context.inbox_prefix().to_owned(),
        context_digest: context.context_digest().to_owned(),
        connection_id: context.connection_id().to_owned(),
        login_session_id: context.login_session_id().map(ToOwned::to_owned),
        principal_id: context.principal_id().to_owned(),
        principal_kind: context.principal_kind(),
        participant_id: context.participant_id().to_owned(),
        platform_privileges: context.platform_privileges().to_vec(),
        deployment_id: context.deployment_id().map(ToOwned::to_owned),
        instance_id: context.instance_id().map(ToOwned::to_owned),
    }
}
