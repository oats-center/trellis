use std::{fmt, pin::Pin, task::Poll};

use async_nats::jetstream::kv::{CreateErrorKind, Operation, UpdateErrorKind};
use async_nats::jetstream::object_store::{GetErrorKind, PutErrorKind};
use bytes::Bytes;
use futures_util::{Stream, StreamExt, TryStreamExt};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{
    nats_error, KvResourceClient, KvResourceOperation, RawKvResourceEntry, ServerError,
    StoreObjectInfo, StoreResourceClient,
};

/// Concrete KV client used by connected service resources.
#[derive(Debug, Clone)]
pub struct BoundKvResourceClient {
    pub(super) store: async_nats::jetstream::kv::Store,
}

/// Watch stream for connected KV resources.
pub struct BoundKvWatch {
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
        Pin::new(&mut self.inner)
            .poll_next(cx)
            .map(|entry| entry.map(|entry| entry.map(kv_entry_from_nats).map_err(nats_error)))
    }
}

impl KvResourceClient for BoundKvResourceClient {
    type Watch = BoundKvWatch;

    async fn get_entry(&self, key: &str) -> Result<Option<RawKvResourceEntry>, ServerError> {
        self.authoritative_entry(key).await
    }

    async fn create(&self, key: &str, value: Bytes) -> Result<RawKvResourceEntry, ServerError> {
        match self.store.create(key, value).await {
            Ok(revision) => self.entry_at(key, revision).await,
            Err(error) if error.kind() == CreateErrorKind::AlreadyExists => {
                Err(self.kv_revision_mismatch(key, 0).await)
            }
            Err(error) => Err(nats_error(error)),
        }
    }

    async fn put(&self, key: &str, value: Bytes) -> Result<RawKvResourceEntry, ServerError> {
        let revision = self.store.put(key, value).await.map_err(nats_error)?;
        self.entry_at(key, revision).await
    }

    async fn replace(
        &self,
        key: &str,
        value: Bytes,
        revision: u64,
    ) -> Result<RawKvResourceEntry, ServerError> {
        match self.store.update(key, value, revision).await {
            Ok(revision) => self.entry_at(key, revision).await,
            Err(error) if error.kind() == UpdateErrorKind::WrongLastRevision => {
                Err(self.kv_revision_mismatch(key, revision).await)
            }
            Err(error) => Err(nats_error(error)),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), ServerError> {
        if self
            .authoritative_entry(key)
            .await?
            .is_none_or(|entry| entry.operation == KvResourceOperation::Delete)
        {
            return Ok(());
        }
        self.store.delete(key).await.map_err(nats_error)
    }

    async fn delete_revision(&self, key: &str, revision: u64) -> Result<(), ServerError> {
        match self.store.delete_expect_revision(key, Some(revision)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == UpdateErrorKind::WrongLastRevision => {
                Err(self.kv_revision_mismatch(key, revision).await)
            }
            Err(error) => Err(nats_error(error)),
        }
    }

    async fn history(&self, key: &str) -> Result<Vec<RawKvResourceEntry>, ServerError> {
        let history = self.store.history(key).await.map_err(nats_error)?;
        history
            .map(|entry| entry.map(kv_entry_from_nats).map_err(nats_error))
            .try_collect()
            .await
    }

    async fn watch(&self, key: &str) -> Result<Self::Watch, ServerError> {
        self.store
            .watch(key)
            .await
            .map(|inner| BoundKvWatch { inner })
            .map_err(nats_error)
    }
}

impl BoundKvResourceClient {
    async fn kv_revision_mismatch(&self, key: &str, expected: u64) -> ServerError {
        let actual = self
            .authoritative_entry(key)
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

    async fn entry_at(&self, key: &str, revision: u64) -> Result<RawKvResourceEntry, ServerError> {
        self.store
            .entry_for_revision(key.to_string(), revision)
            .await
            .map_err(nats_error)?
            .map(kv_entry_from_nats)
            .ok_or_else(|| ServerError::Nats(format!("KV write revision {revision} disappeared")))
    }

    async fn authoritative_entry(
        &self,
        key: &str,
    ) -> Result<Option<RawKvResourceEntry>, ServerError> {
        self.store
            .entry(key)
            .await
            .map(|entry| entry.map(kv_entry_from_nats))
            .map_err(nats_error)
    }
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
    pub(super) store: async_nats::jetstream::object_store::ObjectStore,
}

impl BoundStoreResourceClient {
    #[doc(hidden)]
    pub fn new(store: async_nats::jetstream::object_store::ObjectStore) -> Self {
        Self { store }
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
        let mut object = match self.store.get(key).await {
            Ok(object) => object,
            Err(error) if error.kind() == GetErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(nats_error(error)),
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
        let mut reader = reader;
        match self.store.put(key, &mut reader).await {
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
        let objects = self.store.list().await.map_err(nats_error)?;
        objects
            .map(|object| object.map(|info| info.name).map_err(nats_error))
            .try_collect()
            .await
    }

    async fn list_objects(&self) -> Result<Vec<StoreObjectInfo>, ServerError> {
        let objects = self.store.list().await.map_err(nats_error)?;
        objects
            .map(|object| {
                object
                    .map_err(nats_error)
                    .and_then(|info| store_object_info(&info))
            })
            .try_collect()
            .await
    }

    async fn delete(&self, key: &str) -> Result<(), ServerError> {
        self.store.delete(key).await.map_err(nats_error)
    }
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
