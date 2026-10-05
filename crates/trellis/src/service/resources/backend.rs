use std::{fmt, pin::Pin, task::Poll, time::Duration, time::Instant};

use async_nats::jetstream::kv::{CreateErrorKind, Operation, UpdateErrorKind};
use async_nats::jetstream::object_store::{InfoErrorKind, PutErrorKind};
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt, TryStreamExt};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout_at;

use super::{
    ensure_existing_kv_binding, ensure_existing_store_binding, nats_error, KvResourceBinding,
    KvResourceClient, KvResourceOperation, RawKvResourceEntry, ServerError, StoreObjectInfo,
    StoreResourceBinding, StoreResourceClient,
};
use crate::client::{
    resource_action_marker, ResourceTransportAction, ResourceTransportKind,
    TransportGenerationManager, TransportLease,
};

/// Error for an operation that did not finish inside its absolute budget.
fn resource_timeout(operation: &str) -> ServerError {
    ServerError::Nats(format!("resource operation timed out: {operation}"))
}

/// Acquire one generation lease for the exact resource subject family of one
/// operation. The caller threads one absolute budget through acquisition, the
/// backend open, and every await of the finite operation, so no piece gets a
/// fresh library budget. A handle holds no generation of its own; each physical
/// exchange that needs the resource acquires a suitable generation within that
/// budget and releases it on completion, so a resource approved after connect
/// becomes usable without recreating the logical handle.
#[derive(Clone)]
struct ManagedKvTransport {
    manager: TransportGenerationManager,
    binding: KvResourceBinding,
    timeout_ms: u64,
}

impl ManagedKvTransport {
    async fn lease(
        &self,
        action: ResourceTransportAction,
        deadline: Instant,
    ) -> Result<TransportLease, ServerError> {
        let marker =
            resource_action_marker(ResourceTransportKind::Kv, &self.binding.bucket, action);
        self.manager
            .acquire_for(&[marker], &[], deadline)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))
    }
}

/// Acquire one generation lease for one object-store operation under the same
/// absolute-budget rule as KV.
#[derive(Clone)]
struct ManagedStoreTransport {
    manager: TransportGenerationManager,
    binding: StoreResourceBinding,
    timeout_ms: u64,
    /// Runtime-provisioned buckets (operation staging) are always authorized on
    /// the current generation; contract resources require the exact store action
    /// marker so a call adopts the generation that covers the resource.
    runtime_provisioned: bool,
}

impl ManagedStoreTransport {
    async fn lease(
        &self,
        action: ResourceTransportAction,
        deadline: Instant,
    ) -> Result<TransportLease, ServerError> {
        let requirement = if self.runtime_provisioned {
            Vec::new()
        } else {
            vec![resource_action_marker(
                ResourceTransportKind::Store,
                &self.binding.name,
                action,
            )]
        };
        self.manager
            .acquire_for(&requirement, &[], deadline)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))
    }
}

#[derive(Clone)]
enum StoreBackend {
    Fixed {
        client: async_nats::Client,
        bucket: String,
    },
    Managed(ManagedStoreTransport),
}

/// Concrete KV client used by connected service resources.
#[derive(Clone)]
pub struct BoundKvResourceClient {
    transport: ManagedKvTransport,
}

impl fmt::Debug for BoundKvResourceClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundKvResourceClient")
            .finish_non_exhaustive()
    }
}

/// One acquired KV store for the duration of a single operation.
struct KvStoreGuard {
    _lease: Option<TransportLease>,
    store: async_nats::jetstream::kv::Store,
}

/// Watch stream for connected KV resources; holds the generation lease that
/// keeps its subscription alive and releases it when the stream ends.
pub struct BoundKvWatch {
    _lease: Option<TransportLease>,
    inner: async_nats::jetstream::kv::Watch,
}

impl fmt::Debug for BoundKvWatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundKvWatch")
            .finish_non_exhaustive()
    }
}

impl Stream for BoundKvWatch {
    type Item = Result<RawKvResourceEntry, ServerError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let polled = Pin::new(&mut self.inner).poll_next(cx);
        let mapped = polled
            .map(|entry| entry.map(|entry| entry.map(kv_entry_from_nats).map_err(nats_error)));
        // async-nats yields a transient `Err` item without ending the stream
        // (`consumer::push::Ordered` recreates the subscriber and keeps
        // yielding), so only a true end releases the generation.
        if matches!(&mapped, Poll::Ready(None)) {
            self.get_mut()._lease = None;
        }
        mapped
    }
}

