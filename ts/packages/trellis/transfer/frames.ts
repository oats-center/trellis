import { recordCatalogUpDown } from "../telemetry/metrics.ts";
import { transferWait } from "./session.ts";

/**
 * Supported streaming upload sources. Iterator inputs may reuse a scratch
 * buffer between chunks: prefetch snapshots bounded blocks before advancing.
 * Uint8Array/ArrayBuffer bodies remain caller-owned and must stay unchanged
 * until the upload settles. Queued prefetch is capped at one formed frame
 * plus one frame-sized block, each within the negotiated limit, excluding
 * producer-provided backing buffers.
 */
export type TransferBody =
  | Uint8Array
  | ArrayBuffer
  | ReadableStream<Uint8Array>
  | AsyncIterable<Uint8Array>;

/** Combine ready chunks without waiting to fill a frame or buffering the object. */
export async function* frames(
  body: TransferBody,
  maxBytes: number,
  signal: AbortSignal,
  direction: "upload" | "download",
): AsyncGenerator<Uint8Array> {
  // A 256 KiB ready-frame target still fills the 4 MiB / 16-frame window.
  // Larger ready frames measured faster in bulk but delayed concurrent Live opens.
  maxBytes = Math.min(maxBytes, 256 * 1024);
  const reader = body instanceof ReadableStream ? body.getReader() : undefined;
  const owned = !(body instanceof ArrayBuffer || body instanceof Uint8Array);
  const source: AsyncIterable<Uint8Array> =
    body instanceof ArrayBuffer || body instanceof Uint8Array
      ? (async function* () {
          yield body instanceof Uint8Array ? body : new Uint8Array(body);
        })()
      : body;
  const iterator: AsyncIterator<Uint8Array> = reader
    ? {
        next: async () => {
          const result = await reader.read();
          return result.done
            ? { done: true, value: undefined }
            : { done: false, value: result.value };
        },
      }
    : source[Symbol.asyncIterator]();
  let stopped = false;
  let bufferedBytes = 0;
  let sourceChunk: Uint8Array | undefined;
  let sourceOffset = 0;
  let controller: ReadableStreamDefaultController<Uint8Array> | undefined;
  // The platform serializes pulls and bounds queued bytes (plus one bounded
  // block). It can refill while the previous frame is signed or backpressured.
  const buffered = new ReadableStream<Uint8Array>(
    {
      start(value) {
        controller = value;
      },
      async pull(controller) {
        if (stopped) return;
        while (!sourceChunk?.length) {
          const next = await iterator.next();
          if (stopped) return;
          if (next.done) {
            controller.close();
            return;
          }
          sourceChunk = next.value;
          sourceOffset = 0;
        }
        const view = sourceChunk.subarray(
          sourceOffset,
          sourceOffset + maxBytes,
        );
        // An iterator may reuse its scratch buffer on its next call. Snapshot
        // only this bounded block before allowing source prefetch to advance.
        const chunk = owned ? new Uint8Array(view) : view;
        sourceOffset += view.length;
        if (sourceOffset === sourceChunk.length) sourceChunk = undefined;
        if (owned) {
          bufferedBytes += chunk.length;
          recordCatalogUpDown("trellis.transfer.buffered.bytes", chunk.length, {
            "trellis.direction": direction,
            "trellis.side": direction === "upload" ? "client" : "provider",
          });
        }
        controller.enqueue(chunk);
      },
    },
    new ByteLengthQueuingStrategy({ highWaterMark: maxBytes }),
  ).getReader();
  const stop = () => {
    if (stopped) return;
    stopped = true;
    void buffered.cancel(signal.reason).catch(() => {});
    if (reader) void reader.cancel(signal.reason).catch(() => {});
    else
      void Promise.resolve()
        .then(() => iterator.return?.())
        .catch(() => {});
  };
  signal.addEventListener("abort", stop, { once: true });
  if (signal.aborted) stop();
  let completed = false;
  let pending: Uint8Array | undefined;
  try {
    while (!completed) {
      let chunk = pending;
      pending = undefined;
      if (!chunk) {
        const next = await transferWait(buffered.read(), signal);
        if (next.done) {
          completed = true;
          break;
        }
        chunk = next.value;
      }
      if (!chunk.length) continue;
      const first = chunk.subarray(0, maxBytes);
      const pieces = [first];
      let length = first.length;
      if (chunk.length > length) pending = chunk.subarray(length);
      // desiredSize proves a read is already queued; never await a producer
      // after obtaining the first available bytes of this frame.
      while (
        !pending &&
        length < maxBytes &&
        (controller?.desiredSize ?? maxBytes) < maxBytes
      ) {
        const next = await buffered.read();
        if (next.done) {
          completed = true;
          break;
        }
        const count = Math.min(next.value.length, maxBytes - length);
        pieces.push(next.value.subarray(0, count));
        length += count;
        if (next.value.length > count) pending = next.value.subarray(count);
      }
      let frame = first;
      if (pieces.length > 1) {
        frame = new Uint8Array(length);
        let offset = 0;
        for (const piece of pieces) {
          frame.set(piece, offset);
          offset += piece.length;
        }
      }
      try {
        yield frame;
      } finally {
        if (owned) {
          bufferedBytes -= frame.length;
          recordCatalogUpDown(
            "trellis.transfer.buffered.bytes",
            -frame.length,
            {
              "trellis.direction": direction,
              "trellis.side": direction === "upload" ? "client" : "provider",
            },
          );
        }
      }
    }
  } finally {
    signal.removeEventListener("abort", stop);
    if (!completed) stop();
    sourceChunk = undefined;
    buffered.releaseLock();
    reader?.releaseLock();
    if (bufferedBytes)
      recordCatalogUpDown("trellis.transfer.buffered.bytes", -bufferedBytes, {
        "trellis.direction": direction,
        "trellis.side": direction === "upload" ? "client" : "provider",
      });
  }
}
