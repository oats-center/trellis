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
mod local_validator;
mod operation_repository;
mod operations;
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
mod schema_validation;
mod service_host;
mod transfer;
pub(crate) use transfer::transfer_frame_proof_payload;

#[doc(hidden)]
pub use crate::generated::{EventDescriptor, FeedDescriptor, RpcDescriptor};
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
#[doc(hidden)]
pub use local_validator::{
    payload_hash_base64url, EventVerificationFailure, LocalAuthVerifier, VerifiedCaller,
};
pub use operation_repository::{
    operation_invocation_digest, DurableOperationRecord, DurableOperationSignal,
    KvOperationRepository, OperationRepository, RevisionedOperationRecord,
    MAX_OPERATION_ERROR_BYTES, MAX_OPERATION_INPUT_BYTES, MAX_OPERATION_OUTPUT_BYTES,
    MAX_OPERATION_PROGRESS_BYTES, MAX_OPERATION_RECORD_BYTES, MAX_OPERATION_SIGNALS,
    MAX_OPERATION_SIGNAL_BYTES,
};
pub use operations::{
    control_subject, AcceptedOperation, OperationControl, OperationControlRequest,
    OperationDescriptor, OperationError, OperationFailure, OperationFailureLike,
    OperationLiveEvent, OperationLiveWatch, OperationRefData, OperationSignal,
    OperationSignalAccepted, OperationSnapshot, OperationSnapshotFrame, OperationState,
    OperationTransferProgress,
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
    ServiceHandlerContext, ServiceRuntimeError, DEFAULT_TIMEOUT_MS,
};
#[doc(hidden)]
pub use schema_validation::validate_input_schema;
pub(crate) use service_host::bootstrap_service_host;
#[cfg(test)]
pub(crate) use service_host::ServiceHost;
pub use transfer::{
    decode_upload_transfer_chunk, plan_download_transfer_grant, plan_upload_transfer_grant,
    DownloadTransferGrant, DownloadTransferGrantPlan, FileTransferInfo, TransferDownloadGrantArgs,
    TransferUploadGrantArgs, UploadTransferAck, UploadTransferChunk, UploadTransferCompletion,
    UploadTransferGrant, UploadTransferGrantPlan, UploadTransferSession, TRANSFER_EOF_HEADER,
    TRANSFER_SEQUENCE_HEADER,
};

#[doc(hidden)]
pub mod internal {
    #[cfg(feature = "runtime-internals")]
    pub use super::operations::OperationHandlerRuntime;
    pub use super::request_loop::{
        dispatch_one, encode_error_reply, encode_success_reply, HandlerResponse, InboundRequest,
        OutboundReply, RequestHandler, ResponseStream,
    };
    #[cfg(feature = "runtime-internals")]
    pub use super::resources::backend::BoundStoreResourceClient;

    /// Run a Trellis-owned built-in router through the supplied local request
    /// verifier. Built-in runtimes without a verifier deny all requests
    /// fail-closed; the runtime Auth-side verifier supplies verification in
    /// platform mode.
    #[cfg(feature = "runtime-internals")]
    pub async fn run_builtin_authenticated_router<V>(
        nats: async_nats::Client,
        api_id: &str,
        subjects: &[&str],
        router: super::Router,
        validator: V,
    ) -> Result<(), super::ServerError>
    where
        V: super::RequestValidator + Send + Sync + 'static,
    {
        let deployment_id = match api_id {
            "trellis.auth@v1" | "trellis.core@v1" | "trellis.state@v1" => {
                "dep_trellis_auth_runtime"
            }
            "trellis.events@v1" => "dep_trellis_events_runtime",
            "trellis.health@v1" => "dep_trellis_health_runtime",
            "trellis.jobs@v1" => "dep_trellis_jobs_runtime",
            _ => {
                return Err(super::ServerError::Nats(format!(
                    "unknown built-in API {api_id}"
                )))
            }
        };
        let bound = subjects
            .iter()
            .map(|subject| {
                let family = subject.split('.').next().unwrap_or_default();
                let wildcard = subject.ends_with(".>");
                let subject = subject.strip_suffix(".>").unwrap_or(subject);
                let action = if wildcard {
                    "Route"
                } else if family == "operations" {
                    subject.splitn(5, '.').nth(4).unwrap_or_default()
                } else {
                    subject.splitn(4, '.').nth(3).unwrap_or_default()
                };
                match family {
                    "rpc" => {
                        trellis_protocol::derive_bound_rpc_subject(api_id, deployment_id, action)
                    }
                    "feed" => {
                        trellis_protocol::derive_bound_feed_subject(api_id, deployment_id, action)
                    }
                    "operations" => trellis_protocol::derive_bound_operation_subject(
                        api_id,
                        deployment_id,
                        action,
                    ),
                    _ => {
                        return Err(super::ServerError::Nats(format!(
                            "unsupported built-in subject {subject}"
                        )))
                    }
                }
                .map(|subject| {
                    if wildcard {
                        subject
                            .strip_suffix("Route")
                            .expect("derived subject contains action")
                            .to_owned()
                            + ">"
                    } else {
                        subject
                    }
                })
                .map_err(|error| super::ServerError::Nats(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let subjects = bound.iter().map(String::as_str).collect::<Vec<_>>();
        let router = super::AuthenticatedRouter::new(router, validator);
        super::runtime::run_multi_subject_service(nats, &subjects, router).await
    }
}