impl BoundKvResourceClient {
    pub(crate) fn managed(
        manager: TransportGenerationManager,
        binding: KvResourceBinding,
        timeout_ms: u64,
    ) -> Self {
        Self {
            transport: ManagedKvTransport {
                manager,
                binding,
                timeout_ms,
            },
        }
    }

    /// The absolute budget for one call: acquisition, backend open, every await
    /// of the finite operation.
    fn deadline(&self) -> Instant {
        Instant::now() + Duration::from_millis(self.transport.timeout_ms)
    }

    /// Acquire the store for one operation under `deadline`.
    async fn guard(
        &self,
        action: ResourceTransportAction,
        deadline: Instant,
    ) -> Result<KvStoreGuard, ServerError> {
        let lease = self.transport.lease(action, deadline).await?;
        let store = open_kv_store(lease.nats(), &self.transport.binding).await?;
        Ok(KvStoreGuard {
            _lease: Some(lease),
            store,
        })
    }
}

impl KvResourceClient for BoundKvResourceClient {
    type Watch = BoundKvWatch;

    async fn get_entry(&self, key: &str) -> Result<Option<RawKvResourceEntry>, ServerError> {
        let deadline = self.deadline();
        timeout_at(tokio::time::Instant::from_std(deadline), async {
            let guard = self.guard(ResourceTransportAction::Read, deadline).await?;
            authoritative_entry(&guard.store, key).await
        })
        .await
        .map_err(|_| resource_timeout("kv get"))?
    }

    async fn create(&self, key: &str, value: Bytes) -> Result<RawKvResourceEntry, ServerError> {
        let deadline = self.deadline();
        timeout_at(tokio::time::Instant::from_std(deadline), async {
            let guard = self.guard(ResourceTransportAction::Write, deadline).await?;
            match guard.store.create(key, value).await {
                Ok(revision) => entry_at(&guard.store, key, revision).await,
                Err(error) if error.kind() == CreateErrorKind::AlreadyExists => {
                    Err(kv_revision_mismatch(&guard.store, key, 0).await)
                }
                Err(error) => Err(nats_error(error)),
            }
        })
        .await
        .map_err(|_| resource_timeout("kv create"))?
    }

    async fn put(&self, key: &str, value: Bytes) -> Result<RawKvResourceEntry, ServerError> {
        let deadline = self.deadline();
        timeout_at(tokio::time::Instant::from_std(deadline), async {
            let guard = self.guard(ResourceTransportAction::Write, deadline).await?;
            let revision = guard.store.put(key, value).await.map_err(nats_error)?;
            entry_at(&guard.store, key, revision).await
        })
        .await
        .map_err(|_| resource_timeout("kv put"))?
    }

    async fn replace(
        &self,
        key: &str,
        value: Bytes,
        revision: u64,
    ) -> Result<RawKvResourceEntry, ServerError> {
        let deadline = self.deadline();
        timeout_at(tokio::time::Instant::from_std(deadline), async {
            let guard = self.guard(ResourceTransportAction::Write, deadline).await?;
            match guard.store.update(key, value, revision).await {
                Ok(revision) => entry_at(&guard.store, key, revision).await,
                Err(error) if error.kind() == UpdateErrorKind::WrongLastRevision => {
                    Err(kv_revision_mismatch(&guard.store, key, revision).await)
                }
                Err(error) => Err(nats_error(error)),
            }
        })
        .await
        .map_err(|_| resource_timeout("kv replace"))?
    }

    async fn delete(&self, key: &str) -> Result<(), ServerError> {
        let deadline = self.deadline();
        timeout_at(tokio::time::Instant::from_std(deadline), async {
            let guard = self.guard(ResourceTransportAction::Write, deadline).await?;
            if authoritative_entry(&guard.store, key)
                .await?
                .is_none_or(|entry| entry.operation == KvResourceOperation::Delete)
            {
                return Ok(());
            }
            guard.store.delete(key).await.map_err(nats_error)
        })
        .await
        .map_err(|_| resource_timeout("kv delete"))?
    }

