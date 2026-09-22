mod streaming;

use streaming::{GuardedUploadReader, UploadReadFailure};

use std::{
    fmt,
    future::{pending, Future},
    io::Cursor,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use bytes::Bytes;
use futures_util::{pin_mut, Stream, StreamExt};
use time::OffsetDateTime;
use tokio::io::{AsyncRead, AsyncWrite};

use super::{KvResourceBinding, ServerError, StoreResourceBinding};

pub(crate) mod backend;
use backend::{BoundKvResourceClient, BoundStoreResourceClient};

pub(crate) trait ResourceRuntimeClient {
    /// KV client type returned for a bound KV resource.
    type Kv: KvResourceClient;
    /// Object-store client type returned for a bound store resource.
    type Store: StoreResourceClient;

    /// Open the concrete KV bucket described by `binding`.
    fn open_kv(
        &self,
        binding: &KvResourceBinding,
    ) -> impl Future<Output = Result<Self::Kv, ServerError>> + Send;

    /// Open the concrete object-store bucket described by `binding`.
    fn open_store(
        &self,
        binding: &StoreResourceBinding,
    ) -> impl Future<Output = Result<Self::Store, ServerError>> + Send;
}

/// Raw operations required by a typed bound KV resource handle.
#[doc(hidden)]
pub trait KvResourceClient: Clone + fmt::Debug + Send + Sync + 'static {
    /// Watch stream type returned by this client.
    type Watch: Stream<Item = Result<RawKvResourceEntry, ServerError>> + Send + Unpin + 'static;

    /// Read the authoritative latest entry for `key`, including delete markers.
    fn get_entry(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<RawKvResourceEntry>, ServerError>> + Send;

    /// Create `key` only when no live value exists.
    fn create(
        &self,
        key: &str,
        value: Bytes,
    ) -> impl Future<Output = Result<RawKvResourceEntry, ServerError>> + Send;

    /// Persist `value` at `key` unconditionally.
    fn put(
        &self,
        key: &str,
        value: Bytes,
    ) -> impl Future<Output = Result<RawKvResourceEntry, ServerError>> + Send;

    /// Persist `value` at `key` only if `key` is still at `revision`.
    fn replace(
        &self,
        key: &str,
        value: Bytes,
        revision: u64,
    ) -> impl Future<Output = Result<RawKvResourceEntry, ServerError>> + Send;

    /// Delete `key` from this bucket.
    fn delete(&self, key: &str) -> impl Future<Output = Result<(), ServerError>> + Send;

    /// Delete `key` only if `key` is still at `revision`.
    fn delete_revision(
        &self,
        key: &str,
        revision: u64,
    ) -> impl Future<Output = Result<(), ServerError>> + Send;

    /// Return retained revisions for `key` in storage order, including tombstones.
    fn history(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Vec<RawKvResourceEntry>, ServerError>> + Send;

    /// Watch updates and deletes for one key.
    fn watch(&self, key: &str) -> impl Future<Output = Result<Self::Watch, ServerError>> + Send;
}

/// Operation that produced a KV entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvResourceOperation {
    /// Value bytes were written for the key.
    Put,
    /// The key was deleted or purged.
    Delete,
}

/// Raw KV entry retained behind generated typed facades.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct RawKvResourceEntry {
    /// Key for this entry.
    pub key: String,
    /// Raw value bytes for this revision.
    pub value: Option<Bytes>,
    /// Monotonic bucket revision for this entry.
    pub revision: u64,
    /// Timestamp assigned by the KV backend.
    pub timestamp: OffsetDateTime,
    /// Operation that produced this entry.
    pub operation: KvResourceOperation,
}

/// Typed KV entry preserving the backend revision, timestamp, and tombstone operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvResourceEntry<T> {
    /// Key for this entry.
    pub key: String,
    /// Current in-memory value for puts, or `None` for tombstones.
    pub value: Option<T>,
    /// Opaque backend revision.
    pub revision: crate::client::ResourceRevision,
    /// Timestamp assigned by the authoritative backend.
    pub timestamp: OffsetDateTime,
    /// Operation that produced this revision.
    pub operation: KvResourceOperation,
}

/// Typed KV read, history, or watch failure.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum KvResourceReadError {
    /// This cached handle no longer names the current usable resource binding.
    #[error("KV resource binding is no longer available")]
    Unavailable,
    /// One stored revision could not be decoded or migrated.
    #[error(transparent)]
    Codec(#[from] crate::client::ResourceCodecError),
    /// The backend operation failed.
    #[error("KV backend error: {0}")]
    Backend(String),
}

/// Typed KV write failure with the authoritative current entry on CAS conflict.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum KvResourceWriteError<T> {
    /// This cached handle no longer names the current usable resource binding.
    #[error("KV resource binding is no longer available")]
    Unavailable,
    /// The create or expected-revision condition did not hold.
    #[error("KV revision conflict")]
    Conflict {
        /// Authoritative current entry, or `None` when the key is absent.
        current: Option<KvResourceEntry<T>>,
    },
    /// Encoding, decoding, or migration failed.
    #[error(transparent)]
    Codec(#[from] crate::client::ResourceCodecError),
    /// The backend operation failed.
    #[error("KV backend error: {0}")]
    Backend(String),
}

/// Typed handle for one generated service-owned KV resource.
pub struct KvResourceHandle<T, C> {
    resource_name: Arc<str>,
    binding: KvResourceBinding,
    codec: crate::client::ResourceCodec<T>,
    client: C,
    availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
    generation: u64,
}

impl<T, C: Clone> Clone for KvResourceHandle<T, C> {
    fn clone(&self) -> Self {
        Self {
            resource_name: Arc::clone(&self.resource_name),
            binding: self.binding.clone(),
            codec: self.codec.clone(),
            client: self.client.clone(),
            availability: self.availability.clone(),
            generation: self.generation,
        }
    }
}

