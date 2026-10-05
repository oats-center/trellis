use crate::telemetry::instruments::{add_counter, add_updown, CounterFamily, UpDownFamily};
use bytes::Bytes;
use opentelemetry::KeyValue;
use trellis_protocol::transfer::{
    parse_transfer_control, parse_transfer_signal, TransferControl, TransferDirection,
    TransferFrameDescriptor, TransferFrameKind, TransferSignal,
};

fn attributes(direction: TransferDirection, side: &'static str) -> [KeyValue; 2] {
    [
        KeyValue::new(
            "trellis.direction",
            match direction {
                TransferDirection::Send => "send",
                TransferDirection::Receive => "receive",
            },
        ),
        KeyValue::new("trellis.side", side),
    ]
}

pub(crate) fn observation(
    direction: TransferDirection,
    side: &'static str,
) -> crate::telemetry::lifecycle::Observation {
    crate::telemetry::lifecycle::Observation::start(
        crate::telemetry::instruments::DurationFamily::Transfer,
        attributes(direction, side).into(),
        "aborted",
    )
}

pub(crate) fn frame(descriptor: &TransferFrameDescriptor, payload: &[u8], side: &'static str) {
    let attrs = attributes(descriptor.direction, side);
    if descriptor.kind == TransferFrameKind::Data {
        add_counter(CounterFamily::TransferFrames, 1, &attrs);
        add_counter(
            CounterFamily::TransferWireBytes,
            payload.len() as u64,
            &attrs,
        );
    } else if (descriptor.kind == TransferFrameKind::Control
        && matches!(
            parse_transfer_control(payload),
            Ok(TransferControl::Credit { .. } | TransferControl::EndAck { .. })
        ))
        || (descriptor.kind == TransferFrameKind::Signal
            && matches!(
                parse_transfer_signal(payload),
                Ok(TransferSignal::Credit { .. })
            ))
    {
        add_counter(CounterFamily::TransferCreditControls, 1, &attrs);
    }
}

/// Charge retained payload once, including partial consumption; drop releases it.
#[derive(Debug)]
pub(crate) struct Payload {
    pub(crate) bytes: Bytes,
    charge: Charge,
}
#[derive(Debug)]
pub(crate) struct Charge {
    len: i64,
    attributes: [KeyValue; 2],
}
impl Charge {
    pub(crate) fn new(len: usize, direction: TransferDirection, side: &'static str) -> Self {
        let attributes = attributes(direction, side);
        let len = i64::try_from(len).unwrap_or(i64::MAX);
        add_updown(UpDownFamily::TransferBufferedBytes, len, &attributes);
        Self { len, attributes }
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        add_updown(
            UpDownFamily::TransferBufferedBytes,
            -self.len,
            &self.attributes,
        );
    }
}
impl Payload {
    pub(crate) fn new(bytes: Bytes, direction: TransferDirection, side: &'static str) -> Self {
        Self {
            charge: Charge::new(bytes.len(), direction, side),
            bytes,
        }
    }
    pub(crate) fn into_bytes(self) -> Bytes {
        let Self { bytes, charge } = self;
        drop(charge);
        bytes
    }
}
impl std::ops::Deref for Payload {
    type Target = Bytes;
    fn deref(&self) -> &Bytes {
        &self.bytes
    }
}
impl AsRef<[u8]> for Payload {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

pub(super) struct Pipe {
    stream: tokio::io::DuplexStream,
    charge: std::sync::Arc<std::sync::Mutex<Charge>>,
}

pub(super) fn pipe(
    capacity: usize,
    direction: TransferDirection,
    side: &'static str,
) -> (Pipe, Pipe) {
    let (writer, reader) = tokio::io::duplex(capacity);
    let charge = std::sync::Arc::new(std::sync::Mutex::new(Charge::new(0, direction, side)));
    (
        Pipe {
            stream: writer,
            charge: charge.clone(),
        },
        Pipe {
            stream: reader,
            charge,
        },
    )
}
impl tokio::io::AsyncWrite for Pipe {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let charge = self.charge.clone();
        let mut charge = charge.lock().unwrap_or_else(|error| error.into_inner());
        let result = std::pin::Pin::new(&mut self.stream).poll_write(cx, bytes);
        if let std::task::Poll::Ready(Ok(count)) = &result {
            charge.len += *count as i64;
            add_updown(
                UpDownFamily::TransferBufferedBytes,
                *count as i64,
                &charge.attributes,
            );
        }
        result
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
impl tokio::io::AsyncRead for Pipe {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let charge = self.charge.clone();
        let mut charge = charge.lock().unwrap_or_else(|error| error.into_inner());
        let before = buffer.filled().len();
        let result = std::pin::Pin::new(&mut self.stream).poll_read(cx, buffer);
        let count = buffer.filled().len() - before;
        charge.len -= count as i64;
        add_updown(
            UpDownFamily::TransferBufferedBytes,
            -(count as i64),
            &charge.attributes,
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn backend_pipe_preserves_bytes_across_bounded_backpressure_and_eof() {
        let input = (0..=255).cycle().take(16_384).collect::<Vec<u8>>();
        let (mut writer, mut reader) = pipe(7, TransferDirection::Receive, "provider-tx");
        let mut output = Vec::new();
        let (written, read) = tokio::join!(
            async {
                writer.write_all(&input).await?;
                writer.shutdown().await
            },
            reader.read_to_end(&mut output)
        );
        written.unwrap();
        assert_eq!(read.unwrap(), input.len());
        assert_eq!(output, input);
    }
}