    async fn delete_revision(&self, key: &str, revision: u64) -> Result<(), ServerError> {
        let deadline = self.deadline();
        timeout_at(tokio::time::Instant::from_std(deadline), async {
            let guard = self.guard(ResourceTransportAction::Write, deadline).await?;
            match guard
                .store
                .delete_expect_revision(key, Some(revision))
                .await
            {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == UpdateErrorKind::WrongLastRevision => {
                    Err(kv_revision_mismatch(&guard.store, key, revision).await)
                }
                Err(error) => Err(nats_error(error)),
            }
        })
        .await
        .map_err(|_| resource_timeout("kv delete"))?
    }

    async fn history(&self, key: &str) -> Result<Vec<RawKvResourceEntry>, ServerError> {
        let deadline = self.deadline();
        timeout_at(tokio::time::Instant::from_std(deadline), async {
            let guard = self.guard(ResourceTransportAction::Read, deadline).await?;
            let history = guard.store.history(key).await.map_err(nats_error)?;
            history
                .map(|entry| entry.map(kv_entry_from_nats).map_err(nats_error))
                .try_collect()
                .await
        })
        .await
        .map_err(|_| resource_timeout("kv history"))?
    }

    async fn watch(&self, key: &str) -> Result<Self::Watch, ServerError> {
        let deadline = self.deadline();
        // Only watch setup is bounded by the budget; the stream lifetime is not.
        timeout_at(tokio::time::Instant::from_std(deadline), async {
            let guard = self.guard(ResourceTransportAction::Read, deadline).await?;
            let KvStoreGuard { _lease, store } = guard;
            let inner = store.watch(key).await.map_err(nats_error)?;
            Ok(BoundKvWatch { _lease, inner })
        })
        .await
        .map_err(|_| resource_timeout("kv watch setup"))?
    }
}

pub(super) async fn open_kv_store(
    nats: &async_nats::Client,
    binding: &KvResourceBinding,
) -> Result<async_nats::jetstream::kv::Store, ServerError> {
    let context = async_nats::jetstream::new(nats.clone());
    let store = context
        .get_key_value(binding.bucket.clone())
        .await
        .map_err(nats_error)?;
    ensure_existing_kv_binding(&store, binding).await?;
    Ok(store)
}

async fn kv_revision_mismatch(
    store: &async_nats::jetstream::kv::Store,
    key: &str,
    expected: u64,
) -> ServerError {
    let actual = authoritative_entry(store, key)
        .await
        .ok()
        .flatten()
        .map(|entry| entry.revision);
    ServerError::KvRevisionMismatch {
        key: key.to_string(),
        expected,
        actual,
    }
}

async fn entry_at(
    store: &async_nats::jetstream::kv::Store,
    key: &str,
    revision: u64,
) -> Result<RawKvResourceEntry, ServerError> {
    store
        .entry_for_revision(key.to_string(), revision)
        .await
        .map_err(nats_error)?
        .map(kv_entry_from_nats)
        .ok_or_else(|| ServerError::Nats(format!("KV write revision {revision} disappeared")))
}

async fn authoritative_entry(
    store: &async_nats::jetstream::kv::Store,
    key: &str,
) -> Result<Option<RawKvResourceEntry>, ServerError> {
    store
        .entry(key)
        .await
        .map(|entry| entry.map(kv_entry_from_nats))
        .map_err(nats_error)
}

fn kv_entry_from_nats(entry: async_nats::jetstream::kv::Entry) -> RawKvResourceEntry {
    let operation = match entry.operation {
        Operation::Put => KvResourceOperation::Put,
        Operation::Delete | Operation::Purge => KvResourceOperation::Delete,
    };
    RawKvResourceEntry {
        key: entry.key,
        value: (operation == KvResourceOperation::Put).then_some(entry.value),
        revision: entry.revision,
        timestamp: entry.created,
        operation,
    }
}

/// Concrete object-store client used by connected service resources.
#[derive(Clone)]
pub struct BoundStoreResourceClient {
    backend: StoreBackend,
}

/// One acquired object store for the duration of a single operation.
struct StoreGuard {
    _lease: Option<TransportLease>,
    store: async_nats::jetstream::object_store::ObjectStore,
    context: async_nats::jetstream::Context,
    bucket: String,
    read_batch_bytes: usize,
}