impl<T, C> fmt::Debug for KvResourceHandle<T, C>
where
    C: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KvResourceHandle")
            .field("resource_name", &self.resource_name)
            .field("binding", &self.binding)
            .field("codec", &self.codec)
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl<T, C> KvResourceHandle<T, C>
where
    T: crate::generated::Codec + Send + 'static,
    C: KvResourceClient,
{
    /// Create a typed handle from generated resource metadata and an opened backend.
    #[doc(hidden)]
    pub fn from_generated(
        resource_name: impl Into<Arc<str>>,
        binding: KvResourceBinding,
        codec: crate::client::ResourceCodec<T>,
        client: C,
        availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
    ) -> Self {
        let resource_name = resource_name.into();
        let generation = availability
            .borrow()
            .resource_generation(crate::generated::ResourceKind::Kv, &resource_name)
            .unwrap_or(0);
        Self {
            resource_name,
            binding,
            codec,
            client,
            availability,
            generation,
        }
    }

    /// Contract-local resource alias used to open this handle.
    pub fn resource_name(&self) -> &str {
        &self.resource_name
    }

    /// Concrete resource binding resolved during bootstrap.
    pub fn binding(&self) -> &KvResourceBinding {
        &self.binding
    }

    /// Read the current value, or `None` for an absent/deleted key.
    pub async fn get(&self, key: &str) -> Result<Option<T>, KvResourceReadError> {
        Ok(self.get_entry(key).await?.and_then(|entry| entry.value))
    }

    /// Read the authoritative current entry, including a tombstone.
    pub async fn get_entry(
        &self,
        key: &str,
    ) -> Result<Option<KvResourceEntry<T>>, KvResourceReadError> {
        self.ensure_current()?;
        match self.client.get_entry(key).await.map_err(backend_read)? {
            Some(entry) => self.project(entry).await.map(Some).map_err(Into::into),
            None => Ok(None),
        }
    }

    /// Create `key` only when no live value exists.
    pub async fn create(
        &self,
        key: &str,
        value: &T,
    ) -> Result<KvResourceEntry<T>, KvResourceWriteError<T>> {
        self.write(key, value, None).await
    }

    /// Persist `value` unconditionally.
    pub async fn put(
        &self,
        key: &str,
        value: &T,
    ) -> Result<KvResourceEntry<T>, KvResourceWriteError<T>> {
        self.write(key, value, Some(None)).await
    }

    /// Replace `key` only when `revision` remains current.
    pub async fn replace(
        &self,
        key: &str,
        revision: crate::client::ResourceRevision,
        value: &T,
    ) -> Result<KvResourceEntry<T>, KvResourceWriteError<T>> {
        self.write(key, value, Some(Some(revision))).await
    }

    /// Delete unconditionally or only at the supplied revision.
    pub async fn delete(
        &self,
        key: &str,
        revision: Option<crate::client::ResourceRevision>,
    ) -> Result<(), KvResourceWriteError<T>> {
        self.ensure_current()
            .map_err(|_| KvResourceWriteError::Unavailable)?;
        let result = match revision {
            Some(revision) => {
                self.client
                    .delete_revision(key, revision.into_backend())
                    .await
            }
            None => self.client.delete(key).await,
        };
        match result {
            Ok(()) => Ok(()),
            Err(ServerError::KvRevisionMismatch { .. }) => Err(KvResourceWriteError::Conflict {
                current: self.current_for_conflict(key).await?,
            }),
            Err(error) => Err(KvResourceWriteError::Backend(error.to_string())),
        }
    }

    /// Return retained revisions in backend order, including tombstones.
    pub async fn history(&self, key: &str) -> Result<Vec<KvResourceEntry<T>>, KvResourceReadError> {
        self.ensure_current()?;
        let mut projected = Vec::new();
        for entry in self.client.history(key).await.map_err(backend_read)? {
            projected.push(self.project(entry).await?);
        }
        Ok(projected)
    }

    /// Watch future puts and tombstones; each revision reports its own decode/migration result.
    pub async fn watch(
        &self,
        key: &str,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<KvResourceEntry<T>, KvResourceReadError>> + Send>>,
        KvResourceReadError,
    > {
        self.ensure_current()?;
        let stream = self.client.watch(key).await.map_err(backend_read)?;
        let handle = self.clone();
        Ok(Box::pin(stream.then(move |entry| {
            let handle = handle.clone();
            async move {
                handle.ensure_current()?;
                handle
                    .project(entry.map_err(backend_read)?)
                    .await
                    .map_err(Into::into)
            }
        })))
    }

    async fn write(
        &self,
        key: &str,
        value: &T,
        mode: Option<Option<crate::client::ResourceRevision>>,
    ) -> Result<KvResourceEntry<T>, KvResourceWriteError<T>> {
        self.ensure_current()
            .map_err(|_| KvResourceWriteError::Unavailable)?;
        let value = self.codec.encode(value)?;
        let result = match mode {
            None => self.client.create(key, value).await,
            Some(None) => self.client.put(key, value).await,
            Some(Some(revision)) => {
                self.client
                    .replace(key, value, revision.into_backend())
                    .await
            }
        };
        match result {
            Ok(entry) => self
                .project(entry)
                .await
                .map_err(KvResourceWriteError::Codec),
            Err(ServerError::KvRevisionMismatch { .. }) => Err(KvResourceWriteError::Conflict {
                current: self.current_for_conflict(key).await?,
            }),
            Err(error) => Err(KvResourceWriteError::Backend(error.to_string())),
        }
    }

    async fn current_for_conflict(
        &self,
        key: &str,
    ) -> Result<Option<KvResourceEntry<T>>, KvResourceWriteError<T>> {
        match self.client.get_entry(key).await {
            Ok(Some(entry)) => self
                .project(entry)
                .await
                .map(Some)
                .map_err(KvResourceWriteError::Codec),
            Ok(None) => Ok(None),
            Err(error) => Err(KvResourceWriteError::Backend(error.to_string())),
        }
    }

    async fn project(
        &self,
        entry: RawKvResourceEntry,
    ) -> Result<KvResourceEntry<T>, crate::client::ResourceCodecError> {
        let value = match entry.value {
            Some(value) => Some(self.codec.decode(&value).await?),
            None => None,
        };
        Ok(KvResourceEntry {
            key: entry.key,
            value,
            revision: crate::client::ResourceRevision::from_backend(entry.revision),
            timestamp: entry.timestamp,
            operation: entry.operation,
        })
    }

    fn ensure_current(&self) -> Result<(), KvResourceReadError> {
        if self
            .availability
            .borrow()
            .resource_generation(crate::generated::ResourceKind::Kv, &self.resource_name)
            == Some(self.generation)
            && self
                .availability
                .borrow()
                .has_kv_binding(&self.resource_name, &self.binding)
        {
            Ok(())
        } else {
            Err(KvResourceReadError::Unavailable)
        }
    }
}

fn backend_read(error: ServerError) -> KvResourceReadError {
    KvResourceReadError::Backend(error.to_string())
}

/// Contract-safe metadata for one object-store object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreObjectInfo {
    /// Logical object key.
    pub key: String,
    /// Object size in bytes.
    pub size: u64,
    /// Backend-verified digest when available.
    pub digest: Option<String>,
    /// Last modification time when available.
    pub modified_at: Option<OffsetDateTime>,
}

