//! Low-level outbound Trellis runtime primitives for generated Rust code.
//!
//! This module provides connection/auth helpers plus descriptor-driven request
//! and publish operations. It intentionally avoids contract-specific
//! convenience methods so first-party code can move toward generated SDKs and
//! small local wrappers.

mod auth;
pub(crate) mod authorization;
mod connection;
mod error;
mod events;
#[cfg(feature = "http")]
mod http_error;
mod operations;
mod proof;
mod resources;
mod subject;
mod transfer;
mod transport_generations;

pub use auth::SessionAuth;
pub use authorization::{
    canonical_trellis_origin, AuthorizationLease, AuthorizationOwner,
    HistoricalEventVerificationInput, ResponseVerificationInput, RuntimeAuthorizationTrust,
    VerifiedAuthorizationResponse,
};
pub(crate) use authorization::{
    resource_action_marker, ResourceTransportAction, ResourceTransportKind,
};
pub use authorization::{
    AuthorizationContextBundle, AuthorizationContextCache, AuthorizationInstallation,
    AuthorizationNativeTransport, AuthorizationRuntimeBinding, AuthorizationRuntimeTransports,
    AuthorizationVerificationCore, AuthorizationVerificationError, EventVerificationInput,
    RequestVerificationInput, VerifiedAuthorizationEvent, VerifiedAuthorizationRequest,
};
pub use trellis_protocol::transfer::TransferOrigin;
pub use trellis_protocol::AuthenticatedCaller;
pub use trellis_protocol::{ApiSurfaceKind, CatalogActionIdentity, PermissionAction};

pub use crate::generated::{EventDescriptor, LiveDescriptor, RpcDescriptor};
#[cfg(feature = "device")]
pub(crate) use connection::fetch_device_activation;
#[cfg(feature = "device")]
pub(crate) use connection::DeviceEnrollmentResponse;
#[cfg(feature = "native-oauth")]
pub use connection::UserConnectOptions;
pub use connection::{
    DeviceConnectOptions, EventMessage, EventReplayPolicy, EventSubscribeOptions,
    EventSubscriptionMode, ServiceConnectWithContractOptions, TrellisClient,
};
pub use error::{
    AuthenticationError, CallError, ProtocolError, RemoteErrorPayload, RpcErrorPayload,
    ServiceUnavailableError, TransportError, TrellisClientError,
};
pub use events::{
    dispatch_outbox_once, prepare_event, prepare_event_value, EventStoreError, InboxReceipt,
    InboxStore, MemoryInboxStore, MemoryOutboxStore, OutboxDispatchResult, OutboxEventRecord,
    OutboxStore, PreparedTrellisEvent,
};
#[cfg(feature = "event-store-postgres")]
pub use events::{PostgresInboxStore, PostgresOutboxStore};
#[cfg(feature = "event-store-sqlite")]
pub use events::{SqliteInboxStore, SqliteOutboxStore};
#[cfg(any(feature = "device", feature = "telemetry-otlp"))]
pub(crate) use http_error::http_client_builder;
#[cfg(feature = "device")]
pub(crate) use http_error::read_bounded_http_body;
#[cfg(feature = "http")]
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
#[doc(hidden)]
pub use resources::{BoundStateResourceClient, ConnectedStateHandle, StateWatch};
pub use resources::{
    ConsumerDescriptor, ConsumerHandle, RawStateValue, RawStateWriteError, ResourceCodec,
    ResourceCodecError, ResourceRevision, StateHandle, StateReadError, StateResourceClient,
    StateValue, StateWriteError, StateWriteMode,
};
pub(crate) use subject::resolve_subject;
pub use subject::SubjectError;
pub use transfer::{
    download_transfer_grant_from_value, DownloadTransferGrant, FileInfo, TransferCancellation,
    UploadTransferGrant,
};
pub(crate) use transport_generations::{
    GenerationIntake, GenerationIntakeHandle, GenerationIntakeRetireReason, TransportGeneration,
    TransportGenerationManager, TransportLease,
};
