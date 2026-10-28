//! Unsigned same-broker Core NATS lower bound, mirroring `raw_transfer.ts`.
//! Not an application transfer API and not a Trellis protocol equivalent.
use crate::{now, Error, Hasher, Payload, Sample};
use bytes::BytesMut;
use futures_util::StreamExt;
use std::{path::Path, time::Instant};
use tokio::io::AsyncReadExt;

const WINDOW_BYTES: usize = 4 * 1024 * 1024;

/// Run the diagnostic windowed streams for every size and window configuration.
pub(crate) async fn run(
    url: &str,
    creds: &Path,
    sizes: &[usize],
    samples: usize,
    warmups: usize,
) -> Result<Vec<Sample>, Error> {
    let connect = || async {
        async_nats::ConnectOptions::with_credentials_file(creds)
            .await?
            .connect(url)
            .await
            .map_err(Error::from)
    };
    let sender = connect().await?;
    let receiver = connect().await?;
    let mut rows = Vec::new();
    let mut counter = 0u64;
    for &size in sizes {
        let mut expected = Hasher::default();
        tokio::io::copy(&mut Payload::new(size), &mut expected).await?;
        let expected = expected.digest();
        for (requested, requested_frames) in [(64 * 1024, 16), (64 * 1024, 64), (1024 * 1024, 16)] {
            let frame = requested.min(sender.server_info().max_payload - 4096);
            for trial in 0..(warmups + samples) {
                counter += 1;
                let subject = format!("benchmark.raw.{}.{counter}", std::process::id());
                let mut data = receiver.subscribe(format!("{subject}.data")).await?;
                let mut credit = sender.subscribe(format!("{subject}.credit")).await?;
                sender.flush().await?;
                receiver.flush().await?;
                let started_unix_ms = now();
                let started = Instant::now();
                let frames = size.div_ceil(frame);
                let read = async {
                    let mut hasher = Hasher::default();
                    let (mut sequence, mut offset) = (0usize, 0usize);
                    let (mut credited_sequence, mut credited_bytes) = (0usize, 0usize);
                    while let Some(message) = data.next().await {
                        sequence += 1;
                        let header = message
                            .headers
                            .as_ref()
                            .and_then(|headers| headers.get("sequence"))
                            .map(|value| value.as_str().to_owned());
                        if header.as_deref() != Some(sequence.to_string().as_str())
                            || message.payload.len() != frame.min(size - offset)
                        {
                            return Err::<usize, Error>(
                                "raw stream sequence/length mismatch".into(),
                            );
                        }
                        hasher.update(&message.payload);
                        offset += message.payload.len();
                        if sequence - credited_sequence >= 8
                            || offset - credited_bytes >= 1024 * 1024
                            || offset == size
                        {
                            receiver
                                .publish(format!("{subject}.credit"), sequence.to_string().into())
                                .await?;
                            credited_sequence = sequence;
                            credited_bytes = offset;
                        }
                        if offset == size {
                            break;
                        }
                    }
                    if offset != size || hasher.digest() != expected {
                        return Err("corrupt raw Core NATS stream".into());
                    }
                    Ok(sequence)
                };
                let write = async {
                    let window = requested_frames.min(WINDOW_BYTES / frame);
                    let mut source = Payload::new(size);
                    let (mut sent, mut consumed) = (0usize, 0usize);
                    while sent < frames || consumed < sent {
                        if sent < frames && sent - consumed < window {
                            let length = frame.min(size - sent * frame);
                            let mut buffer = BytesMut::zeroed(length);
                            source.read_exact(&mut buffer).await?;
                            sent += 1;
                            let mut headers = async_nats::HeaderMap::new();
                            headers.insert("sequence", sent.to_string().as_str());
                            sender
                                .publish_with_headers(
                                    format!("{subject}.data"),
                                    headers,
                                    buffer.freeze(),
                                )
                                .await?;
                        } else {
                            let message = credit.next().await.ok_or("raw credit stream closed")?;
                            let value = std::str::from_utf8(&message.payload)?;
                            let cursor: usize = value.parse()?;
                            if cursor.to_string() != value || cursor <= consumed || cursor > sent {
                                return Err::<(), Error>("invalid raw cumulative credit".into());
                            }
                            consumed = cursor;
                        }
                    }
                    Ok(())
                };
                let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    tokio::try_join!(read, write)
                })
                .await;
                let (frames_seen, error) = match outcome {
                    Ok(Ok((frames_seen, ()))) => (Some(frames_seen), None),
                    Ok(Err(error)) => (None, Some(error.to_string())),
                    Err(_) => (None, Some("raw stream exceeded 30 s".to_owned())),
                };
                let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
                rows.push(Sample {
                    scenario: format!(
                        "rust/windowed-stream-{frame}{}",
                        if requested_frames == 16 {
                            String::new()
                        } else {
                            format!("-frames{requested_frames}")
                        }
                    ),
                    transport: "nats",
                    started_unix_ms,
                    duration_ms,
                    bytes: Some(size),
                    error,
                    warmup: trial < warmups,
                    max_frame_bytes: Some(frame),
                    window_frames: Some(requested_frames),
                    window_bytes: Some(WINDOW_BYTES),
                    data_frames: frames_seen,
                    verified_ms: Some(duration_ms),
                    ..Sample::default()
                });
                data.unsubscribe().await.ok();
                credit.unsubscribe().await.ok();
            }
        }
    }
    sender.drain().await.ok();
    receiver.drain().await.ok();
    Ok(rows)
}
