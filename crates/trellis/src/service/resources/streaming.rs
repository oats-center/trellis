use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UploadReadFailure {
    TooLarge {
        attempted_bytes: u64,
        max_bytes: u64,
    },
    SizeMismatch {
        expected_bytes: u64,
        actual_bytes: u64,
    },
    Cancelled,
}

pub(super) struct GuardedUploadReader<R> {
    reader: R,
    validated_eof: Arc<AtomicBool>,
    expected_size: Option<u64>,
    max_size: Option<u64>,
    read: u64,
    pub(super) failure: Option<UploadReadFailure>,
}

impl<R> GuardedUploadReader<R> {
    pub(super) fn new(
        reader: R,
        expected_size: Option<u64>,
        max_size: Option<u64>,
        validated_eof: Arc<AtomicBool>,
    ) -> Self {
        Self {
            reader,
            validated_eof,
            expected_size,
            max_size,
            read: 0,
            failure: None,
        }
    }

    fn limit(&self) -> Option<u64> {
        match (self.expected_size, self.max_size) {
            (Some(expected), Some(max)) => Some(expected.min(max)),
            (Some(expected), None) => Some(expected),
            (None, max) => max,
        }
    }

    fn crossing_failure(&self) -> UploadReadFailure {
        if let Some(expected_bytes) = self.expected_size {
            UploadReadFailure::SizeMismatch {
                expected_bytes,
                actual_bytes: expected_bytes.saturating_add(1),
            }
        } else {
            let max_bytes = self.max_size.expect("a crossing requires a size limit");
            UploadReadFailure::TooLarge {
                attempted_bytes: max_bytes.saturating_add(1),
                max_bytes,
            }
        }
    }
}

impl<R> AsyncRead for GuardedUploadReader<R>
where
    R: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.validated_eof.load(Ordering::Acquire) {
            return Poll::Ready(Ok(()));
        }
        if this.failure.is_some() {
            return Poll::Ready(Err(std::io::Error::other("guarded upload terminated")));
        }
        let limit = this.limit();
        if limit.is_some_and(|limit| this.read == limit) {
            let mut extra = [0_u8; 1];
            let mut probe = ReadBuf::new(&mut extra);
            return match Pin::new(&mut this.reader).poll_read(cx, &mut probe) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) if probe.filled().is_empty() => {
                    this.validated_eof.store(true, Ordering::Release);
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Ok(())) => {
                    this.failure = Some(this.crossing_failure());
                    Poll::Ready(Err(std::io::Error::other(
                        "store upload size limit exceeded",
                    )))
                }
            };
        }

        let remaining = limit
            .map(|limit| usize::try_from(limit - this.read).unwrap_or(usize::MAX))
            .unwrap_or(buf.remaining())
            .min(buf.remaining());
        let unfilled = buf.initialize_unfilled_to(remaining);
        let mut bounded = ReadBuf::new(unfilled);
        match Pin::new(&mut this.reader).poll_read(cx, &mut bounded) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {
                let count = bounded.filled().len();
                buf.advance(count);
                let Some(actual) = this
                    .read
                    .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
                else {
                    return Poll::Ready(Err(std::io::Error::other("store upload size overflow")));
                };
                this.read = actual;
                if count == 0 {
                    if let Some(expected_bytes) = this.expected_size {
                        if actual != expected_bytes {
                            this.failure = Some(UploadReadFailure::SizeMismatch {
                                expected_bytes,
                                actual_bytes: actual,
                            });
                            return Poll::Ready(Err(std::io::Error::other(
                                "store upload size mismatch",
                            )));
                        }
                    }
                    this.validated_eof.store(true, Ordering::Release);
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}
