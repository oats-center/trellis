//! Low-level outbound Trellis runtime primitives for generated Rust code.
//!
//! This module provides connection/auth helpers plus descriptor-driven request
//! and publish operations. It intentionally avoids contract-specific
//! convenience methods so first-party code can move toward generated SDKs and
//! small local wrappers.

mod auth;
mod authorization;
mod connection;
mod error;
mod events;
mod http_error;
mod operations;
mod proof;
mod resources;
mod subject;
mod transfer;

pub use auth::SessionAuth;
pub(crate) use authorization::AuthorizationContextLease;
#[cfg(any(test, feature = "runtime-internals"))]
pub use authorization::AuthorizationRegistryBinding;
pub use authorization::{
    canonical_trellis_origin, canonical_trellis_origin_with_insecure, AuthorizationProviderCache,
};
pub use authorization::{
    AuthorizationApiBinding, AuthorizationContextBundle, AuthorizationContextCache,
    AuthorizationContextPolicy, AuthorizationInstallation, AuthorizationNativeTransport,
    AuthorizationRoutingMaterial, AuthorizationRuntimeBinding, AuthorizationRuntimeTransports,
    AuthorizationVerificationCore, AuthorizationVerificationError, EventVerificationInput,
    RequestVerificationInput, VerifiedAuthorizationEvent, VerifiedAuthorizationRequest,
    VerifiedCaller,
};
#[cfg(feature = "runtime-internals")]
pub use authorization::{RuntimeAuthorizationIoCounters, RuntimeAuthorizationTrust};

pub use crate::generated::{EventDescriptor, LiveDescriptor, RpcDescriptor};
pub(crate) use connection::fetch_device_activation;
pub(crate) use connection::DeviceEnrollmentResponse;
pub use connection::{
    DeviceConnectOptions, EventMessage, EventReplayPolicy, EventSubscribeOptions,
    EventSubscriptionMode, ServiceConnectWithContractOptions, TrellisClient, UserConnectOptions,
    UserSessionCredentials,
};
pub use error::{
    AuthenticationError, CallError, ProtocolError, RemoteErrorPayload, RpcErrorPayload,
    TransportError, TrellisClientError,
};
pub use events::{
    dispatch_outbox_once, prepare_event, prepare_event_value, EventStoreError, InboxReceipt,
    InboxStore, MemoryInboxStore, MemoryOutboxStore, OutboxDispatchResult, OutboxEventRecord,
    OutboxStore, PostgresInboxStore, PostgresOutboxStore, PreparedTrellisEvent, SqliteInboxStore,
    SqliteOutboxStore,
};
pub(crate) use http_error::read_bounded_http_body;
pub use http_error::{decode_trellis_http_error, TrellisHttpError};
pub use operations::{
    control_subject, DeclaredOperationUpdates, HasOperationUpdates, NoOperationUpdates,
    OperationDescriptor, OperationEvent, OperationInputBuilder, OperationInvoker, OperationRef,
    OperationRefData, OperationSignalAccepted, OperationSnapshot, OperationState,
    OperationTransferInputBuilder, OperationTransferProgress, OperationTransferReaderInputBuilder,
    OperationTransferStartError, OperationTransport, OperationUpdateEvent, OperationUpdateEvidence,
    StartedOperationTransfer, TransferOperationDescriptor,
};
pub(crate) use proof::{new_request_id, now_iat_seconds};
pub use proof::{verify_event_proof, VerifyEventProofInput};
#[doc(hidden)]
pub use resources::{BoundStateResourceClient, ConnectedStateHandle};
pub use resources::{
    ConsumerDescriptor, ConsumerHandle, RawStateValue, RawStateWriteError, ResourceCodec,
    ResourceCodecError, ResourceRevision, StateHandle, StateReadError, StateResourceClient,
    StateValue, StateWriteError, StateWriteMode,
};
#[cfg(any(test, feature = "runtime-internals"))]
#[doc(hidden)]
pub use subject::resolve_subject;
#[cfg(not(any(test, feature = "runtime-internals")))]
pub(crate) use subject::resolve_subject;
pub use subject::SubjectError;
pub use transfer::{
    download_transfer_grant_from_value, DownloadTransferDirection, DownloadTransferGrant, FileInfo,
    TransferCancellation, TransferGrantType, UploadTransferDirection, UploadTransferGrant,
};
#[cfg(test)]
mod tests;
