import type { ObjectInfo } from "@nats-io/obj";
import { nanos, nuid } from "@nats-io/nats-core";
import {
  type ConnectionClosedListener,
  type NatsConnection,
  NatsConnectionImpl,
  QueuedIteratorImpl,
} from "@nats-io/nats-core/internal";
import {
  AckPolicy,
  type Consumer,
  type ConsumerMessages,
  DeliverPolicy,
  jetstream,
  JetStreamApiCodes,
  JetStreamApiError,
  jetstreamManager,
  type JsMsg,
} from "@nats-io/jetstream";
import { JetStreamStatusError } from "@nats-io/jetstream/internal";
import { sha256 } from "@noble/hashes/sha256";
import { base64urlDecode, base64urlEncode } from "./auth/utils.ts";
import { StoreError } from "./errors/StoreError.ts";

/** Internal ObjectStore chunk reader: one finite batch, pulled only on demand. */
export function boundedObjectStream(
  nc: NatsConnection,
  bucket: string,
  info: ObjectInfo,
  release: () => void,
): ReadableStream<Uint8Array> {
  if (!(nc instanceof NatsConnectionImpl)) {
    throw new Error("ObjectStore reads require a native NATS connection");
  }
  if (
    info.bucket !== bucket || info.options?.link ||
    !/^[A-Za-z0-9_-]+$/.test(info.nuid) ||
    !Number.isSafeInteger(info.size) || info.size < 0 ||
    !Number.isSafeInteger(info.chunks) || info.chunks < 0 ||
    (info.size === 0) !== (info.chunks === 0) ||
    !/^SHA-256=[A-Za-z0-9_-]{43}=?$/.test(info.digest)
  ) throw new Error("invalid or unsupported ObjectStore metadata");
  const expected = base64urlDecode(info.digest.slice(8));
  if (
    expected.length !== 32 ||
    base64urlEncode(expected) !== info.digest.slice(8).replace(/=$/, "")
  ) {
    throw new Error("invalid ObjectStore SHA-256 digest");
  }
  const hash = sha256.create();
  if (info.size === 0) {
    try {
      if (base64urlEncode(hash.digest()) !== base64urlEncode(expected)) {
        throw new Error("empty ObjectStore digest mismatch");
      }
    } finally {
      hash.destroy();
    }
    release();
    return new ReadableStream<Uint8Array>({
      start(controller) {
        controller.close();
      },
    });
  }
  const stream = `OBJ_${bucket}`;
  const subject = `$O.${bucket}.C.${info.nuid}`;
  const maxPayload = nc.info?.max_payload;
  if (!Number.isSafeInteger(maxPayload) || !maxPayload || maxPayload < 1) {
    hash.destroy();
    throw new Error("NATS maximum payload is unavailable");
  }
  // Msg.size() includes subject and ACK reply as well as payload/headers.
  // The fixed ACK reserve never grows to accommodate an object or a message.
  const budget = Math.max(1024 * 1024, maxPayload) +
    new TextEncoder().encode(subject).length +
    new TextEncoder().encode(stream).length + 1024;
  const consumerName = `TRELLIS_STORE_${nuid.next()}`;
  if (nc.isClosed()) {
    hash.destroy();
    throw new Error("ObjectStore read connection closed");
  }
  const statuses = nc.status();
  if (!(statuses instanceof QueuedIteratorImpl)) {
    hash.destroy();
    throw new Error(
      "ObjectStore reads require stoppable native NATS status ownership",
    );
  }
  let statusTask: Promise<void> | undefined;
  let consumer: Promise<Consumer> | undefined;
  let created = false;
  let pendingBatch: Promise<ConsumerMessages> | undefined;
  let batch: ConsumerMessages | undefined;
  let iterator: AsyncIterator<JsMsg> | undefined;
  let controller: ReadableStreamDefaultController<Uint8Array>;
  let state: "open" | "finishing" | "cancelled" | "failed" | "closed" = "open";
  // Re-read state across asynchronous cancellation, rather than narrowing a
  // stale pre-await value in the pull path.
  const isState = (expected: typeof state): boolean => state === expected;
  let cleanup: Promise<void> | undefined;
  let bytes = 0;
  let chunks = 0;
  let sequence = 0;
  let epochBase = 0;
  let batchMessages = 0;
  let batchBytes = 0;
  const byteBoundary = (cause: unknown): boolean =>
    batchMessages > 0 && cause instanceof JetStreamStatusError &&
    cause.code === 409 && cause.message === "message size exceeds maxbytes";
  const createConsumer = async (): Promise<Consumer> => {
    const manager = await jetstreamManager(nc, { checkAPI: false });
    created = true;
    await manager.consumers.add(stream, {
      name: consumerName,
      filter_subject: subject,
      deliver_policy: DeliverPolicy.StartSequence,
      opt_start_seq: sequence + 1,
      ack_policy: AckPolicy.None,
      max_deliver: 1,
      max_waiting: 1,
      max_batch: 100,
      max_bytes: budget,
      inactive_threshold: nanos(5 * 60 * 1000),
      num_replicas: 1,
    });
    epochBase = chunks;
    return await jetstream(nc).consumers.get(stream, consumerName);
  };

  const finish = (): Promise<void> => {
    if (cleanup) return cleanup;
    if (isState("open")) state = "finishing";
    nc.removeCloseListener(onClose);
    statuses.stop();
    batch?.stop();
    cleanup = (async () => {
      try {
        // Creation and fetch are owned even if cancel interleaves their awaits.
        await consumer?.catch(() => {});
        await pendingBatch?.catch(() => {});
        try {
          if (batch) {
            batch.stop();
            await iterator?.return?.();
            const error = await batch.close();
            if (error && !byteBoundary(error)) throw error;
          }
        } finally {
          batch = undefined;
          iterator = undefined;
          pendingBatch = undefined;
          if (created) {
            const manager = await jetstreamManager(nc, { checkAPI: false });
            try {
              await manager.consumers.delete(stream, consumerName);
            } catch (cause) {
              if (
                !(cause instanceof JetStreamApiError) ||
                (cause.code !== JetStreamApiCodes.ConsumerNotFound &&
                  cause.code !== JetStreamApiCodes.StreamNotFound)
              ) throw cause;
            }
          }
        }
      } finally {
        try {
          await statusTask;
        } finally {
          hash.destroy();
          release();
        }
      }
    })();
    return cleanup;
  };
  const fail = async (cause: unknown): Promise<void> => {
    if (isState("cancelled") || isState("failed") || isState("closed")) {
      return;
    }
    state = "failed";
    try {
      await finish();
    } catch (cleanupError) {
      cause = new AggregateError(
        [cause, cleanupError],
        "ObjectStore read and cleanup failed",
      );
    }
    controller.error(
      new StoreError({
        operation: "stream",
        cause,
        context: { key: info.name },
      }),
    );
  };
  const onClose: ConnectionClosedListener = {
    connectionClosedCallback(cause) {
      // Native close listeners are removable, unlike nc.closed().then callbacks.
      void fail(cause ?? new Error("ObjectStore read connection closed"));
    },
  };

  return new ReadableStream<Uint8Array>({
    start(c) {
      controller = c;
      nc.addCloseListener(onClose);
      statusTask = (async () => {
        try {
          for await (const status of statuses) {
            if (status.type === "disconnect") {
              // Do not await our own cleanup: it joins this status task.
              void fail(new Error("ObjectStore read connection disconnected"));
              break;
            }
          }
        } catch (cause) {
          void fail(cause);
        }
      })();
    },
    async pull() {
      if (!isState("open")) return;
      try {
        consumer ??= createConsumer();
        let active = await consumer;
        if (!isState("open")) return;
        while (isState("open")) {
          if (!batch) {
            // Inactivity expires backend consumers, not the public reader. Use
            // typed INFO errors; fetch status errors erase the missing-resource
            // distinction. Resume only from the last actually yielded chunk.
            consumer = (async () => {
              try {
                await active.info();
                return active;
              } catch (cause) {
                if (
                  !(cause instanceof JetStreamApiError) ||
                  cause.code !== JetStreamApiCodes.ConsumerNotFound
                ) throw cause;
                if (!isState("open")) return active;
                return await createConsumer();
              }
            })();
            active = await consumer;
            if (!isState("open")) return;
            // 3.4.0 prohibits max_messages with max_bytes and caps this at 100.
            pendingBatch = active.fetch({ max_bytes: budget, expires: 30_000 })
              .then((value) => {
                batch = value;
                iterator = value[Symbol.asyncIterator]();
                batchBytes = 0;
                batchMessages = 0;
                return value;
              });
            await pendingBatch;
            if (!isState("open")) return;
          }
          let next: IteratorResult<JsMsg>;
          try {
            next = await iterator!.next();
          } catch (cause) {
            if (!byteBoundary(cause)) throw cause;
            next = { done: true, value: undefined };
          }
          if (!isState("open")) return;
          if (next.done) {
            const error = await batch!.closed();
            if (error && !byteBoundary(error)) throw error;
            await iterator!.return?.();
            batch = undefined;
            iterator = undefined;
            pendingBatch = undefined;
            if (batchMessages === 0) {
              throw new Error("ObjectStore chunks are missing");
            }
            continue;
          }
          const msg = next.value;
          batchMessages++;
          // The server/SDK max_bytes account the full message envelope. JsMsg
          // exposes only the body, which we independently bound as retained data.
          batchBytes += msg.data.length;
          bytes += msg.data.length;
          chunks++;
          if (
            batchMessages > 100 || batchBytes > budget ||
            msg.subject !== subject || msg.data.length === 0 ||
            msg.info.streamSequence <= sequence ||
            msg.info.deliverySequence !== chunks - epochBase ||
            bytes > info.size || chunks > info.chunks
          ) throw new Error("invalid ObjectStore chunk sequence or size");
          sequence = msg.info.streamSequence;
          hash.update(msg.data);
          if (chunks === info.chunks || msg.info.pending === 0) {
            if (
              chunks !== info.chunks || bytes !== info.size ||
              msg.info.pending !== 0 ||
              base64urlEncode(hash.digest()) !== base64urlEncode(expected)
            ) {
              throw new Error(
                "ObjectStore size, chunk count or digest mismatch",
              );
            }
            await finish();
            if (!isState("finishing")) return;
            controller.enqueue(msg.data);
            state = "closed";
            controller.close();
            return;
          }
          controller.enqueue(msg.data);
          return;
        }
      } catch (cause) {
        await fail(cause);
      }
    },
    async cancel() {
      state = "cancelled";
      try {
        await finish();
      } catch (cause) {
        throw new StoreError({
          operation: "stream",
          cause,
          context: { key: info.name },
        });
      }
    },
  }, { highWaterMark: 0 });
}