/// One object opened for streaming, pinned to its generation for the whole
/// stream: the lease travels with the reader so a superseded generation cannot
/// close underneath an in-progress read. The lease is released at true EOF,
/// even if the caller retains the exhausted reader.
pub(crate) struct BoundStoreObject {
    info: async_nats::jetstream::object_store::ObjectInfo,
    chunks: futures_util::stream::BoxStream<'static, std::io::Result<Bytes>>,
    pending: Bytes,
}

impl BoundStoreObject {
    pub(crate) fn info(&self) -> &async_nats::jetstream::object_store::ObjectInfo {
        &self.info
    }
}

impl tokio::io::AsyncRead for BoundStoreObject {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if !this.pending.is_empty() {
                let length = buf.remaining().min(this.pending.len());
                buf.put_slice(&this.pending.split_to(length));
                return Poll::Ready(Ok(()));
            }
            match this.chunks.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => this.pending = bytes,
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// The consumer and its admitting generation live together, including cleanup.
struct StoreReadState {
    runtime: tokio::runtime::Handle,
    guard: StoreGuard,
    consumer: Option<
        async_nats::jetstream::consumer::Consumer<async_nats::jetstream::consumer::pull::Config>,
    >,
    stream: String,
    subject: String,
    name: Option<String>,
    chunks: usize,
    size: usize,
    received_chunks: usize,
    received_bytes: usize,
    stream_sequence: u64,
    consumer_epoch_chunks: usize,
    digest: Vec<u8>,
    hasher: Sha256,
}

impl Drop for StoreReadState {
    fn drop(&mut self) {
        if let Some(name) = self.name.take() {
            let context = self.guard.context.clone();
            let stream = self.stream.clone();
            let lease = self.guard._lease.take();
            self.runtime.spawn(async move {
                let _lease = lease;
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    context.delete_consumer_from_stream(name, stream),
                )
                .await;
            });
        }
    }
}

impl BoundStoreResourceClient {
    /// Bind one already-provisioned bucket to its owning connection.
    #[doc(hidden)]
    pub fn new(client: async_nats::Client, bucket: impl Into<String>) -> Self {
        Self {
            backend: StoreBackend::Fixed {
                client,
                bucket: bucket.into(),
            },
        }
    }

    /// Build one store client that acquires a suitable generation per operation.
    pub(crate) fn managed(
        manager: TransportGenerationManager,
        binding: StoreResourceBinding,
        timeout_ms: u64,
    ) -> Self {
        Self {
            backend: StoreBackend::Managed(ManagedStoreTransport {
                manager,
                binding,
                timeout_ms,
                runtime_provisioned: false,
            }),
        }
    }

    /// Build one store client for a runtime-provisioned bucket such as operation
    /// staging: each call leases the current generation, which is always
    /// authorized for the bucket, instead of a contract resource marker.
    pub(crate) fn managed_current(
        manager: TransportGenerationManager,
        name: String,
        timeout_ms: u64,
    ) -> Self {
        Self {
            backend: StoreBackend::Managed(ManagedStoreTransport {
                manager,
                binding: StoreResourceBinding {
                    name,
                    max_object_bytes: None,
                    max_total_bytes: None,
                    ttl_ms: 0,
                },
                timeout_ms,
                runtime_provisioned: true,
            }),
        }
    }

    /// The absolute budget for one call when this client is managed.
    fn deadline(&self) -> Option<Instant> {
        match &self.backend {
            StoreBackend::Fixed { .. } => None,
            StoreBackend::Managed(transport) => {
                Some(Instant::now() + Duration::from_millis(transport.timeout_ms))
            }
        }
    }

