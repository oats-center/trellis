//! Canonical proof, permission, identifier, subject, and wire primitives shared by
//! Trellis runtimes and language bindings.
//!
//! Semantic contract authoring, resolution, compatibility, and package digests belong
//! to `trellis-idl`. This crate owns only runtime protocol values whose identity and
//! verification must agree across implementations.

pub mod authorization;
mod canonical;
mod error;
mod identifiers;
mod pagination;
mod participant;
mod permissions;
mod session_proof;
mod subjects;

pub use authorization::{
    authorization_context_refresh_at, authorization_context_signing_digest,
    build_authorization_event_proof_input, build_authorization_request_proof_input,
    parse_authorization_context, sign_authorization_context, sign_authorization_event,
    sign_authorization_request, verify_authorization_context, verify_authorization_event,
    verify_authorization_request, AuthorizationContextPurpose, AuthorizationEventProof,
    AuthorizationEventProofInput, AuthorizationEventPublisher, AuthorizationEventVerificationInput,
    AuthorizationIssuerKey, AuthorizationIssuerState, AuthorizationPrincipalKind,
    AuthorizationRequestProof, AuthorizationRequestProofInput,
    AuthorizationRequestVerificationInput, AuthorizationVerificationPolicy,
    SignedAuthorizationContext, UnsignedAuthorizationContext, VerifiedAuthorizationContext,
    VerifiedAuthorizationEventProof, VerifiedAuthorizationRequestProof,
    AUTHORIZATION_CONTEXT_FORMAT_V1, AUTHORIZATION_EVENT_PROOF_DOMAIN_V1,
    AUTHORIZATION_REQUEST_PROOF_DOMAIN_V1,
};
pub use canonical::{canonicalize_json, digest_json, sha256_base64url};
pub use error::{AuthorizationErrorCode, ProtocolError, SessionProofErrorCode};
pub use identifiers::validate_api_id;
pub use pagination::{
    decode_pagination_cursor, encode_pagination_cursor, pagination_query_digest, InvalidPagination,
};
pub use participant::ParticipantKind;
pub use permissions::{
    ApiSurfaceKind, CapabilityDefinition, ConsentMetadata, GrantOwnerKind, GrantSet,
    ParticipantResourceKind, PermissionAction, PermissionAtom, PermissionTarget, PlatformPrivilege,
    GRANT_SET_FORMAT_V1,
};
pub use session_proof::{
    parse_session_proof, session_proof_request_digest, session_proof_signing_digest,
    sign_session_proof, verify_session_proof, AuthorizationContextRefreshSessionProofInput,
    NativeBootstrapSessionProofInput, SessionProof, SessionProofInput, SessionProofPolicy,
    SessionProofPurpose, UserAuthBindSessionProofInput, UserAuthRequestSessionProofInput,
    SESSION_PROOF_FORMAT_V1,
};
pub use subjects::{
    decode_event_descriptor_identity, derive_bound_feed_subject, derive_bound_operation_subject,
    derive_bound_rpc_subject, derive_event_subject, derive_event_wildcard_subject,
    derive_feed_control_subject, derive_feed_instance_control_subject, derive_feed_subject,
    derive_operation_subject, derive_rpc_subject, encode_event_descriptor_identity,
    event_patterns_overlap, route_queue_group, validate_event_descriptor_subject,
    DerivedApiSubjects, DerivedEventSubjects, EventDescriptorIdentity,
};