/// Bounded object metadata query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreListOptions {
    /// Optional key prefix. Empty matches every key.
    pub prefix: String,
    /// Opaque continuation returned by the preceding page.
    pub cursor: Option<String>,
    /// Maximum returned entries. Defaults to 100 and must be at most 500.
    pub limit: Option<usize>,
}

/// One bounded page of object metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreListPage {
    /// Metadata entries sorted by key.
    pub entries: Vec<StoreObjectInfo>,
    /// Opaque continuation when another page exists.
    pub next_cursor: Option<String>,
}

/// Operations required by a high-level bound object-store resource handle.
pub trait StoreResourceClient: Clone + fmt::Debug + Send + Sync + 'static {
    /// Stream `key` into `writer`, or return `None` when the object is absent.
    ///
    /// This method does not flush or shut down the caller-owned writer.
    fn read_into<W>(
        &self,
        key: &str,
        writer: &mut W,
    ) -> impl Future<Output = Result<Option<StoreObjectInfo>, ServerError>> + Send
    where
        W: AsyncWrite + Unpin + Send;

    /// Stream an object from `reader` into the store.
    fn write_from<R>(
        &self,
        key: &str,
        reader: &mut R,
    ) -> impl Future<Output = Result<StoreObjectInfo, ServerError>> + Send
    where
        R: AsyncRead + Unpin + Send;

    /// Read all bytes for `key`, or `None` when the object is absent.
    fn read(&self, key: &str) -> impl Future<Output = Result<Option<Bytes>, ServerError>> + Send {
        async move {
            let mut writer = Cursor::new(Vec::new());
            Ok(self
                .read_into(key, &mut writer)
                .await?
                .map(|_| Bytes::from(writer.into_inner())))
        }
    }

    /// Persist a complete in-memory object at `key`.
    fn write(
        &self,
        key: &str,
        value: Bytes,
    ) -> impl Future<Output = Result<(), ServerError>> + Send {
        async move {
            let mut reader = Cursor::new(value);
            self.write_from(key, &mut reader).await?;
            Ok(())
        }
    }

    /// List active object names in this store.
    fn list(&self) -> impl Future<Output = Result<Vec<String>, ServerError>> + Send;

    /// Return object metadata without retaining its payload.
    fn metadata(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<StoreObjectInfo>, ServerError>> + Send {
        async move {
            // ponytail: generic adapters drain the object; native backends may override this.
            self.read_into(key, &mut tokio::io::sink()).await
        }
    }

    /// List active object metadata.
    fn list_objects(
        &self,
    ) -> impl Future<Output = Result<Vec<StoreObjectInfo>, ServerError>> + Send {
        async move {
            // ponytail: generic adapters use one metadata read per key; native backends override it.
            let mut objects = Vec::new();
            for key in self.list().await? {
                if let Some(info) = self.metadata(&key).await? {
                    objects.push(info);
                }
            }
            Ok(objects)
        }
    }

    /// Delete `key` from this store.
    fn delete(&self, key: &str) -> impl Future<Output = Result<(), ServerError>> + Send;
}

/// High-level handle for one service-owned object-store resource alias.
#[derive(Debug, Clone)]
pub struct StoreResourceHandle<C> {
    service_name: String,
    resource_name: String,
    binding: StoreResourceBinding,
    client: C,
    availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
    generation: u64,
}

/// Options for waiting until an object appears in a bound object store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreWaitOptions {
    /// Maximum time to wait before returning [`ServerError::StoreWaitTimeout`].
    pub timeout: Option<Duration>,
    /// Delay between object existence checks. Defaults to 250ms.
    pub poll_interval: Duration,
}

impl Default for StoreWaitOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            poll_interval: Duration::from_millis(250),
        }
    }
}