    /// Run one bounded operation: the whole `run` future (acquisition, open,
    /// every follow-up await) shares one absolute deadline when managed.
    async fn bounded<F, Fut, T>(&self, operation: &'static str, run: F) -> Result<T, ServerError>
    where
        F: FnOnce(Option<Instant>) -> Fut,
        Fut: std::future::Future<Output = Result<T, ServerError>>,
    {
        match self.deadline() {
            Some(deadline) => {
                let limit = tokio::time::Instant::from_std(deadline);
                timeout_at(limit, run(Some(deadline)))
                    .await
                    .map_err(|_| resource_timeout(operation))?
            }
            None => run(None).await,
        }
    }

    async fn guard(
        &self,
        action: ResourceTransportAction,
        deadline: Option<Instant>,
    ) -> Result<StoreGuard, ServerError> {
        match &self.backend {
            StoreBackend::Fixed { client, bucket } => {
                let context = async_nats::jetstream::new(client.clone());
                let store = context.get_object_store(bucket).await.map_err(nats_error)?;
                Ok(StoreGuard {
                    _lease: None,
                    store,
                    context,
                    bucket: bucket.clone(),
                    read_batch_bytes: client.server_info().max_payload.saturating_add(8192),
                })
            }
            StoreBackend::Managed(transport) => {
                let deadline = deadline.unwrap_or_else(|| {
                    Instant::now() + Duration::from_millis(transport.timeout_ms)
                });
                let lease = transport.lease(action, deadline).await?;
                let store = if transport.runtime_provisioned {
                    // A runtime-provisioned bucket (operation staging) has no
                    // contract binding to validate against.
                    open_runtime_object_store(lease.nats(), &transport.binding.name).await?
                } else {
                    open_object_store(lease.nats(), &transport.binding).await?
                };
                Ok(StoreGuard {
                    context: async_nats::jetstream::new(lease.nats().clone()),
                    bucket: transport.binding.name.clone(),
                    read_batch_bytes: lease.nats().server_info().max_payload.saturating_add(8192),
                    _lease: Some(lease),
                    store,
                })
            }
        }
    }

    /// Open one object for streaming. The returned reader owns the generation
    /// lease; acquisition and the metadata get share the call budget while the
    /// streamed body keeps its ordinary semantics.
    pub(crate) async fn open(&self, key: &str) -> Result<Option<BoundStoreObject>, ServerError> {
        self.bounded("store open object", |deadline| async move {
            let guard = self.guard(ResourceTransportAction::Read, deadline).await?;
            let info = match guard.store.info(key).await {
                Ok(info) if !info.deleted => info,
                Ok(_) => return Ok(None),
                Err(error) if error.kind() == InfoErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(nats_error(error)),
            };
            if info.bucket != guard.bucket
                || info.name != key
                || info
                    .options
                    .as_ref()
                    .is_some_and(|options| options.link.is_some())
                || info.nuid.is_empty()
                || info
                    .nuid
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '.' | '*' | '>'))
            {
                return Err(nats_error(
                    "invalid object metadata or unsupported object link",
                ));
            }
            let encoded = info
                .digest
                .as_deref()
                .and_then(|digest| digest.strip_prefix("SHA-256="))
                .ok_or_else(|| nats_error("missing object SHA-256 digest"))?;
            let digest = URL_SAFE
                .decode(encoded)
                .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))
                .map_err(nats_error)?;
            if digest.len() != 32 || (info.size == 0) != (info.chunks == 0) {
                return Err(nats_error("invalid object size, chunk count, or digest"));
            }
            if info.size == 0 {
                if Sha256::digest([]).as_slice() != digest {
                    return Err(nats_error("object digest mismatch"));
                }
                return Ok(Some(BoundStoreObject {
                    info,
                    chunks: futures_util::stream::empty().boxed(),
                    pending: Bytes::new(),
                }));
            }
            let stream = format!("OBJ_{}", guard.bucket);
            let name = format!("tr_store_{}", ulid::Ulid::new());
            let mut state = StoreReadState {
                runtime: tokio::runtime::Handle::current(),
                subject: format!("$O.{}.C.{}", guard.bucket, info.nuid),
                guard,
                consumer: None,
                stream,
                name: Some(name.clone()),
                chunks: info.chunks,
                size: info.size,
                received_chunks: 0,
                received_bytes: 0,
                stream_sequence: 0,
                consumer_epoch_chunks: 0,
                digest,
                hasher: Sha256::new(),
            };
            let consumer = state
                .guard
                .context
                .create_consumer_on_stream(
                    async_nats::jetstream::consumer::pull::Config {
                        name: Some(name.clone()),
                        filter_subject: format!("$O.{}.C.{}", state.guard.bucket, info.nuid),
                        ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
                        max_waiting: 1,
                        max_batch: 1,
                        max_bytes: state.guard.read_batch_bytes.try_into().map_err(nats_error)?,
                        inactive_threshold: Duration::from_secs(300),
                        ..Default::default()
                    },
                    &state.stream,
                )
                .await
                .map_err(nats_error)?;
            state.consumer = Some(consumer);
            let chunks = futures_util::stream::try_unfold(state, |mut state| async move {
                if state.received_chunks == state.chunks {
                    let deletion = state
                        .guard
                        .context
                        .delete_consumer_from_stream(
                            state.name.as_ref().expect("active store consumer"),
                            &state.stream,
                        )
                        .await;
                    if let Err(error) = deletion {
                        let absent = matches!(error.kind(), async_nats::jetstream::stream::ConsumerErrorKind::JetStream(error)
                            if error.error_code() == async_nats::jetstream::ErrorCode::CONSUMER_NOT_FOUND
                                || error.error_code() == async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND);
                        if !absent {
                            return Err(std::io::Error::other(error));
                        }
                    }
                    drop(state.name.take());
                    return Ok(None);
                }
                // async-nats 0.50's finite Batch reports byte-limit batch
                // completion as an error. One message avoids that ambiguity
                // while max_bytes still bounds even historical large chunks.
                // Ephemeral consumers may be removed while the caller holds a
                // chunk. Reopen at the exact next stream sequence on demand;
                // no heartbeat task or idle deadline belongs to the reader.
                match state.guard.context.get_consumer_from_stream::<async_nats::jetstream::consumer::pull::Config, _, _>(state.name.as_ref().expect("active store consumer"), &state.stream).await {
                    Ok(consumer) => state.consumer = Some(consumer),
                    Err(error) if matches!(error.kind(), async_nats::jetstream::stream::ConsumerErrorKind::JetStream(error) if error.error_code() == async_nats::jetstream::ErrorCode::CONSUMER_NOT_FOUND) => {
                        let consumer = state.guard.context.create_consumer_on_stream(
                            async_nats::jetstream::consumer::pull::Config {
                                name: state.name.clone(),
                                filter_subject: state.subject.clone(),
                                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::ByStartSequence { start_sequence: state.stream_sequence + 1 },
                                ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
                                max_waiting: 1,
                                max_batch: 1,
                                max_bytes: state.guard.read_batch_bytes.try_into().map_err(std::io::Error::other)?,
                                inactive_threshold: Duration::from_secs(300),
                                ..Default::default()
                            }, &state.stream).await.map_err(std::io::Error::other)?;
                        state.consumer = Some(consumer);
                        state.consumer_epoch_chunks = state.received_chunks;
                    }
                    Err(error) => return Err(std::io::Error::other(error)),
                }
                let mut batch = state
                    .consumer
                    .as_ref()
                    .expect("opened store consumer")
                    .fetch()
                    .max_messages(1)
                    .max_bytes(state.guard.read_batch_bytes)
                    .expires(Duration::from_secs(5))
                    .messages()
                    .await
                    .map_err(std::io::Error::other)?;
                let message = batch
                    .next()
                    .await
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "object chunks missing",
                        )
                    })?
                    .map_err(std::io::Error::other)?;
                let message_info = message.info().map_err(std::io::Error::other)?;
                if message_info.consumer_sequence != (state.received_chunks - state.consumer_epoch_chunks) as u64 + 1
                    || message_info.stream_sequence <= state.stream_sequence {
                    return Err(std::io::Error::other("object chunk sequence mismatch"));
                }
                state.received_chunks += 1;
                state.stream_sequence = message_info.stream_sequence;
                state.received_bytes = state
                    .received_bytes
                    .checked_add(message.payload.len())
                    .ok_or_else(|| std::io::Error::other("object size overflow"))?;
                if state.received_bytes > state.size {
                    return Err(std::io::Error::other("object size mismatch"));
                }
                state.hasher.update(&message.payload);
                if state.received_chunks == state.chunks
                    && (state.received_bytes != state.size
                        || message_info.pending != 0
                        || state.hasher.clone().finalize().as_slice() != state.digest)
                {
                    return Err(std::io::Error::other(
                        "object size, chunk count, or digest mismatch",
                    ));
                }
                Ok(Some((message.payload.clone(), state)))
            })
            .boxed();
            Ok(Some(BoundStoreObject {
                info,
                chunks,
                pending: Bytes::new(),
            }))
        })
        .await
    }
}

