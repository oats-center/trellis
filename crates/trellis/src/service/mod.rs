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
mod live_router;
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
#[cfg(feature = "runtime-internals")]
pub use live_router::LiveProviderOwner;
#[doc(hidden)]
pub use local_validator::{
    payload_hash_base64url, EventVerificationFailure, LocalAuthVerifier, VerifiedCaller,
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
        let mut bound: Vec<String> = Vec::new();
        for subject in subjects {
            let family = subject.split('.').next().unwrap_or_default();
            let wildcard = subject.ends_with(".>");
            let subject = subject.strip_suffix(".>").unwrap_or(subject);
            // An operation action name may contain dots (`Auth.X.Y`), so it
            // spans every segment after the `operations.v1.<api>` prefix. RPC
            // and Live wildcard entries keep the derived `Route` prefix form.
            let action = if wildcard && family != "operations" {
                "Route"
            } else {
                let logical = subject.splitn(4, '.').nth(3).unwrap_or_default();
                // A live descriptor's logical name is `<ApiShortName>.<Action>`
                // (for example `Health.Watch`); the bound route drops the API
                // short-name group so it matches the client's bound subject.
                if family == "live" {
                    logical
                        .split_once('.')
                        .map_or(logical, |(_, action)| action)
                } else {
                    logical
                }
            };
            match family {
                "rpc" => {
                    let derived =
                        trellis_protocol::derive_bound_rpc_subject(api_id, deployment_id, action)
                            .map_err(|error| super::ServerError::Nats(error.to_string()))?;
                    bound.push(if wildcard {
                        derived
                            .strip_suffix("Route")
                            .expect("derived subject contains action")
                            .to_owned()
                            + ">"
                    } else {
                        derived
                    });
                }
                "live" => {
                    let derived =
                        trellis_protocol::derive_bound_live_subject(api_id, deployment_id, action)
                            .map_err(|error| super::ServerError::Nats(error.to_string()))?;
                    bound.push(if wildcard {
                        derived
                            .strip_suffix("Route")
                            .expect("derived subject contains action")
                            .to_owned()
                            + ">"
                    } else {
                        derived
                    });
                }
                "operations" => {
                    let base = trellis_protocol::derive_bound_operation_subject(
                        api_id,
                        deployment_id,
                        action,
                    )
                    .map_err(|error| super::ServerError::Nats(error.to_string()))?;
                    if wildcard {
                        // The provider transport grants this exact operation
                        // subject, its `.control` lane, and its `.updates.*`
                        // lane; a deployment-wide wildcard would exceed them.
                        bound.push(format!("{base}.control"));
                        bound.push(format!("{base}.updates.*"));
                    } else {
                        bound.push(base);
                    }
                }
                _ => {
                    return Err(super::ServerError::Nats(format!(
                        "unsupported built-in subject {subject}"
                    )))
                }
            }
        }
        let subjects = bound.iter().map(String::as_str).collect::<Vec<_>>();
        // A live-capable router must be given its connection's provider owner
        // before it serves any traffic; fail loudly here instead of surfacing
        // the mistake to the first caller.
        router.require_live_owner()?;
        let router = super::AuthenticatedRouter::new(router, validator);
        super::runtime::run_multi_subject_service(nats, &subjects, router).await
    }
}