impl<C> StoreResourceHandle<C>
where
    C: StoreResourceClient,
{
    /// Create a store resource handle from a validated binding and opened client.
    pub fn new(
        service_name: impl Into<String>,
        resource_name: impl Into<String>,
        binding: StoreResourceBinding,
        client: C,
        availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
    ) -> Self {
        let resource_name = resource_name.into();
        let generation = availability
            .borrow()
            .resource_generation(crate::generated::ResourceKind::Store, &resource_name)
            .unwrap_or(0);
        Self {
            service_name: service_name.into(),
            resource_name,
            binding,
            client,
            availability,
            generation,
        }
    }

    /// Contract-local resource alias used to open this handle.
    pub fn resource_name(&self) -> &str {
        &self.resource_name
    }

    /// Concrete resource binding resolved during bootstrap.
    pub fn binding(&self) -> &StoreResourceBinding {
        &self.binding
    }

    /// Stream an object from `reader` while enforcing the bound maximum and exact expected size.
    pub async fn write_from<R>(
        &self,
        key: &str,
        reader: R,
        expected_size: Option<u64>,
    ) -> Result<StoreObjectInfo, ServerError>
    where
        R: AsyncRead + Unpin + Send,
    {
        self.write_from_with_cancel(key, reader, expected_size, pending())
            .await
    }

    /// Stream an object from `reader`, aborting before validated EOF when `cancel` resolves.
    ///
    /// Once validated source EOF has been observed, backend metadata commit is awaited and
    /// cancellation can no longer promise rollback.
    pub async fn write_from_with_cancel<R, F>(
        &self,
        key: &str,
        reader: R,
        expected_size: Option<u64>,
        cancel: F,
    ) -> Result<StoreObjectInfo, ServerError>
    where
        R: AsyncRead + Unpin + Send,
        F: Future<Output = ()> + Send,
    {
        self.ensure_current()?;
        let max_size = self
            .binding
            .max_object_bytes
            .and_then(|value| value.try_into().ok());
        if let (Some(expected_bytes), Some(max_bytes)) = (expected_size, max_size) {
            if expected_bytes > max_bytes {
                return Err(ServerError::StoreObjectTooLarge {
                    attempted_bytes: expected_bytes,
                    max_bytes,
                });
            }
        }

        let validated_eof = Arc::new(AtomicBool::new(false));
        let mut guarded =
            GuardedUploadReader::new(reader, expected_size, max_size, Arc::clone(&validated_eof));
        let result = {
            let write = self.client.write_from(key, &mut guarded);
            tokio::pin!(write);
            tokio::pin!(cancel);
            tokio::select! {
                biased;
                result = &mut write => Some(result),
                () = &mut cancel => {
                    if validated_eof.load(Ordering::Acquire) {
                        Some(write.await)
                    } else {
                        None
                    }
                }
            }
        };
        if result.is_none() {
            guarded.failure = Some(UploadReadFailure::Cancelled);
        }
        let result = result.unwrap_or(Err(ServerError::StoreWriteCancelled));
        match guarded.failure {
            Some(UploadReadFailure::TooLarge {
                attempted_bytes,
                max_bytes,
            }) => Err(ServerError::StoreObjectTooLarge {
                attempted_bytes,
                max_bytes,
            }),
            Some(UploadReadFailure::SizeMismatch {
                expected_bytes,
                actual_bytes,
            }) => Err(ServerError::StoreObjectSizeMismatch {
                expected_bytes,
                actual_bytes,
            }),
            Some(UploadReadFailure::Cancelled) => Err(ServerError::StoreWriteCancelled),
            None => result,
        }
    }

    /// Stream an object into `writer`; the writer is neither flushed nor shut down.
    pub async fn read_into<W>(
        &self,
        key: &str,
        writer: W,
    ) -> Result<Option<StoreObjectInfo>, ServerError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        self.read_into_with_cancel(key, writer, pending()).await
    }

    /// Stream an object into `writer`, returning cancellation after any already-written prefix.
    pub async fn read_into_with_cancel<W, F>(
        &self,
        key: &str,
        mut writer: W,
        cancel: F,
    ) -> Result<Option<StoreObjectInfo>, ServerError>
    where
        W: AsyncWrite + Unpin + Send,
        F: Future<Output = ()> + Send,
    {
        self.ensure_current()?;
        tokio::pin!(cancel);
        tokio::select! {
            biased;
            () = &mut cancel => Err(ServerError::StoreReadCancelled),
            result = self.client.read_into(key, &mut writer) => result,
        }
    }

    /// Read all bytes for `key`, or `None` when the object is absent.
    pub async fn read(&self, key: &str) -> Result<Option<Bytes>, ServerError> {
        let mut writer = Cursor::new(Vec::new());
        Ok(self
            .read_into(key, &mut writer)
            .await?
            .map(|_| Bytes::from(writer.into_inner())))
    }

    /// Wait until `key` appears in this store, then return its bytes.
    ///
    /// The handle checks immediately, then polls according to `options`. When
    /// `options.timeout` elapses before the object appears, this returns
    /// [`ServerError::StoreWaitTimeout`].
    pub async fn wait_for(
        &self,
        key: &str,
        options: StoreWaitOptions,
    ) -> Result<Bytes, ServerError> {
        self.wait_for_with_cancel(key, options, std::future::pending::<()>())
            .await
    }

    /// Wait until `key` appears in this store, or until `cancel` resolves.
    ///
    /// This has the same timeout behavior as [`StoreResourceHandle::wait_for`].
    /// If `cancel` resolves first, this returns
    /// [`ServerError::StoreWaitCanceled`].
    pub async fn wait_for_with_cancel<F>(
        &self,
        key: &str,
        options: StoreWaitOptions,
        cancel: F,
    ) -> Result<Bytes, ServerError>
    where
        F: Future<Output = ()> + Send,
    {
        pin_mut!(cancel);
        let started = tokio::time::Instant::now();
        let deadline = options.timeout.map(|timeout| started + timeout);
        loop {
            let read = self.read(key);
            if let (Some(deadline), Some(timeout_duration)) = (deadline, options.timeout) {
                let timeout = tokio::time::sleep_until(deadline);
                tokio::pin!(timeout);
                tokio::select! {
                    biased;
                    () = &mut cancel => {
                        return Err(self.store_wait_canceled_error(key));
                    }
                    result = read => {
                        if let Some(bytes) = result? {
                            return Ok(bytes);
                        }
                    }
                    () = &mut timeout => {
                        return Err(self.store_wait_timeout_error(key, timeout_duration));
                    }
                }
            } else {
                tokio::select! {
                    biased;
                    () = &mut cancel => {
                        return Err(self.store_wait_canceled_error(key));
                    }
                    result = read => {
                        if let Some(bytes) = result? {
                            return Ok(bytes);
                        }
                    }
                }
            }

            let poll_interval = options.poll_interval.max(Duration::from_millis(1));
            let delay = if let (Some(deadline), Some(timeout)) = (deadline, options.timeout) {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Err(self.store_wait_timeout_error(key, timeout));
                }
                poll_interval.min(deadline - now)
            } else {
                poll_interval
            };

            tokio::select! {
                biased;
                () = &mut cancel => {
                    return Err(self.store_wait_canceled_error(key));
                }
                () = tokio::time::sleep(delay) => {}
            }
        }
    }

    fn store_wait_timeout_error(&self, key: &str, timeout: Duration) -> ServerError {
        ServerError::StoreWaitTimeout {
            service_name: self.service_name.clone(),
            store: self.resource_name.clone(),
            key: key.to_string(),
            timeout_ms: timeout.as_millis(),
        }
    }

    fn store_wait_canceled_error(&self, key: &str) -> ServerError {
        ServerError::StoreWaitCanceled {
            service_name: self.service_name.clone(),
            store: self.resource_name.clone(),
            key: key.to_string(),
        }
    }

    /// Persist `value` at `key`.
    pub async fn write(&self, key: &str, value: impl Into<Bytes>) -> Result<(), ServerError> {
        let value = value.into();
        let expected_size = u64::try_from(value.len()).map_err(|_| {
            ServerError::Nats("store object length does not fit in u64".to_string())
        })?;
        let mut reader = Cursor::new(value);
        self.write_from(key, &mut reader, Some(expected_size))
            .await?;
        Ok(())
    }

    /// List active object names in this store.
    pub async fn list(&self) -> Result<Vec<String>, ServerError> {
        self.ensure_current()?;
        self.client.list().await
    }

    /// Return metadata for an active object without retaining its payload.
    pub async fn metadata(&self, key: &str) -> Result<Option<StoreObjectInfo>, ServerError> {
        self.ensure_current()?;
        self.client.metadata(key).await
    }

    /// List a bounded, key-sorted page of object metadata.
    pub async fn list_page(&self, options: StoreListOptions) -> Result<StoreListPage, ServerError> {
        self.ensure_current()?;
        let limit = options.limit.unwrap_or(100);
        if limit == 0 || limit > 500 {
            return Err(ServerError::Nats(
                "store list limit must be between 1 and 500".into(),
            ));
        }
        let query_digest = trellis_protocol::pagination_query_digest(
            "trellis.store.list",
            &serde_json::json!({ "prefix": options.prefix }),
        )
        .map_err(|error| ServerError::Nats(error.to_string()))?;
        let after = options
            .cursor
            .as_deref()
            .map(|cursor| {
                trellis_protocol::decode_pagination_cursor::<String>(cursor, &query_digest)
            })
            .transpose()
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        let mut objects = self.client.list_objects().await?;
        objects.retain(|object| {
            object.key.starts_with(&options.prefix)
                && after.as_ref().is_none_or(|after| object.key > *after)
        });
        objects.sort_by(|left, right| left.key.cmp(&right.key));
        let has_more = objects.len() > limit;
        let entries: Vec<_> = objects.into_iter().take(limit).collect();
        let next_cursor = has_more
            .then(|| {
                trellis_protocol::encode_pagination_cursor(
                    &query_digest,
                    &entries.last().expect("nonempty page with continuation").key,
                )
            })
            .transpose()
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        Ok(StoreListPage {
            entries,
            next_cursor,
        })
    }

    /// Delete `key` from this store.
    pub async fn delete(&self, key: &str) -> Result<(), ServerError> {
        self.ensure_current()?;
        self.client.delete(key).await
    }

    fn ensure_current(&self) -> Result<(), ServerError> {
        if self
            .availability
            .borrow()
            .resource_generation(crate::generated::ResourceKind::Store, &self.resource_name)
            == Some(self.generation)
            && self
                .availability
                .borrow()
                .has_store_binding(&self.resource_name, &self.binding)
        {
            Ok(())
        } else {
            Err(ServerError::ResourceUnavailable {
                resource_kind: "store".to_owned(),
                resource_name: self.resource_name.clone(),
            })
        }
    }
}