impl fmt::Debug for BoundStoreResourceClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundStoreResourceClient")
            .finish_non_exhaustive()
    }
}

impl StoreResourceClient for BoundStoreResourceClient {
    async fn read_into<W>(
        &self,
        key: &str,
        writer: &mut W,
    ) -> Result<Option<StoreObjectInfo>, ServerError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        // Acquisition, open, and the metadata get share the call budget; the
        // streamed body keeps its ordinary semantics and keeps the lease.
        let Some(mut object) = self.open(key).await? else {
            return Ok(None);
        };
        let info = store_object_info(object.info())?;
        tokio::io::copy(&mut object, writer)
            .await
            .map_err(nats_error)?;
        Ok(Some(info))
    }

    async fn write_from<R>(&self, key: &str, reader: &mut R) -> Result<StoreObjectInfo, ServerError>
    where
        R: AsyncRead + Unpin + Send,
    {
        // Acquisition and open are bounded; the put streams the reader and keeps
        // its ordinary semantics.
        let guard = self
            .bounded("store write", |deadline| {
                self.guard(ResourceTransportAction::Write, deadline)
            })
            .await?;
        let mut reader = reader;
        match guard.store.put(key, &mut reader).await {
            Ok(info) => store_object_info(&info),
            Err(error) if error.kind() == PutErrorKind::PublishMetadata => {
                Err(ServerError::StoreCommitIndeterminate {
                    key: key.to_string(),
                    message: error.to_string(),
                })
            }
            Err(error) if error.kind() == PutErrorKind::PurgeOldChunks => {
                Err(ServerError::StoreCommittedCleanupFailed {
                    key: key.to_string(),
                    message: error.to_string(),
                })
            }
            Err(error) => Err(nats_error(error)),
        }
    }

    async fn list(&self) -> Result<Vec<String>, ServerError> {
        self.bounded("store list", |deadline| async move {
            let guard = self.guard(ResourceTransportAction::Read, deadline).await?;
            let objects = guard.store.list().await.map_err(nats_error)?;
            objects
                .map(|object| object.map(|info| info.name).map_err(nats_error))
                .try_collect()
                .await
        })
        .await
    }

    async fn list_objects(&self) -> Result<Vec<StoreObjectInfo>, ServerError> {
        self.bounded("store list", |deadline| async move {
            let guard = self.guard(ResourceTransportAction::Read, deadline).await?;
            let objects = guard.store.list().await.map_err(nats_error)?;
            objects
                .map(|object| {
                    object
                        .map_err(nats_error)
                        .and_then(|info| store_object_info(&info))
                })
                .try_collect()
                .await
        })
        .await
    }

    async fn delete(&self, key: &str) -> Result<(), ServerError> {
        self.bounded("store delete", |deadline| async move {
            let guard = self.guard(ResourceTransportAction::Write, deadline).await?;
            guard.store.delete(key).await.map_err(nats_error)
        })
        .await
    }
}

async fn open_object_store(
    nats: &async_nats::Client,
    binding: &StoreResourceBinding,
) -> Result<async_nats::jetstream::object_store::ObjectStore, ServerError> {
    let context = async_nats::jetstream::new(nats.clone());
    let store = context
        .get_object_store(&binding.name)
        .await
        .map_err(nats_error)?;
    ensure_existing_store_binding(&context, binding).await?;
    Ok(store)
}

/// Open a runtime-provisioned bucket without a contract binding to validate.
async fn open_runtime_object_store(
    nats: &async_nats::Client,
    name: &str,
) -> Result<async_nats::jetstream::object_store::ObjectStore, ServerError> {
    async_nats::jetstream::new(nats.clone())
        .get_object_store(name)
        .await
        .map_err(nats_error)
}

fn store_object_info(
    info: &async_nats::jetstream::object_store::ObjectInfo,
) -> Result<StoreObjectInfo, ServerError> {
    Ok(StoreObjectInfo {
        key: info.name.clone(),
        size: u64::try_from(info.size)
            .map_err(|_| ServerError::Nats("store object size does not fit in u64".to_string()))?,
        digest: info.digest.clone(),
        modified_at: info.modified,
    })
}
