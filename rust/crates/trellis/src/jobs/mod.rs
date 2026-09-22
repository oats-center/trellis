//! Service-local jobs runtime building blocks for Trellis.
//!
//! This module contains the handwritten Rust support code that sits around the
//! generated contract SDKs: binding parsing, event helpers, the projection
//! reducer, worker-loop glue, and service-instance registration.

use std::future::Future;

use serde_json::Value;

use crate::client::TrellisClient;

pub mod active_job;
pub mod api;
pub mod bindings;
/// Constructors for Jobs lifecycle events.
pub mod events;
#[doc(hidden)]
pub mod keys;
pub mod manager;
pub mod projection;
pub mod publisher;
pub mod registry;
pub mod runtime;
#[doc(hidden)]
pub mod runtime_ref;
mod runtime_worker;
pub mod subjects;
pub mod types;
pub mod updates;

pub use active_job::ActiveJob as WorkerActiveJob;
pub use api::{
    ActiveJob, JobFilter, JobIdentity, JobNotEnqueued, JobNotEnqueuedReason, JobQueue, JobRef,
    JobSnapshot, JobSubmitOutcome, JobWorkerHost, JobsError, JobsFacade, JobsService, TerminalJob,
};
pub use bindings::{JobsBinding, JobsQueueBinding, JobsRuntimeBinding};
pub use keys::{derive_key, job_key, key_hash, worker_presence_key, KeyDerivationError};
pub use manager::{
    JobManager, JobManagerError, JobMetaSource, JobProcessError, JobProcessOutcome,
    TrellisJobMetaSource,
};
pub use projection::{is_terminal, job_from_work_event, reduce_job_event};
pub use publisher::{JobEventHeaders, JobEventPublisher};
pub use registry::{
    new_worker_heartbeat, publish_worker_heartbeat, start_worker_heartbeat_loop,
    ActiveJobCancellationRegistry, WorkerHeartbeatHandle, WorkerHeartbeatOptions,
};
pub use runtime::{JobsRuntime, JobsRuntimeMessage, JobsRuntimeMessageStream};
pub use runtime_worker::{JobCancellationToken, WorkerHostHandle, WorkerHostOptions};
pub use subjects::{job_event_subject, worker_heartbeat_subject, WORKER_HEARTBEATS_WILDCARD};
pub use types::{
    Job, JobAdminAction, JobConcurrency, JobContext, JobEvent, JobEventType, JobLogEntry,
    JobLogLevel, JobProgress, JobQueuePolicy, JobQueuePolicyOutcome, JobState, JobWaitEdge,
    JobWaitTarget, JobWaitTargetKind, WorkerHeartbeat,
};
pub use updates::{JobDescriptor, JobUpdate, JobUpdateDescriptor, JobUpdateError};

#[doc(hidden)]
pub type NatsJobEventPublisher = TrellisJobEventPublisher;

#[doc(hidden)]
pub mod internal {
    pub use super::runtime_worker::{
        process_work_payload, start_worker_host_from_binding, WorkerHostError,
    };

    pub fn typed_active_job<D>(
        active: super::WorkerActiveJob<
            super::TrellisJobEventPublisher,
            super::TrellisJobMetaSource,
        >,
    ) -> Result<super::ActiveJob<D::Payload, D::Result>, super::JobsError>
    where
        D: super::JobDescriptor,
    {
        let payload = serde_json::from_value(active.job().payload.clone())
            .map_err(super::JobsError::DecodePayload)?;
        Ok(super::ActiveJob::from_runtime(payload, active))
    }
}

#[derive(Debug, Clone)]
pub struct TrellisJobEventPublisher {
    nats: async_nats::Client,
}

impl TrellisJobEventPublisher {
    #[doc(hidden)]
    pub fn new(nats: async_nats::Client) -> Self {
        Self { nats }
    }
}

impl JobEventPublisher for TrellisJobEventPublisher {
    type Error = String;

    fn publish(
        &self,
        subject: String,
        headers: JobEventHeaders,
        payload: Vec<u8>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let nats = self.nats.clone();
        async move {
            let mut nats_headers = async_nats::HeaderMap::new();
            nats_headers.insert("request-id", headers.request_id.as_str());
            nats_headers.insert("traceparent", headers.traceparent.as_str());
            if let Some(tracestate) = headers.tracestate.as_deref() {
                nats_headers.insert("tracestate", tracestate);
            }
            nats.publish_with_headers(subject, nats_headers, payload.into())
                .await
                .map_err(|error| error.to_string())
        }
    }
}

/// Start a service-private job worker host using a connected Trellis client.
pub(crate) async fn start_worker_host_from_client<MF, M, H, Fut, E>(
    client: &TrellisClient,
    binding: JobsRuntimeBinding,
    instance_id: String,
    meta_factory: MF,
    handler: H,
    options: WorkerHostOptions,
) -> Result<WorkerHostHandle, runtime_worker::WorkerHostError>
where
    MF: Fn(&str, u32) -> M + Clone + Send + Sync + 'static,
    M: JobMetaSource + Send + Sync + 'static,
    H: Fn(active_job::ActiveJob<TrellisJobEventPublisher, M>) -> Fut
        + Clone
        + Send
        + Sync
        + 'static,
    Fut: Future<Output = Result<Value, JobProcessError<E>>> + Send + 'static,
    E: ToString + Send + 'static,
{
    let nats = client.nats().clone();
    runtime_worker::start_worker_host_from_binding(
        nats.clone(),
        binding,
        instance_id,
        move || TrellisJobEventPublisher { nats: nats.clone() },
        meta_factory,
        handler,
        options,
    )
    .await
}