impl<C> StoreResourceClient for StoreResourceHandle<C>
where
    C: StoreResourceClient,
{
    async fn read_into<W>(
        &self,
        key: &str,
        writer: &mut W,
    ) -> Result<Option<StoreObjectInfo>, ServerError>
    where
        W: AsyncWrite + Unpin + Send,
    {
        StoreResourceHandle::read_into(self, key, writer).await
    }

    async fn write_from<R>(&self, key: &str, reader: &mut R) -> Result<StoreObjectInfo, ServerError>
    where
        R: AsyncRead + Unpin + Send,
    {
        StoreResourceHandle::write_from(self, key, reader, None).await
    }

    async fn list(&self) -> Result<Vec<String>, ServerError> {
        self.client.list().await
    }

    async fn metadata(&self, key: &str) -> Result<Option<StoreObjectInfo>, ServerError> {
        self.client.metadata(key).await
    }

    async fn list_objects(&self) -> Result<Vec<StoreObjectInfo>, ServerError> {
        self.client.list_objects().await
    }

    async fn delete(&self, key: &str) -> Result<(), ServerError> {
        self.client.delete(key).await
    }
}

/// Connected typed handle for one contract-declared KV resource.
pub type KvHandle<T> = KvResourceHandle<T, BoundKvResourceClient>;

/// Connected handle for one contract-declared object-store resource.
pub type StoreHandle = StoreResourceHandle<BoundStoreResourceClient>;

pub(crate) async fn open_generated_kv<T>(
    client: &async_nats::Client,
    participant_id: &str,
    name: &str,
    binding: KvResourceBinding,
    codec: crate::client::ResourceCodec<T>,
    availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
) -> Result<KvHandle<T>, ServerError>
where
    T: crate::generated::Codec + Send + 'static,
{
    validate_kv_binding(participant_id, name, &binding)?;
    let backend = client.open_kv(&binding).await?;
    Ok(KvResourceHandle::from_generated(
        name,
        binding,
        codec,
        backend,
        availability,
    ))
}

pub(crate) async fn open_generated_store(
    client: &async_nats::Client,
    participant_id: &str,
    name: &str,
    binding: StoreResourceBinding,
    availability: tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot>,
) -> Result<StoreHandle, ServerError> {
    validate_store_binding(participant_id, name, &binding)?;
    let backend = client.open_store(&binding).await?;
    Ok(StoreResourceHandle::new(
        participant_id,
        name,
        binding,
        backend,
        availability,
    ))
}

impl ResourceRuntimeClient for async_nats::Client {
    type Kv = BoundKvResourceClient;
    type Store = BoundStoreResourceClient;

    async fn open_kv(&self, binding: &KvResourceBinding) -> Result<Self::Kv, ServerError> {
        let context = async_nats::jetstream::new(self.clone());
        let store = context
            .get_key_value(binding.bucket.clone())
            .await
            .map_err(nats_error)?;
        ensure_existing_kv_binding(&store, binding).await?;
        Ok(BoundKvResourceClient { store })
    }

    async fn open_store(&self, binding: &StoreResourceBinding) -> Result<Self::Store, ServerError> {
        let context = async_nats::jetstream::new(self.clone());
        let store = context
            .get_object_store(&binding.name)
            .await
            .map_err(nats_error)?;
        ensure_existing_store_binding(&context, binding).await?;
        Ok(BoundStoreResourceClient { store })
    }
}

