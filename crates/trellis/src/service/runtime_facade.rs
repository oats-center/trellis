//! High-level Trellis service runtime facade for generated Rust services.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_nats::header::HeaderMap;
use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::{Stream, StreamExt};
use opentelemetry::trace::{FutureExt as _, TraceContextExt as _};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use trellis_protocol::event_patterns_overlap;

pub use super::core_bootstrap::CoreBootstrapBinding;
use super::resources::{validate_kv_binding, validate_store_binding, ResourceRuntimeClient};
use super::resources::{KvHandle, KvResourceHandle, StoreHandle, StoreResourceHandle};
use super::runtime::run_multi_subject_service;
use super::transfer::{
    spawn_download_transfer_endpoint, spawn_upload_transfer_endpoint_with_completion,
    spawn_upload_transfer_endpoint_with_progress,
};
use super::{
    bootstrap_service_host, control_subject, BootstrapBindingInfo, DownloadTransferGrantPlan,
    EventPublisher, HandlerResult, JobsResourceBinding, KvResourceBinding, LiveDescriptor,
    OperationControl, OperationDescriptor, OperationTransferProgress, RequestContext, Router,
    RpcDescriptor, ServerError, ServiceResourceBindings, StoreResourceBinding, StoreResourceClient,
    UploadTransferCompletion, UploadTransferSession,
};

use crate::client::{
    EventReplayPolicy, EventSubscribeOptions, EventSubscriptionMode,
    ServiceConnectWithContractOptions, TrellisClient, TrellisClientError,
};
use crate::jobs::{
    start_worker_host_from_client, JobDescriptor, JobManager, JobProcessError, JobRef, JobsError,
    TrellisJobEventPublisher, TrellisJobMetaSource, WorkerHostHandle, WorkerHostOptions,
};
use crate::service::local_validator::LocalAuthVerifier;

const DURABLE_EVENT_CONSUMER_RETRY_MS: u64 = 100;
static SERVICE_EVENT_HANDLER_ID: AtomicU64 = AtomicU64::new(1);

type SharedDurableEventListeners =
    Arc<StdMutex<BTreeMap<DurableEventListenerKey, SharedDurableEventListener>>>;
type SharedEventHandler = Arc<
    dyn Fn(
            Bytes,
            ServiceEventListenerContext,
        ) -> BoxFuture<'static, Result<(), ServiceRuntimeError>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DurableEventListenerKey {
    stream: String,
    durable_name: String,
}

struct SharedDurableEventListener {
    registrations: BTreeMap<String, EventRegistration>,
    concurrency: u32,
    pull_abort_handles: Vec<AbortHandle>,
}

#[derive(Clone)]
struct EventRegistration {
    event_api_id: String,
    event_name: String,
    descriptor_identity: String,
    handlers: BTreeMap<u64, SharedEventHandler>,
}

struct DurableEventPullConfig {
    key: DurableEventListenerKey,
    subscribe_options: EventSubscribeOptions,
    replay_subscribe_options: EventSubscribeOptions,
    context: ServiceEventListenerContext,
    ack_wait: Duration,
    backoff: Vec<Duration>,
    max_deliver: u64,
    resource_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsumerReplayEnvelope {
    dead_letter_id: String,
    generation: u64,
    resource_id: String,
    original_record_sequence: u64,
    original_subject: String,
    original_payload_bytes: Vec<u8>,
    original_headers: BTreeMap<String, Vec<String>>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsumerDeliveryReport {
    resource_id: String,
    source_stream: String,
    source_sequence: String,
    delivery_count: u64,
    delivery_proof: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    replay_generation: Option<u64>,
    outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct ConsumerReportDelivery;
struct ConsumerDeadLetterInspect;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsumerDeadLetterInspectInput {
    resource_id: String,
    dead_letter_id: String,
}

impl crate::generated::RpcDescriptor for ConsumerReportDelivery {
    type Input = ConsumerDeliveryReport;
    type Output = serde_json::Value;
    type Error = serde_json::Value;

    const API_ID: &'static str = "trellis.events@v1";
    const DESCRIPTOR_NAME: &'static str = "rpc:Consumers.ReportDelivery";
    const SUBJECT: &'static str = "";
    const KEY: &'static str = "events.Consumers.ReportDelivery";
    const CALLER_CAPABILITIES: &'static [&'static str] = &[];
    const DOWNLOAD: bool = false;

    fn decode_error(value: serde_json::Value) -> Result<Option<Self::Error>, serde_json::Error> {
        Ok(Some(value))
    }
}

impl crate::generated::RpcDescriptor for ConsumerDeadLetterInspect {
    type Input = ConsumerDeadLetterInspectInput;
    type Output = serde_json::Value;
    type Error = serde_json::Value;

    const API_ID: &'static str = "trellis.events@v1";
    const DESCRIPTOR_NAME: &'static str = "rpc:DeadLetters.Inspect";
    const SUBJECT: &'static str = "";
    const KEY: &'static str = "events.DeadLetters.Inspect";
    const CALLER_CAPABILITIES: &'static [&'static str] = &[];
    const DOWNLOAD: bool = false;

    fn decode_error(value: serde_json::Value) -> Result<Option<Self::Error>, serde_json::Error> {
        Ok(Some(value))
    }
}

#[derive(Clone)]
struct ServiceEventListenerRegistration {
    event_listeners: SharedDurableEventListeners,
    key: DurableEventListenerKey,
    subject: String,
    handler_id: u64,
}

struct ServiceEventListenerRegistryCleanup {
    event_listeners: SharedDurableEventListeners,
}

impl ServiceEventListenerRegistryCleanup {
    fn new(event_listeners: SharedDurableEventListeners) -> Self {
        Self { event_listeners }
    }
}

impl Drop for ServiceEventListenerRegistryCleanup {
    fn drop(&mut self) {
        remove_service_event_listeners(&self.event_listeners);
    }
}

/// Default request/connect timeout for service bootstrap and NATS RPC calls.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// High-level options for connecting a generated Rust service runtime.
#[derive(Clone)]
pub struct ServiceConnectOptions<'a> {
    /// Base Trellis runtime URL used for HTTP bootstrap.
    trellis_url: &'a str,
    /// Optional display metadata; never an assignment or authorization identity.
    name: Option<&'a str>,
    /// Base64url-encoded provisioned service identity seed.
    provisioned_identity_seed_base64url: &'a str,
    /// Request/connect timeout in milliseconds.
    timeout_ms: u64,
}

impl<'a> ServiceConnectOptions<'a> {
    /// Create service connect options with ergonomic default timeouts.
    pub fn new(trellis_url: &'a str, provisioned_identity_seed_base64url: &'a str) -> Self {
        Self {
            trellis_url,
            name: None,
            provisioned_identity_seed_base64url,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    /// Attach optional display metadata without changing the server-owned assignment.
    pub fn with_name(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }

    /// Set the request/connect timeout in milliseconds.
    pub const fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

/// Errors returned by the high-level service runtime facade.
#[derive(Debug, thiserror::Error)]
pub enum ServiceRuntimeError {
    /// Client-side bootstrap, transport, or outbound RPC failure.
    #[error(transparent)]
    Client(#[from] TrellisClientError),

    /// Server-side handler, auth-validation, or runtime-loop failure.
    #[error(transparent)]
    Server(Box<ServerError>),

    /// A service event listener handler failed while processing a concrete event message.
    #[error("event handler failed: {source}")]
    EventHandler {
        /// Handler failure returned by the service implementation.
        source: Box<ServerError>,
        /// Event metadata observed from the delivered message.
        context: Box<ServiceEventListenerContext>,
    },

    /// The service bootstrap response did not include a resource binding.
    #[error("service bootstrap response did not include a binding")]
    MissingBootstrapBinding,

    /// The service bootstrap binding could not be parsed as a core binding.
    #[error("invalid service bootstrap binding: {0}")]
    InvalidBootstrapBinding(#[source] serde_json::Error),

    /// Service-private jobs bindings were missing or invalid.
    #[error(transparent)]
    JobsBinding(#[from] crate::jobs::bindings::JobsBindingError),

    /// A service-private jobs worker host failed.
    #[error(transparent)]
    JobWorker(#[from] crate::jobs::internal::WorkerHostError),

    /// A generated jobs queue was not present in the resolved binding.
    #[error("jobs queue '{queue_type}' was not found in service bootstrap bindings")]
    MissingJobQueue {
        /// Declared queue type absent from the binding.
        queue_type: String,
    },

    /// No durable event consumer group was declared for the requested event subject.
    #[error("event subject '{subject}' is not declared in any event consumer group")]
    MissingEventConsumerGroup {
        /// Event subject requested by the listener.
        subject: String,
    },

    /// More than one durable event consumer group matched the requested event subject.
    #[error(
        "event subject '{subject}' is declared in multiple event consumer groups: {}; specify a group",
        groups.join(", ")
    )]
    AmbiguousEventConsumerGroup {
        /// Event subject requested by the listener.
        subject: String,
        /// Matching group names.
        groups: Vec<String>,
    },

    /// The requested event consumer group is not present in the bootstrap binding.
    #[error("event consumer group '{group}' was not found in service bootstrap bindings")]
    EventConsumerGroupNotFound {
        /// Requested event consumer group name.
        group: String,
    },

    /// The requested event consumer group does not include the event subject.
    #[error("event consumer group '{group}' does not include event subject '{subject}'")]
    EventConsumerGroupSubjectMismatch {
        /// Requested event consumer group name.
        group: String,
        /// Event subject requested by the listener.
        subject: String,
    },

    /// A bound durable listener count must be at least one.
    #[error("event consumer group '{group}' has invalid listener concurrency {concurrency}; expected >= 1")]
    InvalidEventListenerConcurrency {
        /// Event consumer group name.
        group: String,
        /// Invalid requested listener count.
        concurrency: u32,
    },

    /// Registrations sharing one durable consumer must use the same local count.
    #[error(
        "event consumer group '{group}' already uses listener concurrency {existing}; requested {requested}"
    )]
    EventListenerConcurrencyMismatch {
        /// Event consumer group name.
        group: String,
        /// Listener count already registered locally.
        existing: u32,
        /// Conflicting requested listener count.
        requested: u32,
    },
}

impl From<ServerError> for ServiceRuntimeError {
    fn from(source: ServerError) -> Self {
        Self::Server(Box::new(source))
    }
}

/// Options for registering a service event listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEventListenOptions {
    /// Listener delivery mode. Durable listeners use Trellis-provisioned bindings by default.
    pub mode: ServiceEventListenerMode,
    /// Contract-local event consumer group name. Required when more than one group matches.
    pub group: Option<String>,
}

impl Default for ServiceEventListenOptions {
    fn default() -> Self {
        Self {
            mode: ServiceEventListenerMode::Durable,
            group: None,
        }
    }
}

/// Runtime context passed to service event listener handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEventListenerContext {
    /// Listener delivery mode.
    pub mode: ServiceEventListenerMode,
    /// Contract-local event consumer group selected for durable listeners.
    pub group: Option<String>,
    /// Trellis event id from the `Nats-Msg-Id` header, when present.
    pub id: Option<String>,
    /// Trellis event timestamp from the `Trellis-Event-Time` header, when present.
    pub time: Option<String>,
    /// W3C traceparent propagated with the event, when present.
    pub traceparent: Option<String>,
    /// Raw event transport headers delivered with the message.
    pub headers: HeaderMap,
    /// Verified publisher metadata from local event verification, when available.
    pub publisher: Option<ServiceEventPublisherContext>,
}

/// Verified event publisher metadata produced by local event verification.
///
/// The publisher projection is derived from the verified authorization
/// context bound into the event proof: principal kind, deployment/instance
/// identity, participant contract identity, and the active session state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEventPublisherContext {
    /// Publisher participant kind.
    pub kind: String,
    /// Publisher deployment id, when the publisher is deployment-backed.
    pub deployment_id: Option<String>,
    /// Publisher runtime instance id, when known.
    pub instance_id: Option<String>,
    /// Publisher contract id, when known.
    pub contract_id: Option<String>,
    /// Publisher contract digest, when known.
    pub contract_digest: Option<String>,
    /// Retained session lifecycle status used for validation.
    pub session_status: String,
}

