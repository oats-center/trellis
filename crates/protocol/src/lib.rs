//! Canonical proof, permission, identifier, subject, and wire primitives shared by
//! Trellis runtimes and language bindings.
//!
//! Semantic contract authoring, resolution, compatibility, and package digests belong
//! to `trellis-idl`. This crate owns only runtime protocol values whose identity and
//! verification must agree across implementations.

pub mod authorization;
mod canonical;
mod catalog;
mod error;
mod identifiers;
pub mod live;
mod pagination;
mod participant;
mod permissions;
mod session_proof;
mod subjects;
pub mod transfer;
pub mod transport_authorization;

pub use authorization::{
    parse_session_authority, session_event_signing_digest, session_request_signing_digest,
    sign_session_authority, sign_session_event, sign_session_request, verify_session_authority,
    verify_session_event, verify_session_request, AuthenticatedCaller, AuthorityApi,
    AuthorityIssuerKey, AuthorityIssuerState, AuthorityProvider, CapabilityAuthority,
    CredentialKind, OriginalEventPublication, PrincipalKind, SessionAuthorityPurpose,
    SessionAuthorityVerificationInput, SessionAuthorityVerificationPolicy, SessionBinding,
    SessionEvent, SessionEventProof, SessionEventVerificationInput, SessionRequest,
    SessionRequestProof, SessionRequestVerificationInput, SignedSessionAuthority,
    UnsignedSessionAuthority, VerifiedSessionAuthority, VerifiedSessionEvent,
    VerifiedSessionRequest, ADMISSION_GET_DOMAIN_V1, ISSUER_METADATA_DOMAIN_V1,
    LIVE_PROOF_DOMAIN_V1, MESSAGE_EVENT_DOMAIN_V1, MESSAGE_REQUEST_DOMAIN_V1,
    MESSAGE_RESPONSE_DOMAIN_V1, SESSION_AUTHORITY_FORMAT_V1, TRANSFER_PROOF_DOMAIN_V1,
};
pub use canonical::{canonicalize_json, digest_json, sha256_base64url};
pub use catalog::{
    CatalogAction, CatalogActionIdentity, CatalogCapability, CatalogCapabilityMembership,
    SignedCatalogSnapshot, SignedIssuerRotation, SignedProviderCertificate,
    SignedSessionRevocation, CATALOG_SNAPSHOT_FORMAT_V1, ISSUER_ROTATION_FORMAT_V1,
    PROVIDER_CERTIFICATE_FORMAT_V1, SESSION_REVOCATION_FORMAT_V1,
};
pub use error::{AuthorizationErrorCode, ProtocolError, SessionProofErrorCode};
pub use identifiers::validate_api_id;
pub use live::{
    build_live_server_proof_input, derive_live_data_subject, derive_live_observe_subject,
    derive_live_observe_wildcard_subject, generate_nonce, live_server_proof_digest,
    logical_control_hash, logical_open_hash, negotiate_max_data_body_bytes, parse_live_control,
    parse_live_frame, parse_nonce, parse_u64s, sign_live_server_proof, u64s, validate_control_body,
    validate_live_subject, validate_open_body, validate_subject_token, verify_live_server_proof,
    verify_live_server_proof_encoded, AckAction, ActivateAction, CleanupStatus, CloseAction,
    ConsumerCloseReason, EndAckAction, LiveChallengeFrame, LiveControl, LiveControlAck,
    LiveControlAckAction, LiveControlActivate, LiveControlBase, LiveControlClose,
    LiveControlCredit, LiveControlEndAck, LiveControlError, LiveControlKind, LiveControlPulse,
    LiveDataFrame, LiveEndFrame, LiveEndReason, LiveErrorCode, LiveFrame, LiveOffer,
    LiveOfferConsumer, LiveOfferKind, LiveOfferLimits, LiveOfferProvider, LiveOpen, LiveOpenError,
    LiveOpenKind, LiveProtocolError, LiveProtocolErrorCode, LiveServerProof, LiveServerProofInput,
    LiveSessionKind, LiveSessionState, LogicalOpenIdentity, OperationWatchOpen, PulseAction, U64s,
    WireTerminal, WireTerminalError, ACK_FRAME_THRESHOLD, ACK_MAX_DELAY_MS, CHALLENGE_RETRY_MS,
    CLEANUP_GRACE_MS, CLOSE_EXCHANGE_MS, CLOSE_RETRY_MS, CONSUMER_STALL_MS, CONTROL_TIMEOUT_MS,
    HEARTBEAT_INTERVAL_MS, LIVE_SERVER_PROOF_DOMAIN_V1, LIVE_VERSION, MAX_CONSUMER_SESSIONS,
    MAX_CONTROL_BODY_BYTES, MAX_CONTROL_VERIFY_INFLIGHT, MAX_CONTROL_VERIFY_QUEUE,
    MAX_OPEN_BODY_BYTES, MAX_OPEN_VERIFY_INFLIGHT, MAX_OPEN_VERIFY_QUEUE, MAX_PROVIDER_SESSIONS,
    MAX_PROVIDER_SESSIONS_PER_CALLER, MAX_TOMBSTONES, MIN_DATA_BODY_BYTES, OPEN_RESERVATION_MS,
    PEER_INACTIVITY_MS, PROTOCOL_HEADER_RESERVE_BYTES, TOMBSTONE_MS, WINDOW_BYTES, WINDOW_FRAMES,
};
pub use pagination::{
    decode_pagination_cursor, encode_pagination_cursor, pagination_query_digest, InvalidPagination,
};
pub use participant::ParticipantKind;
pub use permissions::{
    ApiSurfaceKind, ParticipantResourceKind, PermissionAction, PermissionAtom, PermissionTarget,
    PlatformPrivilege,
};
pub use session_proof::{
    parse_session_proof, session_proof_request_digest, session_proof_signing_digest,
    sign_session_proof, verify_session_proof, AuthorizationContextRefreshSessionProofInput,
    NativeBootstrapSessionProofInput, SessionProof, SessionProofInput, SessionProofPolicy,
    SessionProofPurpose, UserAuthBindSessionProofInput, UserAuthRequestSessionProofInput,
    SESSION_PROOF_FORMAT_V1,
};
pub use subjects::{
    decode_event_descriptor_identity, derive_bound_live_subject, derive_bound_operation_subject,
    derive_bound_rpc_subject, derive_event_subject, derive_event_wildcard_subject,
    derive_live_control_subject, derive_live_instance_control_subject, derive_live_subject,
    derive_operation_subject, derive_rpc_subject, encode_event_descriptor_identity,
    encode_subject_token, event_patterns_overlap, route_queue_group,
    validate_event_descriptor_subject, DerivedApiSubjects, DerivedEventSubjects,
    EventDescriptorIdentity,
};
pub use transport_authorization::{
    subject_union_covered_by, TransportAuthorizationV1, TransportPolicyClass,
    TransportResponseAuthorizationV1, TRANSPORT_AUTHORIZATION_FORMAT_V1,
};