async fn ensure_existing_store_binding(
    context: &async_nats::jetstream::Context,
    binding: &StoreResourceBinding,
) -> Result<(), ServerError> {
    let mut stream = context
        .get_stream(format!("OBJ_{}", binding.name))
        .await
        .map_err(nats_error)?;
    let info = stream.info().await.map_err(nats_error)?;
    let config = &info.config;
    if config.name != format!("OBJ_{}", binding.name) {
        return Err(nats_error(format!(
            "Store binding '{}' opened the wrong physical bucket",
            binding.name
        )));
    }
    if config.max_age != Duration::from_millis(binding.ttl_ms as u64) {
        return Err(nats_error(format!(
            "Store bucket '{}' TTL does not match the binding",
            binding.name
        )));
    }
    let max_total_bytes = (config.max_bytes > 0).then_some(config.max_bytes);
    if max_total_bytes != binding.max_total_bytes {
        return Err(nats_error(format!(
            "Store bucket '{}' total limit does not match the binding",
            binding.name
        )));
    }
    Ok(())
}

async fn ensure_existing_kv_binding(
    store: &async_nats::jetstream::kv::Store,
    binding: &KvResourceBinding,
) -> Result<(), ServerError> {
    let mut stream = store.stream.clone();
    let info = stream.info().await.map_err(nats_error)?;
    let config = &info.config;
    if store.name != binding.bucket || config.name != format!("KV_{}", binding.bucket) {
        return Err(nats_error(format!(
            "KV binding '{}' opened the wrong physical bucket",
            binding.bucket
        )));
    }
    if config.max_messages_per_subject < binding.history {
        return Err(nats_error(format!(
            "KV bucket '{}' history is less than the binding",
            binding.bucket
        )));
    }
    if config.max_age != Duration::from_millis(binding.ttl_ms as u64) {
        return Err(nats_error(format!(
            "KV bucket '{}' TTL does not match the binding",
            binding.bucket
        )));
    }
    let incompatible_limit = config.max_bytes > 0
        || match binding.max_value_bytes {
            Some(required) => {
                config.max_message_size > 0 && i64::from(config.max_message_size) < required
            }
            None => config.max_message_size > 0,
        };
    if incompatible_limit {
        return Err(nats_error(format!(
            "KV bucket '{}' provider size limit does not satisfy the binding",
            binding.bucket
        )));
    }
    Ok(())
}

pub fn validate_kv_binding(
    service_name: &str,
    resource_name: &str,
    binding: &KvResourceBinding,
) -> Result<(), ServerError> {
    if binding.bucket.is_empty() {
        return Err(invalid_binding(
            service_name,
            "kv",
            resource_name,
            "bucket name is empty",
        ));
    }
    if !is_valid_nats_resource_name(&binding.bucket) {
        return Err(invalid_binding(
            service_name,
            "kv",
            resource_name,
            "bucket name must contain only ASCII letters, digits, underscores, and hyphens",
        ));
    }
    if binding.history < 1 {
        return Err(invalid_binding(
            service_name,
            "kv",
            resource_name,
            "history must be greater than zero",
        ));
    }
    if matches!(binding.max_value_bytes, Some(max_bytes) if max_bytes < 0) {
        return Err(invalid_binding(
            service_name,
            "kv",
            resource_name,
            "max_value_bytes must not be negative",
        ));
    }
    if binding.ttl_ms < 0 {
        return Err(invalid_binding(
            service_name,
            "kv",
            resource_name,
            "ttl_ms must not be negative",
        ));
    }
    Ok(())
}

pub fn validate_store_binding(
    service_name: &str,
    resource_name: &str,
    binding: &StoreResourceBinding,
) -> Result<(), ServerError> {
    if binding.name.is_empty() {
        return Err(invalid_binding(
            service_name,
            "store",
            resource_name,
            "store name is empty",
        ));
    }
    if !is_valid_nats_resource_name(&binding.name) {
        return Err(invalid_binding(
            service_name,
            "store",
            resource_name,
            "store name must contain only ASCII letters, digits, underscores, and hyphens",
        ));
    }
    if matches!(binding.max_object_bytes, Some(max_bytes) if max_bytes < 0) {
        return Err(invalid_binding(
            service_name,
            "store",
            resource_name,
            "max_object_bytes must not be negative",
        ));
    }
    if matches!(binding.max_total_bytes, Some(max_bytes) if max_bytes < 0) {
        return Err(invalid_binding(
            service_name,
            "store",
            resource_name,
            "max_total_bytes must not be negative",
        ));
    }
    if binding.ttl_ms < 0 {
        return Err(invalid_binding(
            service_name,
            "store",
            resource_name,
            "ttl_ms must not be negative",
        ));
    }
    Ok(())
}

fn invalid_binding(
    service_name: &str,
    resource_kind: &str,
    resource_name: &str,
    reason: &str,
) -> ServerError {
    ServerError::InvalidResourceBinding {
        service_name: service_name.to_string(),
        resource_kind: resource_kind.to_string(),
        resource_name: resource_name.to_string(),
        reason: reason.to_string(),
    }
}

