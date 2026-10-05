//! Frame ownership at the storage streaming boundary.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use super::telemetry::Payload;
use bytes::Buf;
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::{mpsc, watch},
};

/// Latest whole-frame consumption; storage reads, not network admission, advance it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Consumed {
    pub seq: u64,
    pub frame_bytes: u64,
    pub bytes: u64,
}

#[derive(Debug)]
pub(super) enum Ingress {
    Frame { seq: u64, bytes: Payload },
    Complete,
}

/// An ordinary byte reader that retains frame boundaries without copying frames.
pub(super) struct FrameReader {
    receiver: mpsc::Receiver<Ingress>,
    current: Option<(u64, u64, Payload)>,
    consumed: watch::Sender<Consumed>,
    complete: bool,
}

impl FrameReader {
    pub(super) fn new(
        receiver: mpsc::Receiver<Ingress>,
        consumed: watch::Sender<Consumed>,
    ) -> Self {
        Self {
            receiver,
            current: None,
            consumed,
            complete: false,
        }
    }
}

impl AsyncRead for FrameReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buf.remaining() == 0 || this.complete {
            return Poll::Ready(Ok(()));
        }
        if this.current.is_none() {
            match this.receiver.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ingress::Frame { seq, bytes })) => {
                    this.current = Some((seq, bytes.len() as u64, bytes));
                }
                Poll::Ready(Some(Ingress::Complete)) => {
                    this.complete = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(None) => {
                    return Poll::Ready(Err(io::Error::other(
                        "upload transfer aborted before validated completion",
                    )))
                }
            }
        }
        let (seq, len, bytes) = this.current.as_mut().expect("frame installed");
        let count = buf.remaining().min(bytes.len());
        buf.put_slice(&bytes[..count]);
        bytes.bytes.advance(count);
        if bytes.is_empty() {
            let previous = *this.consumed.borrow();
            let Some(total) = previous.bytes.checked_add(*len) else {
                return Poll::Ready(Err(io::Error::other("upload consumed byte count overflow")));
            };
            this.consumed.send_replace(Consumed {
                seq: *seq,
                frame_bytes: *len,
                bytes: total,
            });
            this.current = None;
        }
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tokio::io::AsyncReadExt;
    use trellis_protocol::transfer::TransferDirection;

    #[tokio::test]
    async fn partial_reads_only_credit_complete_frames_and_abort_is_not_eof() {
        let (tx, rx) = mpsc::channel(2);
        let (consumed, progress) = watch::channel(Consumed::default());
        let mut reader = FrameReader::new(rx, consumed);
        tx.send(Ingress::Frame {
            seq: 1,
            bytes: Payload::new(
                Bytes::from_static(b"abcd"),
                TransferDirection::Send,
                "provider-rx",
            ),
        })
        .await
        .unwrap();
        let mut first = [0; 2];
        reader.read_exact(&mut first).await.unwrap();
        assert_eq!(&first, b"ab");
        assert_eq!(progress.borrow().bytes, 0);
        reader.read_exact(&mut first).await.unwrap();
        assert_eq!(&first, b"cd");
        assert_eq!(progress.borrow().bytes, 4);
        drop(tx);
        assert!(reader.read(&mut first).await.is_err());
    }

    #[tokio::test]
    async fn validated_completion_drains_all_frames_before_clean_eof() {
        let (tx, rx) = mpsc::channel(3);
        let (consumed, progress) = watch::channel(Consumed::default());
        let mut reader = FrameReader::new(rx, consumed);
        tx.send(Ingress::Frame {
            seq: 1,
            bytes: Payload::new(
                Bytes::from_static(b"one"),
                TransferDirection::Send,
                "provider-rx",
            ),
        })
        .await
        .unwrap();
        tx.send(Ingress::Frame {
            seq: 2,
            bytes: Payload::new(
                Bytes::from_static(b"two"),
                TransferDirection::Send,
                "provider-rx",
            ),
        })
        .await
        .unwrap();
        tx.send(Ingress::Complete).await.unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"onetwo");
        assert_eq!(progress.borrow().seq, 2);
        assert_eq!(progress.borrow().bytes, 6);
    }
}
