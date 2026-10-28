//! A bounded Object Store chunk publisher. Metadata remains the commit point.

use std::{collections::VecDeque, future::IntoFuture, sync::Arc};

use async_nats::jetstream::{
    context::PublishAckFuture,
    object_store::{InfoErrorKind, ObjectInfo, ObjectOptions},
    stream::Stream,
};
use base64::{engine::general_purpose::URL_SAFE, Engine};
use bytes::BytesMut;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{nats_error, store_object_info, ServerError, StoreGuard, StoreObjectInfo};

// At most eight 128 KiB payloads can be awaiting ACKs, including the current
// read. Do not increase this without concurrent-upload memory measurements.
const CHUNK_BYTES: usize = 128 * 1024;
const INFLIGHT_CHUNKS: usize = 8;

struct PendingUpload {
    runtime: tokio::runtime::Handle,
    guard: Arc<StoreGuard>,
    stream: Stream<()>,
    subject: String,
    acks: VecDeque<<PublishAckFuture as IntoFuture>::IntoFuture>,
    uncommitted: bool,
}

impl Drop for PendingUpload {
    fn drop(&mut self) {
        if !self.uncommitted {
            return;
        }
        let guard = Arc::clone(&self.guard);
        let stream = self.stream.clone();
        let subject = self.subject.clone();
        let acks = std::mem::take(&mut self.acks);
        // Reader failure or cancellation cannot publish metadata. Retain the
        // generation through outstanding ACKs and owned partial-chunk cleanup.
        self.runtime.spawn(async move {
            let _ = futures_util::future::join_all(acks).await;
            if let Err(error) = stream.purge().filter(subject).await {
                tracing::warn!(%error, "uncommitted Object Store chunk cleanup failed");
            }
            drop(guard);
        });
    }
}

