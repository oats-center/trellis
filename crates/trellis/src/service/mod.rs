//! Service hosting, generated routing, resources, events, operations, and Jobs.
//!
//! Generated service facades use [`crate::service::ConnectedServiceRuntime`] to own bootstrap,
//! authenticated routing, and lifecycle. Handler registration is descriptor
//! driven; [`crate::service::ServiceHandlerContext`] carries the request and resolved service
//! handles. Runtime-provided [`crate::service::KvHandle`] and
//! [`crate::service::StoreHandle`] values are the
//! supported resource boundary. Jobs are private execution, while operations
//! are caller-visible workflows with progress, updates, cancellation, and named
//! signals.

#[doc(hidden)]
mod authenticated_router;
mod bindings;
#[doc(hidden)]
mod bootstrap_ports;
#[doc(hidden)]
mod core_bootstrap;
mod error;
mod events_runtime;
pub(crate) mod live_router;
mod local_validator;
mod operation_repository;
mod operations;
mod provider_ingress;
#[doc(hidden)]
mod publisher;
#[doc(hidden)]
mod request_loop;
mod resources;
#[doc(hidden)]
mod router;
mod runtime;
mod runtime_facade;
#[doc(hidden)]
pub(crate) mod schema_validation;
mod service_host;
pub(crate) mod transfer;

#[doc(hidden)]
pub use crate::generated::{EventDescriptor, LiveDescriptor, RpcDescriptor};
pub use crate::jobs::{ActiveJob, JobDescriptor, JobRef, JobUpdateDescriptor, JobsError};
#[doc(hidden)]
pub use authenticated_router::{AuthenticatedRouter, RequestValidation, RequestValidator};
pub use bindings::{
    BootstrapBinding, EventConsumerReplay, EventConsumerReplayBinding,
    EventConsumerResourceBinding, JobsQueueResourceBinding, JobsResourceBinding, JobsSchemaRef,
    KvResourceBinding, ServiceResourceBindings, StoreResourceBinding,
};
#[doc(hidden)]
pub use bootstrap_ports::BootstrapBindingInfo;
pub use error::{
    DeclaredRpcError, HandlerResult, SchemaValidationIssue, ServerError, ValidationIssue,
};
pub use events_runtime::{EventsMessageStream, EventsRuntime};
pub(crate) use live_router::LiveProviderOwner;
#[doc(hidden)]
pub use local_validator::{
    payload_hash_base64url, AuthenticatedCaller, EventVerificationFailure, LocalAuthVerifier,
};
pub use operation_repository::{
    operation_invocation_digest, DurableOperationRecord, DurableOperationSignal,
    KvOperationRepository, OperationRepository, OperationTraceCarrier, RevisionedOperationRecord,
    MAX_OPERATION_ERROR_BYTES, MAX_OPERATION_INPUT_BYTES, MAX_OPERATION_OUTPUT_BYTES,
    MAX_OPERATION_PROGRESS_BYTES, MAX_OPERATION_RECORD_BYTES, MAX_OPERATION_SIGNALS,
    MAX_OPERATION_SIGNAL_BYTES,
};
pub use operations::{
    control_subject, AcceptedOperation, OperationCancellation, OperationCancellationReason,
    OperationControl, OperationControlRequest, OperationDescriptor, OperationError,
    OperationFailure, OperationFailureLike, OperationLiveEvent, OperationLiveWatch,
    OperationRefData, OperationSignal, OperationSignalAccepted, OperationSnapshot,
    OperationSnapshotFrame, OperationState, OperationTransferProgress,
};
#[doc(hidden)]
pub use publisher::EventPublisher;
pub(crate) use resources::{open_generated_kv, open_generated_store};
pub use resources::{
    KvHandle, KvResourceClient, KvResourceEntry, KvResourceHandle, KvResourceOperation,
    KvResourceReadError, KvResourceWriteError, StoreHandle, StoreListOptions, StoreListPage,
    StoreObjectInfo, StoreResourceClient, StoreResourceHandle, StoreWaitOptions,
};
#[doc(hidden)]
pub use router::{RequestContext, RoutePermission, Router};
#[doc(hidden)]
pub use runtime_facade::{ConnectedServiceRuntime, CoreBootstrapBinding, ServiceHandle};
pub use runtime_facade::{
    ServiceConnectOptions, ServiceEventListenOptions, ServiceEventListenerContext,
    ServiceEventListenerHandle, ServiceEventListenerMode, ServiceEventPublisherContext,
    ServiceHandlerContext, ServiceLiveHandlerContext, ServiceRuntimeError, DEFAULT_TIMEOUT_MS,
};
#[doc(hidden)]
pub use schema_validation::validate_input_schema;
pub(crate) use service_host::bootstrap_service_host;
#[cfg(test)]
pub(crate) use service_host::ServiceHost;
pub use transfer::{
    plan_download_transfer_grant, plan_upload_transfer_grant, DownloadTransferGrant,
    DownloadTransferGrantPlan, FileTransferInfo, OperationUploadStream, TransferDownloadGrantArgs,
    TransferIdentity, TransferUploadGrantArgs, UploadTransferCompletion, UploadTransferGrant,
    UploadTransferGrantPlan, UploadTransferSession,
};
pub use trellis_protocol::transfer::generate_transfer_id;

/// Runtime-only service support, re-exported through [`crate::__internal`].
pub(crate) mod internal {
    pub use super::request_loop::{
        dispatch_one, encode_error_reply, encode_success_reply, sign_verified_reply,
        HandlerResponse, InboundRequest, OutboundReply, RequestHandler, ResponseStream,
    };
    /// Run a Trellis-owned built-in router through the supplied local request
    /// verifier. Built-in runtimes without a verifier deny all requests
    /// fail-closed; the runtime Auth-side verifier supplies verification in
    /// platform mode.
    pub async fn run_builtin_authenticated_router<V>(
        owner: super::LiveProviderOwner,
        mut router: super::Router,
        validator: V,
    ) -> Result<(), super::ServerError>
    where
        V: super::RequestValidator + Send + Sync + 'static,
    {
        let authority = owner
            .client()
            .authorization_owner()
            .own_authority()
            .map_err(|error| super::ServerError::Nats(error.to_string()))?;
        router.bind_provider_owner(&authority)?;
        let subjects = router.subjects();
        // A live-capable router must be given its connection's provider owner
        // before it serves any traffic; fail loudly here instead of surfacing
        // the mistake to the first caller.
        router.require_live_owner()?;
        let router = std::sync::Arc::new(super::AuthenticatedRouter::new(router, validator));
        super::provider_ingress::run_provider_intake(
            owner.client().transport_generations(),
            subjects.into(),
            router,
        )
        .await
        .map_err(|error| super::ServerError::Nats(error.to_string()))
    }
}