/// Event listener delivery mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEventListenerMode {
    /// Delivery comes from a live NATS subscription without durable JetStream cursor metadata.
    Ephemeral,
    /// Delivery comes from a Trellis-provisioned durable JetStream consumer.
    Durable,
}

/// Handle for a registered service event listener.
///
/// Call [`ServiceEventListenerHandle::abort`] to stop delivery for this handler
/// registration. Durable listeners are removed from the shared listener registry;
/// when the last handler for a durable consumer is removed, the shared pull task
/// is also aborted.
pub struct ServiceEventListenerHandle {
    task: Option<AbortHandle>,
    registration: StdMutex<Option<ServiceEventListenerRegistration>>,
}

impl std::fmt::Debug for ServiceEventListenerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceEventListenerHandle")
            .field(
                "has_registration",
                &self
                    .registration
                    .lock()
                    .map(|registration| registration.is_some())
                    .unwrap_or(false),
            )
            .finish_non_exhaustive()
    }
}

impl ServiceEventListenerHandle {
    fn new(
        task: Option<AbortHandle>,
        registration: Option<ServiceEventListenerRegistration>,
    ) -> Self {
        Self {
            task,
            registration: StdMutex::new(registration),
        }
    }

    /// Abort this listener and remove its durable handler registration, if any.
    pub fn abort(&self) {
        if let Ok(mut registration) = self.registration.lock() {
            if let Some(registration) = registration.take() {
                remove_service_event_listener_registration(registration);
            }
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Drop for ServiceEventListenerHandle {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Cloneable handle exposed to registered service handlers.
#[derive(Clone)]
pub struct ServiceHandle {
    client: Arc<TrellisClient>,
    service_name: Arc<str>,
    binding: CoreBootstrapBinding,
    resources: ServiceResourceBindings,
    event_listeners: SharedDurableEventListeners,
    event_failures: mpsc::UnboundedSender<ServiceRuntimeError>,
    auth: LocalAuthVerifier,
    event_subscribe_needs: &'static [&'static str],
}

impl std::fmt::Debug for ServiceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceHandle")
            .field("service_name", &self.service_name)
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

impl ServiceHandle {
    /// Return the authenticated transport used by generated clients.
    pub fn generated_client(&self) -> crate::generated::Client {
        crate::generated::Client::from_client(Arc::clone(&self.client))
    }

    /// Return the authenticated service session's public key.
    pub fn session_key(&self) -> &str {
        &self.client.auth().session_key
    }

    fn client(&self) -> &Arc<TrellisClient> {
        &self.client
    }

    /// Return the service instance name used during bootstrap.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Return the parsed core bootstrap binding supplied by service bootstrap.
    pub fn binding(&self) -> &CoreBootstrapBinding {
        &self.binding
    }

    /// Return all typed resource bindings resolved during service bootstrap.
    pub fn resources(&self) -> &ServiceResourceBindings {
        &self.resources
    }

    /// Return one KV/state resource binding by contract-local resource name.
    pub fn kv_binding(&self, name: &str) -> Result<&KvResourceBinding, ServerError> {
        self.resources
            .kv
            .get(name)
            .ok_or_else(|| ServerError::MissingResourceBinding {
                service_name: self.service_name().to_string(),
                resource_kind: "kv".to_string(),
                resource_name: name.to_string(),
            })
    }

    /// Open one generated typed KV resource against its installed binding.
    #[doc(hidden)]
    pub async fn generated_kv_handle<T>(
        &self,
        name: &str,
        codec: crate::client::ResourceCodec<T>,
    ) -> Result<KvHandle<T>, ServerError>
    where
        T: crate::generated::Codec + Send + 'static,
    {
        let binding = self.kv_binding(name)?;
        validate_kv_binding(self.service_name(), name, binding)?;
        let client = self.client().nats().open_kv(binding).await?;
        Ok(KvResourceHandle::from_generated(
            name,
            binding.clone(),
            codec,
            client,
            self.client.watch_availability(),
        ))
    }

    /// Return one object-store resource binding by contract-local resource name.
    pub fn store_binding(&self, name: &str) -> Result<&StoreResourceBinding, ServerError> {
        self.resources
            .store
            .get(name)
            .ok_or_else(|| ServerError::MissingResourceBinding {
                service_name: self.service_name().to_string(),
                resource_kind: "store".to_string(),
                resource_name: name.to_string(),
            })
    }

    /// Return the service-private jobs resource binding.
    pub fn jobs_binding(&self) -> Result<&JobsResourceBinding, ServerError> {
        self.resources
            .jobs
            .as_ref()
            .ok_or_else(|| ServerError::MissingResourceBinding {
                service_name: self.service_name().to_string(),
                resource_kind: "jobs".to_string(),
                resource_name: "jobs".to_string(),
            })
    }

    /// Return an event publisher backed by the connected Trellis client.
    pub fn event_publisher(&self) -> EventPublisher {
        EventPublisher::new(Arc::clone(self.client()))
    }

    /// Submit a typed service-private job for generated participant code.
    #[doc(hidden)]
    pub async fn generated_submit_job<D>(
        &self,
        payload: D::Payload,
    ) -> Result<JobRef<D::Payload, D::Result>, JobsError>
    where
        D: JobDescriptor,
    {
        let binding = self
            .binding
            .jobs_runtime_binding()
            .map_err(|error| JobsError::Message {
                message: error.to_string(),
            })?;
        let key_coordinator = crate::jobs::keys::NatsKeyCoordinator::open_for_service(
            self.client().nats().clone(),
            &binding.jobs.namespace,
        )
        .await
        .map_err(|error| JobsError::Message {
            message: error.to_string(),
        })?;
        let manager = JobManager::new_with_key_coordinator(
            TrellisJobEventPublisher::new(self.client().nats().clone()),
            binding.jobs,
            TrellisJobMetaSource,
            Arc::new(key_coordinator),
        );
        let job = manager
            .create(D::QUEUE_TYPE, payload)
            .await
            .map_err(|error| JobsError::Message {
                message: error.to_string(),
            })?;
        let queue = manager
            .bindings()
            .queues
            .get(D::QUEUE_TYPE)
            .cloned()
            .ok_or_else(|| JobsError::Message {
                message: format!("missing jobs queue binding '{}'", D::QUEUE_TYPE),
            })?;
        let waiter = crate::jobs::runtime_ref::NatsJobWaiter::new(
            self.client().nats().clone(),
            queue,
            Duration::from_secs(30),
        );
        Ok(JobRef::from_runtime(job, waiter, manager))
    }

    /// Start a descriptor-backed event listener.
    pub async fn listen_event<D, F, Fut>(
        &self,
        handler: F,
        options: ServiceEventListenOptions,
    ) -> Result<ServiceEventListenerHandle, ServiceRuntimeError>
    where
        D: crate::client::EventDescriptor + 'static,
        D::Event: Send + 'static,
        F: Fn(D::Event, ServiceEventListenerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
    {
        listen_event_with_bindings::<D, _, _>(self, self.auth.api_id(), handler, options).await
    }

    /// Start a descriptor-backed event listener with an explicit owning API id.
    ///
    /// Use this form for events imported from another participant contract; the
    /// API id is part of the precompiled event descriptor and is required for
    /// exact publisher-permission verification.
    pub async fn listen_event_with_api_id<D, F, Fut>(
        &self,
        event_api_id: &str,
        handler: F,
        options: ServiceEventListenOptions,
    ) -> Result<ServiceEventListenerHandle, ServiceRuntimeError>
    where
        D: crate::client::EventDescriptor + 'static,
        D::Event: Send + 'static,
        F: Fn(D::Event, ServiceEventListenerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
    {
        listen_event_with_bindings::<D, _, _>(self, event_api_id, handler, options).await
    }

    /// Open a bound object-store resource client by contract-local resource name.
    pub async fn store_client(&self, name: &str) -> Result<StoreHandle, ServerError> {
        let binding = self.store_binding(name)?;
        validate_store_binding(self.service_name(), name, binding)?;
        let client = self.client().nats().open_store(binding).await?;
        Ok(StoreResourceHandle::new(
            self.service_name(),
            name,
            binding.clone(),
            client,
            self.client.watch_availability(),
        ))
    }

    /// Subscribe and run an upload transfer endpoint backed by the connected NATS client.
    pub async fn spawn_upload_transfer_endpoint_with_progress<C, F>(
        &self,
        session: UploadTransferSession,
        store: C,
        on_progress: F,
    ) -> Result<(), ServerError>
    where
        C: StoreResourceClient,
        F: Fn(OperationTransferProgress) + Send + Sync + 'static,
    {
        spawn_upload_transfer_endpoint_with_progress(
            self.client().nats().clone(),
            session,
            store,
            self.auth.clone(),
            on_progress,
        )
        .await
    }

    /// Subscribe and run an upload transfer endpoint that can be awaited until durable storage.
    pub async fn spawn_upload_transfer_endpoint_with_completion<C>(
        &self,
        session: UploadTransferSession,
        store: C,
    ) -> Result<UploadTransferCompletion, ServerError>
    where
        C: StoreResourceClient,
    {
        spawn_upload_transfer_endpoint_with_completion(
            self.client().nats().clone(),
            session,
            store,
            self.auth.clone(),
        )
        .await
    }

    /// Subscribe and run a download transfer endpoint backed by the connected NATS client.
    pub async fn spawn_download_transfer_endpoint<C>(
        &self,
        plan: DownloadTransferGrantPlan,
        store: C,
    ) -> Result<(), ServerError>
    where
        C: StoreResourceClient,
    {
        spawn_download_transfer_endpoint(
            self.client().nats().clone(),
            plan,
            store,
            self.auth.clone(),
        )
        .await
    }
}

/// High-level context for one verified live Live handler invocation.
///
/// Embeds the ordinary [`ServiceHandlerContext`] unchanged and adds the source
/// scope's cancellation token. The token has no authority constructor exposed
/// to applications.
#[derive(Debug, Clone)]
pub struct ServiceLiveHandlerContext {
    /// Ordinary service handler context for this invocation.
    pub context: ServiceHandlerContext,
    /// Cancellation for this Live source scope.
    pub cancellation: crate::live::LiveCancellation,
}

/// Per-request handler context with request metadata and a cloneable service handle.
#[derive(Debug, Clone)]
pub struct ServiceHandlerContext {
    request: RequestContext,
    handle: ServiceHandle,
    download_allowed: bool,
}

impl ServiceHandlerContext {
    /// Build a handler context from low-level request metadata and a service handle.
    pub fn new(request: RequestContext, handle: ServiceHandle) -> Self {
        Self {
            request,
            handle,
            download_allowed: false,
        }
    }

    /// Return low-level request metadata, including caller and tracing fields.
    pub fn request(&self) -> &RequestContext {
        &self.request
    }

    /// Return the cloneable service handle for outbound calls and bindings.
    pub fn handle(&self) -> &ServiceHandle {
        &self.handle
    }

    /// Plan a download transfer using this request's authenticated caller and service bindings.
    pub fn plan_download_transfer(
        &self,
        store: &str,
        transfer_id: &str,
        expires_at: &str,
        chunk_bytes: u64,
        info: super::FileTransferInfo,
    ) -> Result<DownloadTransferGrantPlan, ServerError> {
        if !self.download_allowed {
            return Err(ServerError::Nats(
                "RPC descriptor does not declare a download transfer".to_owned(),
            ));
        }
        let session_key = self
            .request
            .caller
            .as_ref()
            .map(|caller| caller.session_key.as_str())
            .ok_or_else(|| ServerError::MissingSessionKey {
                subject: self.request.subject.clone(),
            })?;
        super::plan_download_transfer_grant(super::TransferDownloadGrantArgs {
            service_name: self.handle.service_name(),
            session_key,
            service_session_key: self.handle.session_key(),
            resources: self.handle.resources(),
            store,
            transfer_id,
            expires_at,
            chunk_bytes,
            info,
        })
    }

    /// Consume this context into the low-level request metadata.
    pub fn into_request_context(self) -> RequestContext {
        self.request
    }
}

/// Connected high-level service runtime for one generated service contract.
pub struct ConnectedServiceRuntime<C> {
    client: Arc<TrellisClient>,
    binding: CoreBootstrapBinding,
    resources: ServiceResourceBindings,
    store_handles: BTreeMap<String, StoreHandle>,
    event_listeners: SharedDurableEventListeners,
    event_failures: mpsc::UnboundedSender<ServiceRuntimeError>,
    event_failure_receiver: mpsc::UnboundedReceiver<ServiceRuntimeError>,
    auth: LocalAuthVerifier,
    _event_listener_cleanup: ServiceEventListenerRegistryCleanup,
    router: Router,
    provider_deployment_id: String,
    operation_executor_id: String,
    operation_connection_id: String,
    operation_repository: Option<super::KvOperationRepository>,
    operation_staging: Option<super::resources::backend::BoundStoreResourceClient>,
    service_name: String,
    registered_subjects: BTreeSet<String>,
    job_hosts: Vec<WorkerHostHandle>,
    event_subscribe_needs: &'static [&'static str],
    _contract: PhantomData<C>,
}

impl<C> std::fmt::Debug for ConnectedServiceRuntime<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectedServiceRuntime")
            .field("binding", &self.binding)
            .field("service_name", &self.service_name)
            .field("registered_subjects", &self.registered_subjects)
            .finish_non_exhaustive()
    }
}

impl<C> ConnectedServiceRuntime<C> {
    /// Build a connected runtime from an injected client and bootstrap binding.
    pub(crate) fn from_parts(
        service_name: impl Into<String>,
        client: Arc<TrellisClient>,
        binding: CoreBootstrapBinding,
        api_id: impl Into<String>,
    ) -> Self {
        let resources = binding.resource_bindings();
        let event_listeners = SharedDurableEventListeners::default();
        let (event_failures, event_failure_receiver) = mpsc::unbounded_channel();
        let api_id = api_id.into();
        let auth =
            LocalAuthVerifier::new(client.authorization_context_cache().ok(), api_id.clone());
        let mut router = Router::new();
        let provider_deployment_id = client
            .own_deployment_id()
            .expect("connected services always have a deployment assignment");
        router.set_provider_deployment_id(provider_deployment_id.clone());
        let provider_instance_id = client
            .own_instance_id()
            .expect("connected services always have an instance assignment");
        let operation_connection_id = client
            .own_connection_id()
            .expect("connected services always have a logical connection identity");
        router.set_provider_instance_id(provider_instance_id.clone());
        Self {
            client,
            binding,
            resources,
            store_handles: BTreeMap::new(),
            event_listeners: Arc::clone(&event_listeners),
            event_failures,
            event_failure_receiver,
            auth,
            _event_listener_cleanup: ServiceEventListenerRegistryCleanup::new(event_listeners),
            router,
            provider_deployment_id,
            operation_executor_id: ulid::Ulid::new().to_string(),
            operation_connection_id,
            operation_repository: None,
            operation_staging: None,
            service_name: service_name.into(),
            registered_subjects: BTreeSet::new(),
            job_hosts: Vec::new(),
            event_subscribe_needs: &[],
            _contract: PhantomData,
        }
    }

    /// Return the internal Trellis client owned by this runtime.
    pub(crate) fn client(&self) -> &Arc<TrellisClient> {
        &self.client
    }

    fn descriptor_subject(&self, family: &str, api_id: &str, action: &str) -> String {
        let action = action.split_once('.').map_or(action, |(_, name)| name);
        let subject = match family {
            "rpc" => trellis_protocol::derive_bound_rpc_subject(
                api_id,
                &self.provider_deployment_id,
                action,
            ),
            "operation" => trellis_protocol::derive_bound_operation_subject(
                api_id,
                &self.provider_deployment_id,
                action,
            ),
            "live" => trellis_protocol::derive_bound_live_subject(
                api_id,
                &self.provider_deployment_id,
                action,
            ),
            _ => unreachable!("only request route families are deployment-bound"),
        };
        subject.expect("generated route metadata must form a valid bound subject")
    }

    /// Return the authenticated transport consumed by generated facades.
    #[doc(hidden)]
    pub fn generated_client(&self) -> crate::generated::Client {
        crate::generated::Client::from_client(Arc::clone(&self.client))
    }

    /// Return the parsed core bootstrap binding supplied by service bootstrap.
    pub fn binding(&self) -> &CoreBootstrapBinding {
        &self.binding
    }

    /// Return all typed resource bindings resolved during service bootstrap.
    pub fn resources(&self) -> &ServiceResourceBindings {
        &self.resources
    }

    /// Return an opened generic object-store handle when bootstrap installed the resource.
    #[doc(hidden)]
    pub fn generated_store_handle(&self, name: &str) -> Option<&StoreHandle> {
        self.store_handles.get(name)
    }

    /// Return one KV/state resource binding by contract-local resource name.
    pub fn kv_binding(&self, name: &str) -> Result<&KvResourceBinding, ServerError> {
        self.resources
            .kv
            .get(name)
            .ok_or_else(|| ServerError::MissingResourceBinding {
                service_name: self.service_name().to_string(),
                resource_kind: "kv".to_string(),
                resource_name: name.to_string(),
            })
    }

    /// Open one generated typed KV resource against its installed binding.
    #[doc(hidden)]
    pub async fn generated_kv_handle<T>(
        &self,
        name: &str,
        codec: crate::client::ResourceCodec<T>,
    ) -> Result<KvHandle<T>, ServerError>
    where
        T: crate::generated::Codec + Send + 'static,
    {
        let binding = self.kv_binding(name)?;
        validate_kv_binding(self.service_name(), name, binding)?;
        let client = self.client().nats().open_kv(binding).await?;
        Ok(KvResourceHandle::from_generated(
            name,
            binding.clone(),
            codec,
            client,
            self.client.watch_availability(),
        ))
    }

    /// Return one object-store resource binding by contract-local resource name.
    pub fn store_binding(&self, name: &str) -> Result<&StoreResourceBinding, ServerError> {
        self.resources
            .store
            .get(name)
            .ok_or_else(|| ServerError::MissingResourceBinding {
                service_name: self.service_name().to_string(),
                resource_kind: "store".to_string(),
                resource_name: name.to_string(),
            })
    }

    /// Return the service-private jobs resource binding.
    pub fn jobs_binding(&self) -> Result<&JobsResourceBinding, ServerError> {
        self.resources
            .jobs
            .as_ref()
            .ok_or_else(|| ServerError::MissingResourceBinding {
                service_name: self.service_name().to_string(),
                resource_kind: "jobs".to_string(),
                resource_name: "jobs".to_string(),
            })
    }

    /// Return the Jobs-domain transport used by Trellis infrastructure services.
    pub fn jobs_runtime(&self) -> crate::jobs::JobsRuntime {
        crate::jobs::JobsRuntime::from_client(self.client())
    }

    /// Return the Event Log domain transport used by Trellis infrastructure.
    pub fn events_runtime(&self) -> super::EventsRuntime {
        super::EventsRuntime::from_client(Arc::clone(self.client()))
    }

    /// Open the platform-provisioned durable operation repository for this deployment.
    pub async fn operation_repository(&self) -> Result<super::KvOperationRepository, ServerError> {
        if let Some(repository) = &self.operation_repository {
            return Ok(repository.clone());
        }
        let bucket = format!("trellis_operations_{}", self.provider_deployment_id);
        let store = async_nats::jetstream::new(self.client.nats())
            .get_key_value(bucket)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        Ok(super::KvOperationRepository::new(store))
    }

    /// Submit a typed service-private job for generated participant code.
    #[doc(hidden)]
    pub async fn generated_submit_job<D>(
        &self,
        payload: D::Payload,
    ) -> Result<JobRef<D::Payload, D::Result>, JobsError>
    where
        D: JobDescriptor,
    {
        self.generated_handle()
            .generated_submit_job::<D>(payload)
            .await
    }

    /// Start one generated service-private job worker and retain its lifecycle.
    #[doc(hidden)]
    pub async fn register_generated_job_worker<D, H, Fut, E>(
        &mut self,
        handler: H,
    ) -> Result<(), ServiceRuntimeError>
    where
        D: JobDescriptor + 'static,
        H: Fn(crate::jobs::ActiveJob<D::Payload, D::Result>) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = Result<D::Result, JobProcessError<E>>> + Send + 'static,
        E: ToString + Send + 'static,
    {
        self.register_generated_job_worker_with_concurrency::<D, H, Fut, E>(handler, 1)
            .await
    }

    /// Start generated service-private job workers with local concurrency.
    #[doc(hidden)]
    pub async fn register_generated_job_worker_with_concurrency<D, H, Fut, E>(
        &mut self,
        handler: H,
        concurrency: u32,
    ) -> Result<(), ServiceRuntimeError>
    where
        D: JobDescriptor + 'static,
        H: Fn(crate::jobs::ActiveJob<D::Payload, D::Result>) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = Result<D::Result, JobProcessError<E>>> + Send + 'static,
        E: ToString + Send + 'static,
    {
        let mut binding = self.binding.jobs_runtime_binding()?;
        binding
            .jobs
            .queues
            .retain(|queue, _| queue == D::QUEUE_TYPE);
        if binding.jobs.queues.is_empty() {
            return Err(ServiceRuntimeError::MissingJobQueue {
                queue_type: D::QUEUE_TYPE.to_string(),
            });
        }
        let host = start_worker_host_from_client(
            self.client(),
            binding,
            ulid::Ulid::new().to_string(),
            |_, _| TrellisJobMetaSource,
            move |active| {
                let handler = handler.clone();
                async move {
                    let active = crate::jobs::internal::typed_active_job::<D>(active)
                        .map_err(|error| JobProcessError::Failed(error.to_string()))?;
                    let result = handler(active).await.map_err(|error| match error {
                        JobProcessError::Retryable(error) => {
                            JobProcessError::Retryable(error.to_string())
                        }
                        JobProcessError::Failed(error) => {
                            JobProcessError::Failed(error.to_string())
                        }
                    })?;
                    serde_json::to_value(result)
                        .map_err(|error| JobProcessError::Failed(error.to_string()))
                }
            },
            WorkerHostOptions {
                queue_concurrency: std::collections::BTreeMap::from([(
                    D::QUEUE_TYPE.to_owned(),
                    concurrency,
                )]),
                ..WorkerHostOptions::default()
            },
        )
        .await?;
        self.job_hosts.push(host);
        Ok(())
    }

    /// Return an event publisher backed by the connected NATS client.
    pub fn event_publisher(&self) -> EventPublisher {
        EventPublisher::new(Arc::clone(self.client()))
    }

    /// Start a descriptor-backed event listener.
    pub async fn listen_event<D, F, Fut>(
        &self,
        handler: F,
        options: ServiceEventListenOptions,
    ) -> Result<ServiceEventListenerHandle, ServiceRuntimeError>
    where
        D: crate::client::EventDescriptor + 'static,
        D::Event: Send + 'static,
        F: Fn(D::Event, ServiceEventListenerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
    {
        self.generated_handle()
            .listen_event::<D, _, _>(handler, options)
            .await
    }

    /// Open a bound object-store resource client by contract-local resource name.
    pub async fn store_client(&self, name: &str) -> Result<StoreHandle, ServerError> {
        let binding = self.store_binding(name)?;
        validate_store_binding(self.service_name(), name, binding)?;
        let client = self.client().nats().open_store(binding).await?;
        Ok(StoreResourceHandle::new(
            self.service_name(),
            name,
            binding.clone(),
            client,
            self.client.watch_availability(),
        ))
    }

    /// Return the service instance name used during bootstrap.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Return the registered NATS subjects, derived from descriptors.
    pub fn registered_subjects(&self) -> Vec<&str> {
        self.registered_subjects
            .iter()
            .map(String::as_str)
            .collect()
    }

    /// Start a descriptor-backed event listener with an explicit owning API id.
    ///
    /// Use this form for events imported from another participant contract; the
    /// API id is part of the precompiled event descriptor and is required for
    /// exact publisher-permission verification.
    pub async fn listen_event_with_api_id<D, F, Fut>(
        &self,
        event_api_id: &str,
        handler: F,
        options: ServiceEventListenOptions,
    ) -> Result<ServiceEventListenerHandle, ServiceRuntimeError>
    where
        D: crate::client::EventDescriptor + 'static,
        D::Event: Send + 'static,
        F: Fn(D::Event, ServiceEventListenerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
    {
        self.generated_handle()
            .listen_event_with_api_id::<D, _, _>(event_api_id, handler, options)
            .await
    }

    /// Register one descriptor-backed RPC handler and record its subject.
    pub fn register_rpc<D, F, Fut>(&mut self, handler: F)
    where
        D: RpcDescriptor + 'static,
        F: Fn(ServiceHandlerContext, D::Input) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HandlerResult<D::Output>> + Send + 'static,
    {
        let handle = self.generated_handle();
        self.router.register_rpc::<D, _, _>(move |request, input| {
            let mut context = ServiceHandlerContext::new(request, handle.clone());
            context.download_allowed = D::DOWNLOAD;
            handler(context, input)
        });
        self.registered_subjects
            .insert(self.descriptor_subject("rpc", D::API_ID, D::KEY));
    }

    /// Register one descriptor-backed live Live handler and record its subject.
    ///
    /// The high-level handler receives the embedded ordinary
    /// [`ServiceHandlerContext`] plus this source scope's cancellation token.
    pub fn register_live<D, F, S>(&mut self, handler: F)
    where
        D: LiveDescriptor + 'static,
        D::Input: Send + 'static,
        F: Fn(ServiceLiveHandlerContext, D::Input) -> S + Send + Sync + 'static,
        S: Stream<Item = Result<D::Event, ServerError>> + Send + 'static,
    {
        let handle = self.generated_handle();
        self.router.register_live::<D, _, _>(move |request, input| {
            handler(
                ServiceLiveHandlerContext {
                    context: ServiceHandlerContext::new(request.request, handle.clone()),
                    cancellation: request.cancellation,
                },
                input,
            )
        });
        let subject = self.descriptor_subject("live", D::API_ID, D::KEY);
        self.registered_subjects.insert(subject.clone());
        self.router
            .set_live_owner(super::live_router::LiveProviderOwner::new(
                std::sync::Arc::clone(&self.client),
            ));
    }

    /// Register one operation business handler and record its runtime-owned lifecycle routes.
    ///
    /// An operation control route is a Live route, so this installs the
    /// connection's live provider owner exactly as [`Self::register_live`] does.
    pub fn register_operation_handler<D, F, Fut>(&mut self, handler: F)
    where
        D: OperationDescriptor + 'static,
        D::Progress: serde::de::DeserializeOwned,
        D::Output: serde::de::DeserializeOwned,
        F: Fn(RequestContext, D::Input, OperationControl<D>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
    {
        self.router.register_operation_provider::<D, _>(
            super::operations::RuntimeOperationProvider::new_authenticated(
                super::operations::OperationHandlerRuntime {
                    service: self.service_name.clone(),
                    deployment_id: self.provider_deployment_id.clone(),
                    executor_id: self.operation_executor_id.clone(),
                    connection_id: self.operation_connection_id.clone(),
                    repository: self
                        .operation_repository
                        .clone()
                        .expect("connected service operation repository"),
                    nats: self.client.nats().clone(),
                    service_session_key: self.client.auth().session_key.clone(),
                    staging: self
                        .operation_staging
                        .clone()
                        .expect("connected service operation staging store"),
                    validator: self.auth.clone(),
                },
                handler,
                self.client
                    .participant_id()
                    .expect("connected services always have a participant identity"),
                Some(Arc::clone(&self.client)),
            ),
        );
        let subject = self.descriptor_subject("operation", D::API_ID, D::KEY);
        self.registered_subjects.insert(subject.clone());
        self.registered_subjects.insert(control_subject(&subject));
        self.router
            .set_live_owner(super::live_router::LiveProviderOwner::new(
                std::sync::Arc::clone(&self.client),
            ));
    }

    /// Run registered subjects using the default NATS request loop.
    pub async fn run(self) -> Result<(), ServiceRuntimeError> {
        self.router.recover_operations().await?;
        // A live-capable router must be given its connection's provider owner
        // before it serves any traffic; fail before readiness, not at the first
        // caller.
        self.router
            .require_live_owner()
            .map_err(ServiceRuntimeError::from)?;
        let mut event_failures = self.event_failure_receiver;
        let subjects = self.registered_subjects.into_iter().collect::<Vec<_>>();
        let job_hosts = self.job_hosts;
        let host = bootstrap_service_host(
            &self.service_name,
            self.binding.bootstrap_binding(),
            self.router,
            self.auth,
        );
        let serve = async {
            if subjects.is_empty() {
                std::future::pending::<()>().await;
            }
            let subject_refs = subjects.iter().map(String::as_str).collect::<Vec<_>>();
            run_multi_subject_service(self.client.nats().clone(), &subject_refs, host)
                .await
                .map_err(ServiceRuntimeError::from)
        };
        let run = async {
            if job_hosts.is_empty() {
                return serve.await;
            }
            let workers = async {
                futures_util::future::try_join_all(
                    job_hosts.into_iter().map(WorkerHostHandle::join),
                )
                .await
                .map_err(ServiceRuntimeError::JobWorker)?;
                Ok(())
            };
            tokio::try_join!(serve, workers)?;
            Ok(())
        };
        tokio::select! {
            result = run => result,
            Some(error) = event_failures.recv() => Err(error),
        }
    }

    /// Return a cloneable service handle for generated participant code.
    #[doc(hidden)]
    pub fn generated_handle(&self) -> ServiceHandle {
        ServiceHandle {
            client: Arc::clone(&self.client),
            service_name: Arc::from(self.service_name.as_str()),
            binding: self.binding.clone(),
            resources: self.resources.clone(),
            event_listeners: Arc::clone(&self.event_listeners),
            event_failures: self.event_failures.clone(),
            auth: self.auth.clone(),
            event_subscribe_needs: self.event_subscribe_needs,
        }
    }
}

impl<C: crate::generated::ParticipantDescriptor> ConnectedServiceRuntime<C> {
    /// Connect with generated participant evidence and parse the returned bootstrap binding.
    pub async fn connect(options: ServiceConnectOptions<'_>) -> Result<Self, ServiceRuntimeError> {
        let client =
            TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
                trellis_url: options.trellis_url,
                participant_id: C::ID,
                participant_path: C::PATH,
                package_evidence: C::package_evidence(),
                name: options.name,
                provisioned_identity_seed_base64url: options.provisioned_identity_seed_base64url,
                timeout_ms: options.timeout_ms,
            })
            .await?;
        let binding = parse_bootstrap_binding(&client)?;
        let api_id = C::IMPLEMENTED_API_IDS.first().copied().ok_or_else(|| {
            TrellisClientError::Bootstrap(format!(
                "generated service participant `{}` implements no API",
                C::ID
            ))
        })?;
        let mut runtime = Self::from_parts(
            options.name.unwrap_or(C::ID),
            Arc::new(client),
            binding,
            api_id,
        );
        runtime.event_subscribe_needs = C::EVENT_SUBSCRIBE_NEEDS;
        runtime.operation_repository = Some(runtime.operation_repository().await?);
        let staging = async_nats::jetstream::new(runtime.client.nats().clone())
            .get_object_store(format!(
                "trellis_operation_staging_{}",
                runtime.provider_deployment_id
            ))
            .await
            .map_err(|error| {
                ServiceRuntimeError::Server(Box::new(ServerError::Nats(error.to_string())))
            })?;
        runtime.operation_staging = Some(super::resources::backend::BoundStoreResourceClient::new(
            staging,
        ));
        for name in runtime.resources.store.keys().cloned().collect::<Vec<_>>() {
            let handle = runtime.store_client(&name).await?;
            runtime.store_handles.insert(name, handle);
        }
        Ok(runtime)
    }
}

fn parse_bootstrap_binding(
    client: &TrellisClient,
) -> Result<CoreBootstrapBinding, ServiceRuntimeError> {
    client
        .service_bootstrap_binding()
        .cloned()
        .ok_or(ServiceRuntimeError::MissingBootstrapBinding)
}

/// Whether a participant's declared Event Subscribe needs authorize ephemeral
/// delivery for one event. A declared durable consumer is not included.
#[must_use]
pub(crate) fn ephemeral_event_authorized(needs: &[&str], event_name: &str) -> bool {
    let need = format!("event:{event_name}");
    needs.contains(&need.as_str())
}

fn service_event_context_from_headers(
    mode: ServiceEventListenerMode,
    group: Option<String>,
    headers: Option<&HeaderMap>,
    publisher: Option<ServiceEventPublisherContext>,
) -> ServiceEventListenerContext {
    let headers = headers.cloned().unwrap_or_default();
    ServiceEventListenerContext {
        mode,
        group,
        id: headers
            .get("Nats-Msg-Id")
            .map(|value| value.as_str().to_string()),
        time: headers
            .get("Trellis-Event-Time")
            .map(|value| value.as_str().to_string()),
        traceparent: headers
            .get("traceparent")
            .map(|value| value.as_str().to_string()),
        headers,
        publisher,
    }
}

async fn listen_event_with_bindings<D, F, Fut>(
    service: &ServiceHandle,
    event_api_id: &str,
    handler: F,
    options: ServiceEventListenOptions,
) -> Result<ServiceEventListenerHandle, ServiceRuntimeError>
where
    D: crate::client::EventDescriptor + 'static,
    D::Event: Send + 'static,
    F: Fn(D::Event, ServiceEventListenerContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
{
    let client = &service.client;
    let auth = service.auth.clone();
    let bindings = &service.resources.event_consumers;
    let event_listeners = Arc::clone(&service.event_listeners);
    let failures = service.event_failures.clone();
    let event_api_id = event_api_id.to_owned();
    let event_name = D::KEY
        .split_once('.')
        .map_or(D::KEY, |(_, name)| name)
        .to_owned();
    let descriptor_identity = D::descriptor_identity()
        .map_err(|error| ServiceRuntimeError::Client(TrellisClientError::Subject(error)))?;
    let route = crate::telemetry::instruments::route_token(
        crate::telemetry::instruments::RouteFamily::Consumer,
        &event_name,
    );
    if options.mode == ServiceEventListenerMode::Ephemeral {
        // A declared durable consumer grants Consume authority only. Raw
        // ephemeral observation needs its own Event Subscribe need; fail fast
        // rather than sitting on a subscription the broker will not deliver to.
        if !ephemeral_event_authorized(service.event_subscribe_needs, &event_name) {
            return Err(ServiceRuntimeError::Client(
                TrellisClientError::EventSubscriptionProtocol(format!(
                    "ephemeral event delivery for '{event_name}' requires an Event Subscribe authority; a declared consumer grants durable delivery only"
                )),
            ));
        }
        let mut events = client
            .nats()
            .subscribe(client.descriptor_subject(D::SUBSCRIBE_SUBJECT))
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
        client
            .nats()
            .flush()
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
        let event_auth = auth.clone();
        let descriptor_identity = descriptor_identity.clone();
        let task = tokio::spawn(async move {
            let result = async {
                while let Some(message) = events.next().await {
                    let observation = crate::telemetry::lifecycle::Observation::start(
                        crate::telemetry::instruments::DurationFamily::EventProcess,
                        vec![crate::telemetry::KeyValue::new("trellis.route", route)],
                        "cancelled",
                    );
                    let publisher = match event_auth
                        .verify_event(
                            message.subject.as_ref(),
                            &message.payload,
                            message.headers.as_ref(),
                            &descriptor_identity,
                        )
                        .await
                    {
                        Ok(publisher) => publisher,
                        Err(error) => {
                            observation.finish(match &error {
                                super::EventVerificationFailure::Retryable(_) => "unavailable",
                                super::EventVerificationFailure::Rejected(_) => "invalid",
                            });
                            tracing::warn!(
                                subject = %message.subject,
                                error = %error.message(),
                                "Event auth validation failed"
                            );
                            continue;
                        }
                    };
                    let context = service_event_context_from_headers(
                        ServiceEventListenerMode::Ephemeral,
                        None,
                        message.headers.as_ref(),
                        Some(publisher),
                    );
                    let event = serde_json::from_slice::<D::Event>(&message.payload)
                        .map_err(TrellisClientError::from);
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            observation.finish("invalid");
                            return Err(error.into());
                        }
                    };
                    let attempt_span = tracing::info_span!(parent: None, "trellis.event.attempt.start", "trellis.route" = route);
                    if let Some(headers) = message.headers.as_ref() {
                        let pairs: Vec<_> = headers.iter().flat_map(|(name, values)| {
                            values.iter().map(move |value| (name.to_string(), value.as_str().to_owned()))
                        }).collect();
                        let carrier = crate::telemetry::propagation::extract_context(&pairs);
                        let linked = carrier.span().span_context().clone();
                        if linked.is_valid() {
                            attempt_span.add_link(linked);
                        }
                    }
                    let attempt_context = opentelemetry::Context::new()
                        .with_remote_span_context(attempt_span.context().span().span_context().clone());
                    drop(attempt_span);
                    if let Err(source) = async { handler(event, context.clone()).await }
                        .with_context(attempt_context)
                        .await
                    {
                        observation.finish("error");
                        return Err(ServiceRuntimeError::EventHandler {
                            source: Box::new(source),
                            context: Box::new(context),
                        });
                    }
                    observation.finish("ok");
                }
                Ok::<(), ServiceRuntimeError>(())
            }
            .await;
            if let Err(error) = result {
                let _ = failures.send(error);
            }
        });
        return Ok(ServiceEventListenerHandle::new(
            Some(task.abort_handle()),
            None,
        ));
    }

    let subject = client.descriptor_subject(D::SUBSCRIBE_SUBJECT);
    let (group, binding) =
        resolve_event_consumer_binding(bindings, &subject, options.group.as_deref())?;
    validate_event_listener_concurrency(&group, binding.concurrency, None)?;
    let key = DurableEventListenerKey {
        stream: binding.stream.clone(),
        durable_name: binding.consumer_name.clone(),
    };
    let context = ServiceEventListenerContext {
        mode: ServiceEventListenerMode::Durable,
        group: Some(group),
        id: None,
        time: None,
        traceparent: None,
        headers: HeaderMap::new(),
        publisher: None,
    };
    let handler = Arc::new(handler);
    let handler_id = SERVICE_EVENT_HANDLER_ID.fetch_add(1, Ordering::Relaxed);
    let handler: SharedEventHandler = Arc::new(move |payload, context| {
        let handler = Arc::clone(&handler);
        let event = serde_json::from_slice::<D::Event>(&payload)
            .map_err(TrellisClientError::from)
            .map_err(ServiceRuntimeError::from);
        Box::pin(async move {
            handler(event?, context.clone()).await.map_err(|source| {
                ServiceRuntimeError::EventHandler {
                    source: Box::new(source),
                    context: Box::new(context),
                }
            })
        })
    });

    let mut listeners = lock_service_event_listeners(&event_listeners);
    if let Some(listener) = listeners.get_mut(&key) {
        validate_event_listener_concurrency(
            context.group.as_deref().expect("durable listener group"),
            binding.concurrency,
            Some(listener.concurrency),
        )?;
        for (pattern, registration) in &listener.registrations {
            if event_patterns_overlap(pattern, &subject)
                && (pattern != &subject
                    || registration.event_api_id != event_api_id
                    || registration.event_name != event_name
                    || registration.descriptor_identity != descriptor_identity)
            {
                return Err(TrellisClientError::EventSubscriptionProtocol(format!(
                    "event registration '{subject}' overlaps '{pattern}'"
                ))
                .into());
            }
        }
        listener
            .registrations
            .entry(subject.clone())
            .or_insert_with(|| EventRegistration {
                event_api_id: event_api_id.clone(),
                event_name: event_name.clone(),
                descriptor_identity: descriptor_identity.clone(),
                handlers: BTreeMap::new(),
            })
            .handlers
            .insert(handler_id, handler);
        return Ok(ServiceEventListenerHandle::new(
            None,
            Some(ServiceEventListenerRegistration {
                event_listeners: Arc::clone(&event_listeners),
                key,
                subject,
                handler_id,
            }),
        ));
    }

    let subscribe_options = EventSubscribeOptions {
        stream: Some(binding.stream.clone()),
        mode: EventSubscriptionMode::Durable,
        replay: EventReplayPolicy::New,
        durable_name: Some(binding.consumer_name.clone()),
    };
    let pull_abort_handles = (0..binding.concurrency)
        .map(|_| {
            let pull = run_durable_event_pull_loop(
                Arc::clone(client),
                auth.clone(),
                Arc::clone(&event_listeners),
                DurableEventPullConfig {
                    key: key.clone(),
                    subscribe_options: subscribe_options.clone(),
                    replay_subscribe_options: EventSubscribeOptions {
                        stream: Some(binding.replay_binding.stream.clone()),
                        mode: EventSubscriptionMode::Durable,
                        replay: EventReplayPolicy::New,
                        durable_name: Some(binding.replay_binding.consumer_name.clone()),
                    },
                    context: context.clone(),
                    ack_wait: Duration::from_millis(binding.ack_wait_ms.unsigned_abs()),
                    backoff: binding
                        .backoff_ms
                        .iter()
                        .map(|delay| Duration::from_millis(delay.unsigned_abs()))
                        .collect(),
                    max_deliver: binding.max_deliver.unsigned_abs(),
                    resource_id: binding.resource_id.clone(),
                },
            );
            let failures = failures.clone();
            tokio::spawn(async move {
                if let Err(error) = pull.await {
                    let _ = failures.send(error);
                }
            })
            .abort_handle()
        })
        .collect();
    listeners.insert(
        key.clone(),
        SharedDurableEventListener {
            registrations: BTreeMap::from([(
                subject.clone(),
                EventRegistration {
                    event_api_id,
                    event_name,
                    descriptor_identity,
                    handlers: BTreeMap::from([(handler_id, handler)]),
                },
            )]),
            concurrency: binding.concurrency,
            pull_abort_handles,
        },
    );
    drop(listeners);

    Ok(ServiceEventListenerHandle::new(
        None,
        Some(ServiceEventListenerRegistration {
            event_listeners,
            key,
            subject,
            handler_id,
        }),
    ))
}

fn validate_event_listener_concurrency(
    group: &str,
    requested: u32,
    existing: Option<u32>,
) -> Result<(), ServiceRuntimeError> {
    if requested == 0 {
        return Err(ServiceRuntimeError::InvalidEventListenerConcurrency {
            group: group.to_string(),
            concurrency: requested,
        });
    }
    if let Some(existing) = existing.filter(|existing| *existing != requested) {
        return Err(ServiceRuntimeError::EventListenerConcurrencyMismatch {
            group: group.to_string(),
            existing,
            requested,
        });
    }
    Ok(())
}

fn lock_service_event_listeners(
    event_listeners: &SharedDurableEventListeners,
) -> std::sync::MutexGuard<'_, BTreeMap<DurableEventListenerKey, SharedDurableEventListener>> {
    event_listeners
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn remove_service_event_listener_registration(registration: ServiceEventListenerRegistration) {
    let mut listeners = lock_service_event_listeners(&registration.event_listeners);
    let Some(listener) = listeners.get_mut(&registration.key) else {
        return;
    };
    if let Some(event) = listener.registrations.get_mut(&registration.subject) {
        event.handlers.remove(&registration.handler_id);
        if event.handlers.is_empty() {
            listener.registrations.remove(&registration.subject);
        }
    }
    if listener.registrations.is_empty() {
        if let Some(listener) = listeners.remove(&registration.key) {
            for handle in listener.pull_abort_handles {
                handle.abort();
            }
        }
    }
}

fn remove_service_event_listeners(event_listeners: &SharedDurableEventListeners) {
    let listeners = std::mem::take(&mut *lock_service_event_listeners(event_listeners));
    for (_, listener) in listeners {
        for handle in listener.pull_abort_handles {
            handle.abort();
        }
    }
}

async fn run_durable_event_pull_loop(
    client: Arc<TrellisClient>,
    auth: LocalAuthVerifier,
    event_listeners: SharedDurableEventListeners,
    config: DurableEventPullConfig,
) -> Result<(), ServiceRuntimeError> {
    let mut replay = false;
    let mut original_consumer_opened = false;
    loop {
        let is_replay = replay;
        replay = !replay;
        let messages = match client
            .event_messages::<serde_json::Value>(
                if is_replay {
                    config.replay_subscribe_options.clone()
                } else {
                    config.subscribe_options.clone()
                },
                None,
                Some(1),
            )
            .await
        {
            Ok(messages) => messages,
            Err(error)
                if missing_durable_event_consumer_is_retryable(
                    &error,
                    is_replay,
                    original_consumer_opened,
                ) =>
            {
                tokio::time::sleep(Duration::from_millis(DURABLE_EVENT_CONSUMER_RETRY_MS)).await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if !is_replay {
            original_consumer_opened = true;
        }
        let mut messages = messages.take(1);

        loop {
            let result = match tokio::time::timeout(Duration::from_secs(1), messages.next()).await {
                Ok(Some(result)) => result,
                Ok(None) => break,
                Err(_) => break,
            };
            let message = match result {
                Ok(message) => message,
                Err(error)
                    if missing_durable_event_consumer_is_retryable(
                        &error,
                        is_replay,
                        original_consumer_opened,
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(DURABLE_EVENT_CONSUMER_RETRY_MS))
                        .await;
                    break;
                }
                Err(error) => return Err(error.into()),
            };
            let replay_envelope = if is_replay {
                Some(
                    serde_json::from_slice::<ConsumerReplayEnvelope>(message.payload()).map_err(
                        |error| TrellisClientError::EventSubscriptionProtocol(error.to_string()),
                    )?,
                )
            } else {
                None
            };
            if let Some(envelope) = &replay_envelope {
                if envelope.resource_id != config.resource_id
                    || envelope.original_record_sequence == 0
                {
                    message.term().await?;
                    continue;
                }
                let inspected = match crate::generated::Client::from_client(Arc::clone(&client))
                    .call::<ConsumerDeadLetterInspect>(&ConsumerDeadLetterInspectInput {
                        resource_id: config.resource_id.clone(),
                        dead_letter_id: envelope.dead_letter_id.clone(),
                    })
                    .await
                {
                    Ok(inspected) => inspected,
                    Err(error) => {
                        tracing::warn!(error = %error, "Replay state inspection unavailable");
                        let delivery = message.delivery_count();
                        let delay = config
                            .backoff
                            .get(delivery.saturating_sub(1) as usize)
                            .or_else(|| config.backoff.last())
                            .copied()
                            .unwrap_or(config.ack_wait);
                        message.nak_after(delay).await?;
                        continue;
                    }
                };
                let detail = inspected.get("deadLetter").unwrap_or(&inspected);
                let dead_letter = detail.get("deadLetter").unwrap_or(detail);
                let projected_generation = dead_letter.get("generation").and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                });
                if projected_generation.is_none_or(|generation| generation < envelope.generation) {
                    message
                        .nak_after(Duration::from_millis(DURABLE_EVENT_CONSUMER_RETRY_MS))
                        .await?;
                    continue;
                }
                if projected_generation != Some(envelope.generation)
                    || !matches!(
                        dead_letter.get("state").and_then(serde_json::Value::as_str),
                        Some("replayPending" | "replaying")
                    )
                {
                    message.ack().await?;
                    continue;
                }
            }
            let effective_subject = replay_envelope.as_ref().map_or_else(
                || message.subject(),
                |envelope| envelope.original_subject.as_str(),
            );
            let registration = lock_service_event_listeners(&event_listeners)
                .get(&config.key)
                .and_then(|listener| {
                    listener
                        .registrations
                        .iter()
                        .find(|(pattern, _)| event_patterns_overlap(pattern, effective_subject))
                        .map(|(_, registration)| registration.clone())
                });
            let Some(registration) = registration else {
                tracing::warn!(subject = %message.subject(), "No registered event handler; retaining message for redelivery");
                message.nak_after(Duration::from_secs(5)).await?;
                continue;
            };
            let mut replay_headers = HeaderMap::new();
            if let Some(envelope) = &replay_envelope {
                for (name, values) in &envelope.original_headers {
                    for value in values {
                        replay_headers.append(name.as_str(), value.as_str());
                    }
                }
            }
            let effective_payload = replay_envelope.as_ref().map_or_else(
                || message.payload(),
                |envelope| envelope.original_payload_bytes.as_slice(),
            );
            let effective_headers = replay_envelope
                .as_ref()
                .map_or_else(|| message.headers(), |_| Some(&replay_headers));
            let route = crate::telemetry::instruments::route_token(
                crate::telemetry::instruments::RouteFamily::Consumer,
                &registration.event_name,
            );
            let mut observations: Vec<_> = registration
                .handlers
                .values()
                .map(|_| {
                    crate::telemetry::lifecycle::Observation::start(
                        crate::telemetry::instruments::DurationFamily::EventProcess,
                        vec![crate::telemetry::KeyValue::new("trellis.route", route)],
                        "cancelled",
                    )
                })
                .collect();
            let publisher = match auth
                .verify_event(
                    effective_subject,
                    effective_payload,
                    effective_headers,
                    &registration.descriptor_identity,
                )
                .await
            {
                Ok(publisher) => publisher,
                Err(error) => {
                    tracing::warn!(
                        subject = %message.subject(),
                        error = %error.message(),
                        "Event auth validation failed"
                    );
                    match error {
                        super::EventVerificationFailure::Retryable(_) => {
                            let delivery = message.delivery_count();
                            let delay = config
                                .backoff
                                .get(delivery.saturating_sub(1) as usize)
                                .or_else(|| config.backoff.last())
                                .copied()
                                .unwrap_or(config.ack_wait);
                            let result = message.nak_after(delay).await;
                            for observation in observations.drain(..) {
                                observation.finish(if result.is_ok() {
                                    "unavailable"
                                } else {
                                    "error"
                                });
                            }
                        }
                        super::EventVerificationFailure::Rejected(_) => {
                            if let Some(envelope) = &replay_envelope {
                                message.ack_progress().await?;
                                let report = ConsumerDeliveryReport {
                                    resource_id: config.resource_id.clone(),
                                    source_stream: config
                                        .replay_subscribe_options
                                        .stream
                                        .clone()
                                        .unwrap_or_default(),
                                    source_sequence: message.stream_sequence()?.to_string(),
                                    delivery_count: message.delivery_count(),
                                    delivery_proof: message.delivery_proof()?,
                                    replay_generation: Some(envelope.generation),
                                    outcome: "unreplayable".to_owned(),
                                    error: Some(error.message().to_owned()),
                                };
                                if let Err(error) =
                                    crate::generated::Client::from_client(Arc::clone(&client))
                                        .call::<ConsumerReportDelivery>(&report)
                                        .await
                                {
                                    tracing::warn!(%error, consumer = %config.key.durable_name, "Replay delivery report unavailable");
                                    for observation in observations.drain(..) {
                                        observation.finish("error");
                                    }
                                    continue;
                                }
                                let result = message.term().await;
                                for observation in observations.drain(..) {
                                    observation.finish(if result.is_ok() {
                                        "invalid"
                                    } else {
                                        "error"
                                    });
                                }
                                result?;
                            } else {
                                let result = message.term().await;
                                for observation in observations.drain(..) {
                                    observation.finish(if result.is_ok() {
                                        "invalid"
                                    } else {
                                        "error"
                                    });
                                }
                            }
                        }
                    }
                    continue;
                }
            };
            let attempt_span = tracing::info_span!(parent: None, "trellis.event.attempt.start", "trellis.route" = route);
            if let Some(headers) = effective_headers {
                let pairs: Vec<_> = headers
                    .iter()
                    .flat_map(|(name, values)| {
                        values
                            .iter()
                            .map(move |value| (name.to_string(), value.as_str().to_owned()))
                    })
                    .collect();
                let carrier = crate::telemetry::propagation::extract_context(&pairs);
                let linked = carrier.span().span_context().clone();
                if linked.is_valid() {
                    attempt_span.add_link(linked);
                }
            }
            let attempt_context = opentelemetry::Context::new()
                .with_remote_span_context(attempt_span.context().span().span_context().clone());
            drop(attempt_span);
            let mut handled = true;
            let delivery = message.delivery_count();
            let effective_wait = config
                .backoff
                .get(delivery.saturating_sub(1) as usize)
                .or_else(|| config.backoff.last())
                .copied()
                .unwrap_or(config.ack_wait);
            let progress_interval = durable_event_progress_interval(effective_wait);
            let mut completed: Vec<crate::telemetry::lifecycle::Observation> = Vec::new();
            let mut observations = observations.into_iter();
            for handler in registration.handlers.values() {
                let observation = observations.next().expect("one observation per handler");
                let context = service_event_context_from_headers(
                    config.context.mode,
                    config.context.group.clone(),
                    effective_headers,
                    Some(publisher.clone()),
                );
                let future =
                    async { handler(Bytes::copy_from_slice(effective_payload), context).await }
                        .with_context(attempt_context.clone());
                tokio::pin!(future);
                let mut progress = tokio::time::interval(progress_interval);
                progress.tick().await;
                let result = loop {
                    tokio::select! {
                        result = &mut future => break result,
                        _ = progress.tick() => message.ack_progress().await?,
                    }
                };
                if result.is_err() {
                    let error = result.err().map(|error| error.to_string());
                    if delivery >= config.max_deliver {
                        message.ack_progress().await?;
                        let report = ConsumerDeliveryReport {
                            resource_id: config.resource_id.clone(),
                            source_stream: if is_replay {
                                config
                                    .replay_subscribe_options
                                    .stream
                                    .clone()
                                    .unwrap_or_default()
                            } else {
                                config.subscribe_options.stream.clone().unwrap_or_default()
                            },
                            source_sequence: message.stream_sequence()?.to_string(),
                            delivery_count: delivery,
                            delivery_proof: message.delivery_proof()?,
                            replay_generation: replay_envelope
                                .as_ref()
                                .map(|envelope| envelope.generation),
                            outcome: "exhausted".to_owned(),
                            error,
                        };
                        let report_result =
                            crate::generated::Client::from_client(Arc::clone(&client))
                                .call::<ConsumerReportDelivery>(&report)
                                .await;
                        let reported = report_result.is_ok();
                        if reported {
                            if let Err(error) = message.ack().await {
                                for earlier in completed.drain(..) {
                                    earlier.finish("ok");
                                }
                                observation.finish("error");
                                return Err(error.into());
                            }
                        } else if let Err(error) = report_result {
                            tracing::warn!(%error, group = ?config.context.group, "Delivery report unavailable");
                        }
                        for earlier in completed.drain(..) {
                            earlier.finish("ok");
                        }
                        observation.finish(if reported { "exhausted" } else { "error" });
                        handled = false;
                        break;
                    }
                    let delay = config
                        .backoff
                        .get(delivery.saturating_sub(1) as usize)
                        .or_else(|| config.backoff.last())
                        .copied()
                        .unwrap_or(Duration::ZERO);
                    let result = message.nak_after(delay).await;
                    for earlier in completed.drain(..) {
                        earlier.finish("ok");
                    }
                    observation.finish(if result.is_ok() { "retry" } else { "error" });
                    handled = false;
                    break;
                }
                completed.push(observation);
            }
            for uncalled in observations {
                uncalled.discard();
            }
            if !handled {
                continue;
            }
            if let Some(envelope) = &replay_envelope {
                message.ack_progress().await?;
                let report = ConsumerDeliveryReport {
                    resource_id: config.resource_id.clone(),
                    source_stream: config
                        .replay_subscribe_options
                        .stream
                        .clone()
                        .unwrap_or_default(),
                    source_sequence: message.stream_sequence()?.to_string(),
                    delivery_count: delivery,
                    delivery_proof: message.delivery_proof()?,
                    replay_generation: Some(envelope.generation),
                    outcome: "succeeded".to_owned(),
                    error: None,
                };
                if let Err(error) = crate::generated::Client::from_client(Arc::clone(&client))
                    .call::<ConsumerReportDelivery>(&report)
                    .await
                {
                    tracing::warn!(%error, consumer = %config.key.durable_name, "Replay delivery report unavailable");
                    for observation in completed {
                        observation.finish("error");
                    }
                    continue;
                }
            }
            let ack = message.ack().await;
            for observation in completed {
                observation.finish(if ack.is_ok() { "ok" } else { "error" });
            }
            ack?;
        }
    }
}

fn durable_event_progress_interval(effective_wait: Duration) -> Duration {
    Duration::from_millis((effective_wait.as_millis() as u64 / 3).max(1))
}

fn resolve_event_consumer_binding(
    bindings: &BTreeMap<String, super::EventConsumerResourceBinding>,
    subject: &str,
    group: Option<&str>,
) -> Result<(String, super::EventConsumerResourceBinding), ServiceRuntimeError> {
    if let Some(group) = group {
        let binding =
            bindings
                .get(group)
                .ok_or_else(|| ServiceRuntimeError::EventConsumerGroupNotFound {
                    group: group.to_string(),
                })?;
        if !binding
            .filter_subjects
            .iter()
            .any(|filter_subject| filter_subject == subject)
        {
            return Err(ServiceRuntimeError::EventConsumerGroupSubjectMismatch {
                group: group.to_string(),
                subject: subject.to_string(),
            });
        }
        return Ok((group.to_string(), binding.clone()));
    }

    let matches = bindings
        .iter()
        .filter(|(_, binding)| {
            binding
                .filter_subjects
                .iter()
                .any(|filter_subject| filter_subject == subject)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(ServiceRuntimeError::MissingEventConsumerGroup {
            subject: subject.to_string(),
        }),
        [(group, binding)] => Ok(((*group).clone(), (*binding).clone())),
        _ => Err(ServiceRuntimeError::AmbiguousEventConsumerGroup {
            subject: subject.to_string(),
            groups: matches.iter().map(|(group, _)| (*group).clone()).collect(),
        }),
    }
}

fn is_missing_durable_event_consumer_error(error: &TrellisClientError) -> bool {
    let TrellisClientError::NatsRequest(message) = error else {
        return false;
    };

    let message = message.to_ascii_lowercase();
    message.contains("consumer not found")
        || message.contains("consumer does not exist")
        || message.contains("no consumer")
        || message.contains("consumer is paused")
        || message.contains("consumer paused")
}

fn missing_durable_event_consumer_is_retryable(
    error: &TrellisClientError,
    is_replay: bool,
    original_consumer_opened: bool,
) -> bool {
    is_missing_durable_event_consumer_error(error) && (is_replay || !original_consumer_opened)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{
        BootstrapBinding, EventConsumerReplay, EventConsumerReplayBinding,
        EventConsumerResourceBinding, KvResourceBinding, StoreResourceBinding,
    };
    use std::collections::BTreeMap;

    #[test]
    fn explicit_ephemeral_requires_a_declared_event_subscribe_need() {
        // A declared durable consumer alone grants no raw-subscribe authority.
        assert!(!ephemeral_event_authorized(&[], "Alpha"));
        assert!(!ephemeral_event_authorized(&["event:Beta"], "Alpha"));
        assert!(ephemeral_event_authorized(&["event:Alpha"], "Alpha"));
        assert!(ephemeral_event_authorized(
            &["event:Beta", "event:Alpha"],
            "Alpha"
        ));
    }

    fn binding() -> CoreBootstrapBinding {
        CoreBootstrapBinding::new(
            BootstrapBinding {
                contract_id: "example.service@v1".to_string(),
                digest: "sha256:test".to_string(),
            },
            ServiceResourceBindings {
                event_consumers: BTreeMap::from([(
                    "projection".to_string(),
                    EventConsumerResourceBinding {
                        resource_id: "consumer/projection".to_string(),
                        stream: "trellis".to_string(),
                        consumer_name: "svc-projection".to_string(),
                        filter_subjects: vec!["events.v1.Billing.Paid".to_string()],
                        replay: EventConsumerReplay::New,
                        concurrency: 1,
                        ack_wait_ms: 30_000,
                        max_deliver: 5,
                        backoff_ms: vec![1_000, 5_000],
                        replay_binding: EventConsumerReplayBinding {
                            stream: "trellis-replay".to_string(),
                            consumer_name: "svc-projection-replay".to_string(),
                        },
                    },
                )]),
                jobs: None,
                kv: BTreeMap::from([(
                    "drafts".to_string(),
                    KvResourceBinding {
                        bucket: "svc_drafts".to_string(),
                        history: 3,
                        max_value_bytes: Some(4096),
                        ttl_ms: 60_000,
                    },
                )]),
                store: BTreeMap::from([(
                    "evidence".to_string(),
                    StoreResourceBinding {
                        name: "svc_evidence".to_string(),
                        max_object_bytes: Some(8192),
                        max_total_bytes: None,
                        ttl_ms: 0,
                    },
                )]),
            },
        )
    }

    fn event_consumer_binding(subjects: &[&str]) -> EventConsumerResourceBinding {
        EventConsumerResourceBinding {
            resource_id: "consumer/test".to_string(),
            stream: "trellis".to_string(),
            consumer_name: "consumer".to_string(),
            filter_subjects: subjects
                .iter()
                .map(|subject| (*subject).to_string())
                .collect(),
            replay: EventConsumerReplay::New,
            concurrency: 1,
            ack_wait_ms: 30_000,
            max_deliver: 5,
            backoff_ms: vec![1_000, 5_000],
            replay_binding: EventConsumerReplayBinding {
                stream: "trellis-replay".to_string(),
                consumer_name: "consumer-replay".to_string(),
            },
        }
    }

    #[test]
    fn resolve_event_consumer_binding_infers_unique_group() {
        let bindings = BTreeMap::from([(
            "projection".to_string(),
            event_consumer_binding(&["events.v1.Billing.Paid"]),
        )]);

        let (group, binding) =
            resolve_event_consumer_binding(&bindings, "events.v1.Billing.Paid", None)
                .expect("binding resolves");

        assert_eq!(group, "projection");
        assert_eq!(binding.consumer_name, "consumer");
    }

    #[test]
    fn core_bootstrap_maps_event_consumer_concurrency() {
        let mut resources = binding().resource_bindings();
        resources
            .event_consumers
            .get_mut("projection")
            .expect("projection event consumer binding")
            .concurrency = 4;

        assert_eq!(resources.event_consumers["projection"].concurrency, 4);
    }

    #[test]
    fn durable_event_listener_concurrency_enforces_group_agreement() {
        assert!(validate_event_listener_concurrency("projection", 4, Some(4)).is_ok());
        assert!(matches!(
            validate_event_listener_concurrency("projection", 2, Some(4)),
            Err(ServiceRuntimeError::EventListenerConcurrencyMismatch {
                group,
                existing: 4,
                requested: 2
            }) if group == "projection"
        ));
        assert!(matches!(
            validate_event_listener_concurrency("projection", 0, None),
            Err(ServiceRuntimeError::InvalidEventListenerConcurrency {
                group,
                concurrency: 0
            }) if group == "projection"
        ));
    }

    #[test]
    fn resolve_event_consumer_binding_rejects_invalid_group_selection() {
        let bindings = BTreeMap::from([(
            "projection".to_string(),
            event_consumer_binding(&["events.v1.Billing.Paid"]),
        )]);

        assert!(matches!(
            resolve_event_consumer_binding(&bindings, "events.v1.Missing", None),
            Err(ServiceRuntimeError::MissingEventConsumerGroup { subject })
                if subject == "events.v1.Missing"
        ));
        assert!(matches!(
            resolve_event_consumer_binding(
                &bindings,
                "events.v1.Billing.Paid",
                Some("missing"),
            ),
            Err(ServiceRuntimeError::EventConsumerGroupNotFound { group })
                if group == "missing"
        ));
        assert!(matches!(
            resolve_event_consumer_binding(
                &bindings,
                "events.v1.Other",
                Some("projection"),
            ),
            Err(ServiceRuntimeError::EventConsumerGroupSubjectMismatch { group, subject })
                if group == "projection" && subject == "events.v1.Other"
        ));
    }

    #[test]
    fn resolve_event_consumer_binding_requires_group_for_ambiguous_match() {
        let bindings = BTreeMap::from([
            (
                "first".to_string(),
                event_consumer_binding(&["events.v1.Billing.Paid"]),
            ),
            (
                "second".to_string(),
                event_consumer_binding(&["events.v1.Billing.Paid"]),
            ),
        ]);

        assert!(matches!(
            resolve_event_consumer_binding(
                &bindings,
                "events.v1.Billing.Paid",
                None,
            ),
            Err(ServiceRuntimeError::AmbiguousEventConsumerGroup { subject, groups })
                if subject == "events.v1.Billing.Paid"
                    && groups == vec!["first".to_string(), "second".to_string()]
        ));
    }

    #[test]
    fn is_missing_durable_event_consumer_error_matches_only_missing_consumer_requests() {
        assert!(is_missing_durable_event_consumer_error(
            &TrellisClientError::NatsRequest("consumer not found".to_string())
        ));
        assert!(is_missing_durable_event_consumer_error(
            &TrellisClientError::NatsRequest("Consumer does not exist".to_string())
        ));
        assert!(is_missing_durable_event_consumer_error(
            &TrellisClientError::NatsRequest("no consumer available".to_string())
        ));
        assert!(!is_missing_durable_event_consumer_error(
            &TrellisClientError::NatsRequest("permissions violation".to_string())
        ));
        assert!(!is_missing_durable_event_consumer_error(
            &TrellisClientError::Timeout
        ));
    }

    #[test]
    fn deleting_an_opened_original_consumer_is_fatal() {
        let deleted = TrellisClientError::NatsRequest("consumer not found".to_string());

        assert!(missing_durable_event_consumer_is_retryable(
            &deleted, false, false
        ));
        assert!(!missing_durable_event_consumer_is_retryable(
            &deleted, false, true
        ));
        assert!(missing_durable_event_consumer_is_retryable(
            &deleted, true, true
        ));
    }

    #[test]
    fn durable_event_progress_interval_has_one_millisecond_minimum() {
        assert_eq!(
            durable_event_progress_interval(Duration::from_millis(1)),
            Duration::from_millis(1)
        );
        assert_eq!(
            durable_event_progress_interval(Duration::from_millis(2)),
            Duration::from_millis(1)
        );
        assert_eq!(
            durable_event_progress_interval(Duration::from_millis(6)),
            Duration::from_millis(2)
        );
    }
}
