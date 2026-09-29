use std::{fmt, pin::Pin, task::Poll, time::Duration, time::Instant};

use async_nats::jetstream::kv::{CreateErrorKind, Operation, UpdateErrorKind};
use async_nats::jetstream::object_store::{GetErrorKind, PutErrorKind};
use bytes::Bytes;
use futures_util::{Stream, StreamExt, TryStreamExt};
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
    Fixed(Box<async_nats::jetstream::object_store::ObjectStore>),
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
}

/// One object opened for streaming, pinned to its generation for the whole
/// stream: the lease travels with the reader so a superseded generation cannot
/// close underneath an in-progress read. The lease is released at true EOF,
/// even if the caller retains the exhausted reader.
pub(crate) struct BoundStoreObject {
    _lease: Option<TransportLease>,
    object: async_nats::jetstream::object_store::Object,
}

impl BoundStoreObject {
    pub(crate) fn info(&self) -> &async_nats::jetstream::object_store::ObjectInfo {
        self.object.info()
    }
}

impl tokio::io::AsyncRead for BoundStoreObject {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let capacity = buf.remaining();
        let polled = Pin::new(&mut this.object).poll_read(cx, buf);
        let terminal = match &polled {
            // True EOF fills no bytes when the caller offered capacity; a
            // zero-capacity read is not EOF. A read error does not end the
            // async-nats object reader (it can continue after a transient
            // subscription error), so it must not release the generation.
            Poll::Ready(Ok(())) => capacity > 0 && buf.filled().is_empty(),
            Poll::Ready(Err(_)) | Poll::Pending => false,
        };
        if terminal {
            this._lease = None;
        }
        polled
    }
}

impl BoundStoreResourceClient {
    /// Build one store client pinned to an already-open object store.
    #[doc(hidden)]
    pub fn new(store: async_nats::jetstream::object_store::ObjectStore) -> Self {
        Self {
            backend: StoreBackend::Fixed(Box::new(store)),
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
            StoreBackend::Fixed(_) => None,
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
            StoreBackend::Fixed(store) => Ok(StoreGuard {
                _lease: None,
                store: store.as_ref().clone(),
            }),
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
        let found: Option<(StoreGuard, async_nats::jetstream::object_store::Object)> = self
            .bounded("store open object", |deadline| async move {
                let guard = self.guard(ResourceTransportAction::Read, deadline).await?;
                match guard.store.get(key).await {
                    Ok(object) => Ok(Some((guard, object))),
                    Err(error) if error.kind() == GetErrorKind::NotFound => Ok(None),
                    Err(error) => Err(nats_error(error)),
                }
            })
            .await?;
        match found {
            Some((guard, object)) => {
                let StoreGuard { _lease, .. } = guard;
                Ok(Some(BoundStoreObject { _lease, object }))
            }
            None => Ok(None),
        }
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
        let found: Option<(StoreGuard, async_nats::jetstream::object_store::Object)> = self
            .bounded("store read", |deadline| async move {
                let guard = self.guard(ResourceTransportAction::Read, deadline).await?;
                match guard.store.get(key).await {
                    Ok(object) => Ok(Some((guard, object))),
                    Err(error) if error.kind() == GetErrorKind::NotFound => Ok(None),
                    Err(error) => Err(nats_error(error)),
                }
            })
            .await?;
        let Some((guard, mut object)) = found else {
            return Ok(None);
        };
        let info = store_object_info(object.info())?;
        tokio::io::copy(&mut object, writer)
            .await
            .map_err(nats_error)?;
        drop(guard);
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