fn is_valid_nats_resource_name(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn nats_error(error: impl fmt::Display) -> ServerError {
    ServerError::Nats(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::{oneshot, Notify},
    };

    use super::*;

    #[derive(Debug, Clone, Default)]
    struct RecordingStore {
        writes: Arc<AtomicUsize>,
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    #[derive(Debug, Clone, Default)]
    struct StalledStore {
        started: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    #[derive(Debug, Clone, Default)]
    struct CommitStalledStore {
        committing: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl StoreResourceClient for CommitStalledStore {
        async fn read_into<W>(
            &self,
            _key: &str,
            _writer: &mut W,
        ) -> Result<Option<StoreObjectInfo>, ServerError>
        where
            W: AsyncWrite + Unpin + Send,
        {
            unreachable!()
        }

        async fn write_from<R>(
            &self,
            key: &str,
            reader: &mut R,
        ) -> Result<StoreObjectInfo, ServerError>
        where
            R: AsyncRead + Unpin + Send,
        {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.map_err(nats_error)?;
            self.committing.notify_one();
            self.release.notified().await;
            Ok(StoreObjectInfo {
                key: key.to_string(),
                size: bytes.len() as u64,
                digest: None,
                modified_at: None,
            })
        }

        async fn list(&self) -> Result<Vec<String>, ServerError> {
            unreachable!()
        }

        async fn delete(&self, _key: &str) -> Result<(), ServerError> {
            unreachable!()
        }
    }

    impl StoreResourceClient for StalledStore {
        async fn read_into<W>(
            &self,
            _key: &str,
            _writer: &mut W,
        ) -> Result<Option<StoreObjectInfo>, ServerError>
        where
            W: AsyncWrite + Unpin + Send,
        {
            unreachable!()
        }

        async fn write_from<R>(
            &self,
            _key: &str,
            reader: &mut R,
        ) -> Result<StoreObjectInfo, ServerError>
        where
            R: AsyncRead + Unpin + Send,
        {
            struct DropFlag(Arc<AtomicBool>);
            impl Drop for DropFlag {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }

            let mut byte = [0_u8; 1];
            reader.read_exact(&mut byte).await.map_err(nats_error)?;
            let _drop_flag = DropFlag(Arc::clone(&self.dropped));
            self.started.notify_one();
            pending().await
        }

        async fn list(&self) -> Result<Vec<String>, ServerError> {
            unreachable!()
        }

        async fn delete(&self, _key: &str) -> Result<(), ServerError> {
            unreachable!()
        }
    }

    impl StoreResourceClient for RecordingStore {
        async fn read_into<W>(
            &self,
            key: &str,
            writer: &mut W,
        ) -> Result<Option<StoreObjectInfo>, ServerError>
        where
            W: AsyncWrite + Unpin + Send,
        {
            let bytes = self.bytes.lock().expect("recording store lock").clone();
            writer.write_all(&bytes).await.map_err(nats_error)?;
            Ok(Some(StoreObjectInfo {
                key: key.to_string(),
                size: bytes.len() as u64,
                digest: None,
                modified_at: None,
            }))
        }

        async fn write_from<R>(
            &self,
            key: &str,
            reader: &mut R,
        ) -> Result<StoreObjectInfo, ServerError>
        where
            R: AsyncRead + Unpin + Send,
        {
            self.writes.fetch_add(1, Ordering::SeqCst);
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.map_err(nats_error)?;
            let size = bytes.len() as u64;
            *self.bytes.lock().expect("recording store lock") = bytes;
            Ok(StoreObjectInfo {
                key: key.to_string(),
                size,
                digest: None,
                modified_at: None,
            })
        }

        async fn list(&self) -> Result<Vec<String>, ServerError> {
            Ok(Vec::new())
        }

        async fn list_objects(&self) -> Result<Vec<StoreObjectInfo>, ServerError> {
            Ok(["zeta", "prefix/two", "prefix/one"]
                .into_iter()
                .map(|key| StoreObjectInfo {
                    key: key.to_string(),
                    size: key.len() as u64,
                    digest: None,
                    modified_at: None,
                })
                .collect())
        }

        async fn delete(&self, _key: &str) -> Result<(), ServerError> {
            Ok(())
        }
    }

    fn handle(max_object_bytes: Option<i64>) -> StoreResourceHandle<RecordingStore> {
        let binding = StoreResourceBinding {
            name: "test_store".to_string(),
            max_object_bytes,
            max_total_bytes: None,
            ttl_ms: 0,
        };
        StoreResourceHandle::new(
            "test-service",
            "objects",
            binding.clone(),
            RecordingStore::default(),
            store_availability("objects", binding),
        )
    }

    #[tokio::test]
    async fn store_list_page_filters_sorts_and_bounds_metadata() {
        let first = handle(None)
            .list_page(StoreListOptions {
                prefix: "prefix/".into(),
                cursor: None,
                limit: Some(1),
            })
            .await
            .unwrap();
        let page = handle(None)
            .list_page(StoreListOptions {
                prefix: "prefix/".into(),
                cursor: first.next_cursor,
                limit: Some(1),
            })
            .await
            .unwrap();

        assert_eq!(page.entries[0].key, "prefix/two");
        assert_eq!(page.next_cursor, None);
    }

    fn store_availability(
        resource_name: &str,
        binding: StoreResourceBinding,
    ) -> tokio::sync::watch::Receiver<crate::generated::AvailabilitySnapshot> {
        let mut resources = crate::service::ServiceResourceBindings::default();
        resources.store.insert(resource_name.to_owned(), binding);
        tokio::sync::watch::channel(crate::generated::AvailabilitySnapshot::new(
            Vec::new(),
            resources,
        ))
        .1
    }

    #[tokio::test]
    async fn store_write_from_rejects_known_oversize_before_backend_io() {
        let handle = handle(Some(4));
        let writes = Arc::clone(&handle.client.writes);
        let mut reader = Cursor::new(b"12345".to_vec());

        let error = handle
            .write_from("key", &mut reader, Some(5))
            .await
            .expect_err("known oversize must fail");

        assert!(matches!(
            error,
            ServerError::StoreObjectTooLarge {
                attempted_bytes: 5,
                max_bytes: 4
            }
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert_eq!(reader.position(), 0);
    }

    #[tokio::test]
    async fn store_write_from_rejects_unknown_oversize_without_clean_eof() {
        let handle = handle(Some(4));
        let error = handle
            .write_from("key", Cursor::new(b"12345".to_vec()), None)
            .await
            .expect_err("unknown oversize must fail");

        assert!(matches!(
            error,
            ServerError::StoreObjectTooLarge {
                attempted_bytes: 5,
                max_bytes: 4
            }
        ));
        assert!(handle
            .client
            .bytes
            .lock()
            .expect("recording store lock")
            .is_empty());
    }

    #[tokio::test]
    async fn store_write_from_requires_exact_expected_size() {
        for (bytes, actual) in [(b"123".as_slice(), 3), (b"12345".as_slice(), 5)] {
            let handle = handle(None);
            let error = handle
                .write_from("key", Cursor::new(bytes.to_vec()), Some(4))
                .await
                .expect_err("size mismatch must fail");
            assert!(matches!(
                error,
                ServerError::StoreObjectSizeMismatch {
                    expected_bytes: 4,
                    actual_bytes
                } if actual_bytes == actual
            ));
        }
    }

    #[tokio::test]
    async fn store_trait_forwarding_keeps_bound_maximum() {
        let handle = handle(Some(4));
        let mut reader = Cursor::new(b"12345".to_vec());
        let error = StoreResourceClient::write_from(&handle, "key", &mut reader)
            .await
            .expect_err("trait forwarding must enforce binding");
        assert!(matches!(error, ServerError::StoreObjectTooLarge { .. }));
    }

    #[tokio::test]
    async fn store_streaming_and_whole_buffer_paths_round_trip() {
        let handle = handle(Some(1024));
        let info = handle
            .write_from("key", Cursor::new(b"streamed".to_vec()), Some(8))
            .await
            .expect("stream upload");
        assert_eq!(info.size, 8);

        let mut writer = Cursor::new(Vec::new());
        let read_info = handle
            .read_into("key", &mut writer)
            .await
            .expect("stream download")
            .expect("object exists");
        assert_eq!(read_info.size, 8);
        assert_eq!(writer.into_inner(), b"streamed");

        handle
            .write("key", Bytes::from_static(b"buffered"))
            .await
            .unwrap();
        assert_eq!(
            handle.read("key").await.unwrap().unwrap(),
            b"buffered".as_slice()
        );
    }

    #[tokio::test]
    async fn store_write_from_cancellation_aborts_before_eof() {
        let handle = handle(None);
        let (mut writer, reader) = tokio::io::duplex(8);
        writer.write_all(b"prefix").await.unwrap();
        let error = handle
            .write_from_with_cancel("key", reader, None, async {})
            .await
            .expect_err("cancellation must abort upload");
        assert!(matches!(error, ServerError::StoreWriteCancelled));
    }

    #[tokio::test]
    async fn store_write_from_cancellation_drops_stalled_backend_before_eof() {
        let store = StalledStore::default();
        let started = Arc::clone(&store.started);
        let dropped = Arc::clone(&store.dropped);
        let binding = StoreResourceBinding {
            name: "test_store".to_string(),
            ttl_ms: 0,
            max_object_bytes: None,
            max_total_bytes: None,
        };
        let handle = StoreResourceHandle::new(
            "test-service",
            "uploads",
            binding.clone(),
            store,
            store_availability("uploads", binding),
        );
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            handle
                .write_from_with_cancel("key", Cursor::new(b"payload"), None, async {
                    let _ = cancel_rx.await;
                })
                .await
        });

        started.notified().await;
        cancel_tx.send(()).expect("send cancellation");
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancellation must not wait for stalled backend")
            .expect("upload task must join");

        assert!(matches!(result, Err(ServerError::StoreWriteCancelled)));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn store_write_from_ignores_cancellation_after_validated_eof() {
        let store = CommitStalledStore::default();
        let committing = Arc::clone(&store.committing);
        let release = Arc::clone(&store.release);
        let binding = StoreResourceBinding {
            name: "test_store".to_string(),
            ttl_ms: 0,
            max_object_bytes: None,
            max_total_bytes: None,
        };
        let handle = StoreResourceHandle::new(
            "test-service",
            "uploads",
            binding.clone(),
            store,
            store_availability("uploads", binding),
        );
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let mut task = tokio::spawn(async move {
            handle
                .write_from_with_cancel("key", Cursor::new(b"payload"), None, async {
                    let _ = cancel_rx.await;
                })
                .await
        });

        committing.notified().await;
        cancel_tx.send(()).expect("send cancellation");
        tokio::select! {
            result = &mut task => panic!("cancellation rolled back validated EOF: {result:?}"),
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        release.notify_one();
        let info = task
            .await
            .expect("upload task must join")
            .expect("metadata commit must complete");
        assert_eq!(info.size, 7);
    }

    #[cfg(feature = "live-integration")]
    #[tokio::test]
    async fn live_kv_binding_validation_uses_physical_bucket_status() {
        let source = tempfile::tempdir().unwrap();
        trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
            source.path(),
        ))
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut nats = trellis_local_nats::LocalNats::builder()
            .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
            .cache_dir(state.path().join("cache"))
            .source(source.path())
            .temporary_state()
            .ephemeral_ports()
            .output(trellis_local_nats::NatsOutput::Log {
                path: state.path().join("nats.log"),
                mirror: false,
            })
            .start()
            .unwrap();
        let client = async_nats::ConnectOptions::new()
            .credentials_file(source.path().join("creds/trellis-auth.creds"))
            .await
            .unwrap()
            .connect(nats.nats_url())
            .await
            .unwrap();
        let jetstream = async_nats::jetstream::new(client.clone());
        let cases = [
            ("exact", 2, 1_000, 100, 0, true),
            ("over_history", 3, 1_000, 100, 0, true),
            ("short_history", 1, 1_000, 100, 0, false),
            ("short_ttl", 2, 999, 100, 0, false),
            ("long_ttl", 2, 1_001, 100, 0, false),
            ("unlimited_ttl", 2, 0, 100, 0, false),
            ("unlimited_value", 2, 1_000, 0, 0, true),
            ("unlimited_total", 2, 1_000, 100, 0, true),
            ("small_value", 2, 1_000, 99, 0, false),
            ("finite_total", 2, 1_000, 100, 1_000, false),
            ("small_total", 2, 1_000, 100, 99, false),
        ];
        let binding = KvResourceBinding {
            bucket: String::new(),
            history: 2,
            ttl_ms: 1_000,
            max_value_bytes: Some(100),
        };

        for (label, history, ttl_ms, max_value, max_total, compatible) in cases {
            let bucket = format!("trellis_kv_{label}_{}", ulid::Ulid::new());
            let store = jetstream
                .create_key_value(async_nats::jetstream::kv::Config {
                    bucket: bucket.clone(),
                    history,
                    max_age: Duration::from_millis(ttl_ms),
                    max_value_size: max_value,
                    max_bytes: max_total,
                    ..Default::default()
                })
                .await
                .unwrap();
            let result = client
                .open_kv(&KvResourceBinding {
                    bucket,
                    ..binding.clone()
                })
                .await;
            assert_eq!(result.is_ok(), compatible, "{label}: {result:?}");

            if label == "exact" {
                let wrong = ensure_existing_kv_binding(
                    &store,
                    &KvResourceBinding {
                        bucket: "wrong_physical_bucket".to_string(),
                        ..binding.clone()
                    },
                )
                .await;
                assert!(wrong.is_err());
            }
        }
        let forever_bucket = format!("trellis_kv_forever_{}", ulid::Ulid::new());
        jetstream
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: forever_bucket.clone(),
                history: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        client
            .open_kv(&KvResourceBinding {
                bucket: forever_bucket,
                history: 2,
                ttl_ms: 0,
                max_value_bytes: None,
            })
            .await
            .expect("forever and unlimited must match the binding");
        nats.stop().unwrap();
    }
}