pub(super) async fn write<R: AsyncRead + Unpin + Send>(
    guard: StoreGuard,
    key: &str,
    reader: &mut R,
) -> Result<StoreObjectInfo, ServerError> {
    if key.is_empty() {
        return Err(nats_error("Object Store name must not be empty"));
    }
    let previous = match guard.store.info(key).await {
        Ok(info) => Some(info),
        Err(error) if error.kind() == InfoErrorKind::NotFound => None,
        Err(error) => return Err(nats_error(error)),
    };
    let nuid = ulid::Ulid::new().to_string();
    let stream = guard
        .context
        .get_stream_no_info(format!("OBJ_{}", guard.bucket))
        .await
        .map_err(nats_error)?;
    let mut pending = PendingUpload {
        runtime: tokio::runtime::Handle::current(),
        subject: format!("$O.{}.C.{nuid}", guard.bucket),
        guard: Arc::new(guard),
        stream,
        acks: VecDeque::new(),
        uncommitted: true,
    };
    let mut hasher = Sha256::new();
    let mut size = 0usize;
    let mut chunks = 0usize;
    loop {
        if pending.acks.len() == INFLIGHT_CHUNKS {
            // Await in place: cancellation must leave this ACK owned by Drop.
            let result = pending.acks.front_mut().expect("full ACK queue").await;
            pending.acks.pop_front();
            result.map_err(nats_error)?;
        }
        // Fill each chunk across short reads; only EOF publishes a partial one.
        let mut buffer = BytesMut::with_capacity(CHUNK_BYTES);
        while buffer.len() < CHUNK_BYTES {
            if reader.read_buf(&mut buffer).await.map_err(nats_error)? == 0 {
                break;
            }
        }
        let count = buffer.len();
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count)
            .ok_or_else(|| nats_error("Object Store size overflow"))?;
        chunks = chunks
            .checked_add(1)
            .ok_or_else(|| nats_error("Object Store chunk count overflow"))?;
        hasher.update(&buffer);
        let ack = pending
            .guard
            .context
            .publish(pending.subject.clone(), buffer.freeze())
            .await
            .map_err(nats_error)?;
        pending.acks.push_back(ack.into_future());
    }
    while let Some(ack) = pending.acks.front_mut() {
        let result = ack.await;
        pending.acks.pop_front();
        result.map_err(nats_error)?;
    }
    let info = ObjectInfo {
        name: key.to_owned(),
        description: None,
        metadata: Default::default(),
        headers: None,
        options: Some(ObjectOptions {
            link: None,
            max_chunk_size: Some(CHUNK_BYTES),
        }),
        bucket: pending.guard.bucket.clone(),
        nuid,
        size,
        chunks,
        digest: Some(format!("SHA-256={}", URL_SAFE.encode(hasher.finalize()))),
        modified: Some(async_nats::datetime::now()),
        deleted: false,
    };
    let payload = serde_json::to_vec(&info).map_err(nats_error)?;
    let mut headers = async_nats::HeaderMap::new();
    headers.insert(async_nats::header::NATS_ROLLUP, "sub");
    // Once a metadata publish can have escaped, do not purge its chunks even if
    // the ACK is lost. The existing caller reconciliation owns ambiguous commit.
    pending.uncommitted = false;
    let commit = async {
        pending
            .guard
            .context
            .publish_with_headers(
                format!("$O.{}.M.{}", info.bucket, URL_SAFE.encode(key)),
                headers,
                payload.into(),
            )
            .await
            .map_err(nats_error)?
            .await
            .map_err(nats_error)?;
        Ok::<(), ServerError>(())
    }
    .await;
    if let Err(error) = commit {
        return Err(ServerError::StoreCommitIndeterminate {
            key: key.to_owned(),
            message: error.to_string(),
        });
    }
    if let Some(previous) = previous {
        if let Err(error) = pending
            .stream
            .purge()
            .filter(format!("$O.{}.C.{}", info.bucket, previous.nuid))
            .await
        {
            return Err(ServerError::StoreCommittedCleanupFailed {
                key: key.to_owned(),
                message: error.to_string(),
            });
        }
    }
    store_object_info(&info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::resources::{BoundStoreResourceClient, StoreResourceClient};
    use bytes::Bytes;
    use futures_util::{stream, TryStreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_object_upload_preserves_native_reads_and_uncommitted_cleanup() {
        let source = tempfile::tempdir().unwrap();
        trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
            source.path(),
        ))
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut nats = trellis_local_nats::LocalNats::builder()
            .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
            .cache_dir(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/transfer-testkit-cache"),
            )
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
        let context = async_nats::jetstream::new(client.clone());
        let bucket = format!("upload_{}", ulid::Ulid::new());
        let native = context
            .create_object_store(async_nats::jetstream::object_store::Config {
                bucket: bucket.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        let bound = BoundStoreResourceClient::new(client, bucket.clone());
        let key = "payload/é";
        let data: Vec<u8> = (0..(3 * 1024 * 1024 + 7))
            .map(|i| (i % 251) as u8)
            .collect();
        // Uneven short reads and an odd final chunk must not reorder
        // pipelined chunks or change the standard Object Store digest/metadata.
        let reads = data
            .chunks(7919)
            .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)));
        let mut reader = tokio_util::io::StreamReader::new(stream::iter(reads));
        let written = bound.write_from(key, &mut reader).await.unwrap();
        assert_eq!(
            native.info(key).await.unwrap().chunks,
            data.len().div_ceil(CHUNK_BYTES),
            "short reads must fill whole chunks rather than publish one message per read"
        );
        let mut object_stream = context.get_stream(format!("OBJ_{bucket}")).await.unwrap();
        let (mut destination, mut received) = tokio::io::duplex(CHUNK_BYTES);
        let reading = {
            let bound = bound.clone();
            tokio::spawn(async move { bound.read_into(key, &mut destination).await })
        };
        let mut recovered = vec![0u8; 1024 * 1024];
        received.read_exact(&mut recovered).await.unwrap();
        let consumer = object_stream
            .consumer_names()
            .try_next()
            .await
            .unwrap()
            .expect("active streaming reader");
        let paused = object_stream
            .get_consumer::<async_nats::jetstream::consumer::pull::Config>(&consumer)
            .await
            .unwrap();
        let mut independent = Vec::new();
        native
            .get(key)
            .await
            .unwrap()
            .read_to_end(&mut independent)
            .await
            .unwrap();
        assert_eq!(independent, data);
        assert!(
            paused.get_info().await.unwrap().delivered.consumer_sequence
                < native.info(key).await.unwrap().chunks as u64,
            "a paused reader must not drain the entire object in the background"
        );
        object_stream.delete_consumer(&consumer).await.unwrap();
        received.read_to_end(&mut recovered).await.unwrap();
        assert!(reading.await.unwrap().unwrap().is_some());
        assert_eq!(
            recovered, data,
            "consumer loss must resume at the application cursor, including buffered read-ahead"
        );
        let mut verified = Vec::new();
        native
            .get(key)
            .await
            .unwrap()
            .read_to_end(&mut verified)
            .await
            .unwrap();
        assert_eq!(verified, data);
        assert_eq!(written.size, data.len() as u64);

        bound
            .write_from("empty", &mut tokio::io::empty())
            .await
            .unwrap();
        verified.clear();
        native
            .get("empty")
            .await
            .unwrap()
            .read_to_end(&mut verified)
            .await
            .unwrap();
        assert!(verified.is_empty());
        let mut left = &b"left concurrent object"[..];
        let mut right = &b"right concurrent object"[..];
        let (left_result, right_result) = tokio::join!(
            bound.write_from("left", &mut left),
            bound.write_from("right", &mut right)
        );
        left_result.unwrap();
        right_result.unwrap();
        for (key, expected) in [
            ("left", &b"left concurrent object"[..]),
            ("right", &b"right concurrent object"[..]),
        ] {
            verified.clear();
            native
                .get(key)
                .await
                .unwrap()
                .read_to_end(&mut verified)
                .await
                .unwrap();
            assert_eq!(verified, expected);
        }

        let committed_messages = object_stream.info().await.unwrap().state.messages;
        let reads = [
            Ok(Bytes::from_static(b"uncommitted replacement")),
            Err(std::io::Error::other("source interrupted")),
        ];
        let mut interrupted = tokio_util::io::StreamReader::new(stream::iter(reads));
        assert!(bound.write_from(key, &mut interrupted).await.is_err());
        verified.clear();
        native
            .get(key)
            .await
            .unwrap()
            .read_to_end(&mut verified)
            .await
            .unwrap();
        assert_eq!(
            verified, data,
            "failed replacement must preserve the committed object"
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while object_stream.info().await.unwrap().state.messages != committed_messages {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let (mut producer, mut reader) = tokio::io::duplex(CHUNK_BYTES);
        let writing = {
            let bound = bound.clone();
            tokio::spawn(async move { bound.write_from("cancelled", &mut reader).await })
        };
        // One full chunk publishes while the writer still awaits the rest.
        producer.write_all(&vec![7; CHUNK_BYTES + 5]).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while object_stream.info().await.unwrap().state.messages <= committed_messages {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        writing.abort();
        assert!(writing.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while object_stream.info().await.unwrap().state.messages != committed_messages {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            native.info("cancelled").await.unwrap_err().kind(),
            InfoErrorKind::NotFound
        );

        // Exercise many refills through a real bounded destination pipe.
        // Small objects cannot expose refill/accounting failures after many pulls.
        let chunks =
            (0..2048).map(|_| Ok::<_, std::io::Error>(Bytes::from_static(&[0x5a; 64 * 1024])));
        let mut source = tokio_util::io::StreamReader::new(stream::iter(chunks));
        let large = bound
            .write_from("large-backpressure", &mut source)
            .await
            .unwrap();
        {
            let (mut destination, mut reader) = tokio::io::duplex(128 * 1024);
            let reading = {
                let bound = bound.clone();
                tokio::spawn(async move {
                    bound
                        .read_into("large-backpressure", &mut destination)
                        .await
                })
            };
            let mut received = 0u64;
            let mut buffer = [0u8; 16 * 1024];
            loop {
                let count = reader.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                assert!(buffer[..count].iter().all(|byte| *byte == 0x5a));
                received += count as u64;
            }
            reading.await.unwrap().unwrap();
            assert_eq!(received, large.size);
        }

        // A real server-rejected chunk ACK must prevent metadata publication.
        let mut config = object_stream.info().await.unwrap().config.clone();
        config.max_message_size = 1024;
        context.update_stream(config).await.unwrap();
        assert!(bound
            .write_from("rejected", &mut std::io::Cursor::new(vec![9; 2048]))
            .await
            .is_err());
        assert_eq!(
            native.info("rejected").await.unwrap_err().kind(),
            InfoErrorKind::NotFound
        );
        nats.stop().unwrap();
    }
}
