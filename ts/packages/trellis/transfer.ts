import {
  AsyncResult,
  isErr,
  Result,
  type Result as ResultType,
} from "@oatscenter/result";
import Type, { type Static } from "typebox";
import { Value } from "typebox/value";
import { sha256 } from "@noble/hashes/sha256";
import { type Msg, NoRespondersError } from "@nats-io/nats-core";
import { base64urlEncode } from "./auth/utils.ts";
import type { TrellisAuth } from "./session.ts";
import { TransferError } from "./errors/TransferError.ts";
import { TransportError } from "./errors/TransportError.ts";
import type { TrellisTransportProvider } from "./transport/generations.ts";
import { SenderWindow } from "./data_plane/flow.ts";
import {
  TransferCredit,
  TransferSession,
  transferWait,
} from "./transfer/session.ts";
import { TransferIngress } from "./service/runtime/transfer/queue.ts";
import {
  transferConstants,
  type TransferControlWire as TransferControl,
  transferParseCounter,
  transferParseGrant,
  transferParseSignal,
  transferParseTerminal,
} from "./auth/protocol_wasm.ts";
import {
  recordCatalogCounter,
  recordCatalogDuration,
} from "./telemetry/mod.ts";
import { recordCatalogUpDown } from "./telemetry/metrics.ts";

/** Failures a transfer handle operation can report. */
export type TransferOperationError = TransferError | TransportError;

/** Logical file metadata; no physical storage identifiers are exposed. */
export const FileInfoSchema = Type.Object({
  key: Type.String({ minLength: 1 }),
  size: Type.Integer({ minimum: 0 }),
  updatedAt: Type.String({ minLength: 1 }),
  digest: Type.Optional(Type.String({ minLength: 1 })),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  metadata: Type.Record(Type.String({ minLength: 1 }), Type.String()),
});
/** Logical metadata returned after storage completion. */
export type FileInfo = Static<typeof FileInfoSchema>;
const IdentitySchema = Type.Object({
  connectionId: Type.String({ minLength: 1 }),
  sessionKey: Type.String({ minLength: 1 }),
});
const GrantProperties = {
  format: Type.Literal("trellis.transfer.v2"),
  type: Type.Literal("TransferGrant"),
  service: Type.String({ minLength: 1 }),
  transferId: Type.String({ minLength: 1 }),
  expiresAt: Type.String({ minLength: 1 }),
  provider: IdentitySchema,
  consumer: IdentitySchema,
  dataSubject: Type.String({ minLength: 1 }),
  controlSubject: Type.String({ minLength: 1 }),
  signalSubject: Type.String({ minLength: 1 }),
  maxFrameBytes: Type.Integer({ minimum: 1 }),
  windowFrames: Type.Integer({ minimum: 1 }),
  windowBytes: Type.Integer({ minimum: 1 }),
};
/** A runtime-issued upload session. Applications should not construct grants. */
export const SendTransferGrantSchema = Type.Object({
  ...GrantProperties,
  direction: Type.Literal("send"),
  maxBytes: Type.Optional(Type.Integer({ minimum: 1 })),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  metadata: Type.Optional(
    Type.Record(Type.String({ minLength: 1 }), Type.String()),
  ),
});
/** A runtime-issued download session carrying independently verified metadata. */
export const ReceiveTransferGrantSchema = Type.Object({
  ...GrantProperties,
  direction: Type.Literal("receive"),
  info: Type.Object({
    ...FileInfoSchema.properties,
    digest: Type.String({ minLength: 1 }),
  }),
});
/** Schema for either one-way streaming direction. */
export const TransferGrantSchema = Type.Union([
  SendTransferGrantSchema,
  ReceiveTransferGrantSchema,
]);
/** Prepared caller-to-provider stream. */
export type SendTransferGrant = Static<typeof SendTransferGrantSchema>;
/** Prepared provider-to-caller stream. */
export type ReceiveTransferGrant = Static<typeof ReceiveTransferGrantSchema>;
/** Runtime-owned prepared transfer session. */
export type TransferGrant = Static<typeof TransferGrantSchema>;
/** Supported streaming upload sources. */
export type TransferBody =
  | Uint8Array
  | ArrayBuffer
  | ReadableStream<Uint8Array>
  | AsyncIterable<Uint8Array>;

/** Split arbitrary source chunks using views, without payload-sized copies. */
async function* frames(
  body: TransferBody,
  maxBytes: number,
  signal: AbortSignal,
): AsyncGenerator<Uint8Array> {
  const reader = body instanceof ReadableStream ? body.getReader() : undefined;
  const source: AsyncIterable<Uint8Array> =
    body instanceof ArrayBuffer || body instanceof Uint8Array
      ? (async function* () {
        yield body instanceof Uint8Array ? body : new Uint8Array(body);
      })()
      : body;
  const iterator: AsyncIterator<Uint8Array> = reader
    ? { next: () => reader.read() }
    : source[Symbol.asyncIterator]();
  const cancelReader = () => {
    void reader?.cancel(signal.reason).catch(() => {});
  };
  signal.addEventListener("abort", cancelReader, { once: true });
  let completed = false;
  try {
    while (true) {
      const next = await transferWait(iterator.next(), signal);
      if (next.done) {
        completed = true;
        return;
      }
      const owned =
        !(body instanceof ArrayBuffer || body instanceof Uint8Array);
      if (owned) {
        recordCatalogUpDown(
          "trellis.transfer.buffered.bytes",
          next.value.length,
          { "trellis.direction": "upload" },
        );
      }
      try {
        for (let offset = 0; offset < next.value.length; offset += maxBytes) {
          yield next.value.subarray(
            offset,
            Math.min(next.value.length, offset + maxBytes),
          );
        }
      } finally {
        if (owned) {
          recordCatalogUpDown(
            "trellis.transfer.buffered.bytes",
            -next.value.length,
            { "trellis.direction": "upload" },
          );
        }
      }
    }
  } finally {
    signal.removeEventListener("abort", cancelReader);
    if (reader) {
      if (!completed) cancelReader();
      reader.releaseLock();
    } else if (!completed) {
      // Application iterators need not support interrupting an in-flight next.
      // Observe their return, but never let that application promise pin the SDK.
      void Promise.resolve(iterator.return?.()).catch(() => {});
    }
  }
}

class ClientSession {
  readonly activated = Promise.withResolvers<number>();
  readonly committed = Promise.withResolvers<FileInfo>();
  readonly terminal = Promise.withResolvers<void>();
  window: SenderWindow | undefined;
  ingress: TransferIngress | undefined;
  #controlSeq = 0n;
  #controlLane = Promise.resolve();
  #lane = Promise.resolve();
  #pending = 0;
  #active = false;
  #cancelled = false;
  #activationRequestId: string | undefined;
  #offeredMaxFrameBytes = 0;

  constructor(
    readonly session: TransferSession,
    readonly activationDeadlineMs: number,
    readonly timeoutMs: number,
  ) {
    // Reject only through owned cancellation races; no unobserved rejected gates.
    session.subscriptions.push(
      session.lease.nc.subscribe(session.grant.signalSubject, {
        callback: (error, msg) => {
          if (error) {
            session.fail(error);
            return;
          }
          if (
            ++this.#pending > session.grant.windowFrames ||
            msg.data.length > transferConstants().maxControlBytes
          ) {
            session.fail(new Error("transfer signal flood"));
            return;
          }
          this.#lane = this.#lane.then(() => this.#signal(msg)).finally(() =>
            this.#pending--
          );
          session.run(this.#lane);
        },
      }),
    );
  }

  async #signal(msg: Msg): Promise<void> {
    // Match nats-core's noMux request classifier: only an empty 503 is
    // broker unavailability, never an unsigned Transfer business message.
    if (msg.data.length === 0 && msg.headers?.code === 503) {
      throw new TransportError({
        code: "trellis.request.unavailable",
        message: "Trellis could not reach the transfer provider.",
        hint: "Check that the transfer provider is reachable, then try again.",
        cause: new NoRespondersError(this.session.grant.controlSubject),
      });
    }
    let signal: ReturnType<typeof transferParseSignal>;
    try {
      signal = transferParseSignal(msg.data);
    } catch (error) {
      console.warn("Transfer rejected signal body", {
        transferId: this.session.grant.transferId,
        bodyBytes: msg.data.length,
        maxBodyBytes: transferConstants().maxControlBytes,
        statusCode: msg.headers?.code ?? 0,
        headerCount: msg.headers?.keys().length ?? 0,
        headerValueBytes: msg.headers
          ? [...msg.headers].reduce(
            (bytes, [, values]) =>
              bytes + values.reduce((n, value) =>
                n + new TextEncoder().encode(value).length, 0),
            0,
          )
          : 0,
      });
      throw error;
    }
    if (signal.transferId !== this.session.grant.transferId) {
      throw new Error("transfer signal session mismatch");
    }
    await this.session.verifyProvider(msg, "0", "signal");
    switch (signal.type) {
      case "activated": {
        if (this.#active) throw new Error("duplicate transfer activation");
        if (
          signal.controlSeq !== "1" ||
          signal.requestId !== this.#activationRequestId ||
          signal.maxFrameBytes > this.session.grant.maxFrameBytes ||
          signal.maxFrameBytes > this.#offeredMaxFrameBytes ||
          signal.windowFrames !== this.session.grant.windowFrames ||
          signal.windowBytes !== this.session.grant.windowBytes
        ) {
          throw new Error("invalid negotiated frame size");
        }
        this.#active = true;
        this.activated.resolve(signal.maxFrameBytes);
        break;
      }
      case "credit":
        if (!this.#active || !this.window) {
          throw new Error("unexpected transfer credit");
        }
        if (
          this.window.applyCredit(
            BigInt(signal.receivedSeq),
            BigInt(signal.consumedSeq),
            transferParseCounter(signal.consumedBytes),
          )
        ) throw new Error("invalid transfer credit");
        recordCatalogCounter("trellis.transfer.credit.controls", 1, {
          "trellis.direction": "upload",
        });
        break;
      case "committed":
        if (
          !this.#active || !this.window ||
          !Value.Check(FileInfoSchema, signal.info)
        ) throw new Error("invalid transfer commit");
        if (this.window.validateComplete(BigInt(signal.finalSeq))) {
          throw new Error("incomplete transfer commit");
        }
        this.committed.resolve(signal.info);
        break;
      case "cancelled":
        this.#cancelled = true;
        this.terminal.resolve();
        this.session.fail(new Error("transfer cancelled"));
        break;
      case "error":
        throw new Error(`transfer failed: ${signal.code}`);
    }
  }

  control(
    action: TransferControl["action"],
    receivedSeq = 0n,
    consumedSeq = 0n,
    receiveMaxFrameBytes?: number,
    consumedBytes = 0,
  ): Promise<void> {
    this.#controlLane = this.#controlLane.then(async () => {
      const session = this.session;
      const payload = new TextEncoder().encode(JSON.stringify(
        {
          format: "trellis.transfer.v2",
          type: "control",
          action,
          transferId: session.grant.transferId,
          controlSeq: (++this.#controlSeq).toString(),
          receivedSeq: receivedSeq.toString(),
          consumedSeq: consumedSeq.toString(),
          ...(action === "activate"
            ? { receiveMaxFrameBytes }
            : { consumedBytes: String(consumedBytes) }),
          ...(action === "end-ack" ? { finalSeq: consumedSeq.toString() } : {}),
        },
      ));
      const headers = await session.requestHeaders(
        session.grant.controlSubject,
        payload,
        this.#controlSeq,
        "control",
      );
      if (action === "activate") {
        this.#activationRequestId = headers.get("request-id");
      }
      session.throwIfAborted();
      session.lease.nc.publish(session.grant.controlSubject, payload, {
        headers,
        reply: session.grant.signalSubject,
      });
      if (action === "end-ack") {
        try {
          await transferWait(session.lease.nc.flush(), session.abort.signal);
        } catch (cause) {
          throw new TransportError({
            code: "trellis.request.unavailable",
            message:
              "Trellis could not deliver the transfer completion acknowledgement.",
            hint:
              "Check that the transfer provider is reachable, then try again.",
            cause,
          });
        }
      }
    });
    return this.#controlLane;
  }

  async activate(): Promise<number> {
    const session = this.session;
    const timer = setTimeout(() =>
      session.fail(
        new TransportError({
          code: "trellis.request.unavailable",
          message: "Transfer activation timed out.",
          hint:
            "Check that the transfer provider is responding, then try again.",
        }),
      ), Math.max(0, this.activationDeadlineMs - Date.now()));
    try {
      await session.retainLocal();
      const maxFrameBytes = Math.min(
        session.grant.maxFrameBytes,
        (session.lease.nc.info?.max_payload ?? 0) -
          transferConstants().headerReserve,
      );
      if (maxFrameBytes < 1) {
        throw new Error("NATS payload limit cannot carry transfer frames");
      }
      this.#offeredMaxFrameBytes = maxFrameBytes;
      await transferWait(session.lease.nc.flush(), session.abort.signal);
      await transferWait(
        this.control("activate", 0n, 0n, maxFrameBytes),
        session.abort.signal,
      );
      return await transferWait(this.activated.promise, session.abort.signal);
    } finally {
      clearTimeout(timer);
    }
  }

  async cancel(): Promise<void> {
    const timer = setTimeout(() =>
      this.session.fail(
        new TransportError({
          code: "trellis.request.unavailable",
          message:
            "Transfer cancellation timed out; remote cleanup is unconfirmed.",
          hint:
            "Local ownership has been released. Reconcile the upload Operation before retrying.",
        }),
      ), this.timeoutMs);
    try {
      if (!this.session.abort.signal.aborted) {
        await transferWait(this.control("cancel"), this.session.abort.signal);
        await transferWait(this.terminal.promise, this.session.abort.signal);
      }
    } catch (cause) {
      if (!this.#cancelled) throw cause;
    } finally {
      clearTimeout(timer);
    }
  }
}

abstract class BaseTransferHandle {
  protected constructor(
    readonly transport: TrellisTransportProvider,
    readonly auth: TrellisAuth,
    readonly timeoutMs: number,
  ) {}
  protected async open(grant: TransferGrant): Promise<ClientSession> {
    transferParseGrant(new TextEncoder().encode(JSON.stringify(grant)));
    if (
      grant.consumer.sessionKey !== this.auth.sessionKey ||
      Date.now() >= Date.parse(grant.expiresAt)
    ) throw new Error("invalid or expired transfer grant");
    const activationDeadlineMs = Math.min(
      Date.now() + this.timeoutMs,
      Date.parse(grant.expiresAt),
    );
    const lease = await this.transport.acquireFor({
      publish: grant.direction === "send"
        ? [grant.controlSubject, grant.dataSubject]
        : [grant.controlSubject],
      subscribe: grant.direction === "send"
        ? [grant.signalSubject]
        : [grant.signalSubject, grant.dataSubject],
    }, {
      deadlineMs: activationDeadlineMs,
    });
    return new ClientSession(
      new TransferSession(grant, lease, this.auth, false),
      activationDeadlineMs,
      this.timeoutMs,
    );
  }
}

/** Caller-owned upload helper. Success means backend and required Operation commit. */
export class SendTransferHandle extends BaseTransferHandle {
  constructor(
    transport: TrellisTransportProvider,
    auth: TrellisAuth,
    timeoutMs: number,
    readonly grant: SendTransferGrant,
    _inboxPrefix = "_INBOX",
  ) {
    super(transport, auth, timeoutMs);
  }

  /** Send a bounded one-way byte stream and wait for verified durable commitment. */
  send(body: TransferBody): AsyncResult<FileInfo, TransferOperationError> {
    return AsyncResult.from(
      (async (): Promise<ResultType<FileInfo, TransferOperationError>> => {
        const started = performance.now();
        let client: ClientSession | undefined;
        try {
          client = await this.open(this.grant);
          const session = client.session;
          const maxFrameBytes = await client.activate();
          const window = client.window = new SenderWindow({
            maxFrameBytes,
            windowFrames: this.grant.windowFrames,
            windowBytes: this.grant.windowBytes,
          });
          const hasher = sha256.create();
          let size = 0;
          for await (
            const frame of frames(body, maxFrameBytes, session.abort.signal)
          ) {
            size += frame.length;
            if (
              !Number.isSafeInteger(size) ||
              (this.grant.maxBytes !== undefined && size > this.grant.maxBytes)
            ) throw new Error("transfer maximum size exceeded");
            while (window.validateFrameSlot(frame.length) === "window_full") {
              await transferWait(window.waitCredit(), session.abort.signal);
            }
            if (window.validateFrameSlot(frame.length)) {
              throw new Error("transfer frame exceeds limits");
            }
            const seq = window.nextFrameSeq();
            if (seq === undefined) {
              throw new Error("transfer sequence exhausted");
            }
            const headers = await session.requestHeaders(
              this.grant.dataSubject,
              frame,
              seq,
            );
            session.throwIfAborted();
            session.lease.nc.publish(this.grant.dataSubject, frame, {
              headers,
              reply: this.grant.signalSubject,
            });
            if (window.commitFrame(seq, frame.length)) {
              throw new Error(
                "transfer frame accounting failed",
              );
            }
            recordCatalogCounter("trellis.transfer.frames", 1, {
              "trellis.direction": "upload",
            });
            recordCatalogCounter("trellis.transfer.wire.bytes", frame.length, {
              "trellis.direction": "upload",
            });
            hasher.update(frame);
          }
          const digest = `SHA-256=${base64urlEncode(hasher.digest())}`;
          const completion = new TextEncoder().encode(
            JSON.stringify({
              format: "trellis.transfer.v2",
              type: "complete",
              transferId: this.grant.transferId,
              finalSeq: window.highestSent.toString(),
              size,
              digest,
            }),
          );
          const completionHeaders = await session.requestHeaders(
            this.grant.dataSubject,
            completion,
            window.highestSent,
            "complete",
          );
          session.throwIfAborted();
          session.lease.nc.publish(this.grant.dataSubject, completion, {
            headers: completionHeaders,
            reply: this.grant.signalSubject,
          });
          const info = await transferWait(
            client.committed.promise,
            session.abort.signal,
          );
          if (
            info.size !== size ||
            info.digest?.replace(/=+$/, "") !== digest.replace(/=+$/, "")
          ) throw new Error("transfer committed metadata mismatch");
          observe("upload", started, true);
          return Result.ok(info);
        } catch (cause) {
          // Cancellation has its own exchange budget and never substitutes for success.
          if (client && !client.session.abort.signal.aborted) {
            await client
              .cancel().catch(() => {});
          }
          observe("upload", started, false);
          return Result.err(
            cause instanceof TransportError || cause instanceof TransferError
              ? cause
              : new TransferError({ operation: "send", cause }),
          );
        } finally {
          client?.session.finish();
          await client?.session.join();
        }
      })(),
    );
  }
}

/** Caller-owned download helper; consumption/backpressure drives cumulative credit. */
export class ReceiveTransferHandle extends BaseTransferHandle {
  constructor(
    transport: TrellisTransportProvider,
    auth: TrellisAuth,
    timeoutMs: number,
    readonly grant: ReceiveTransferGrant,
    _inboxPrefix = "_INBOX",
  ) {
    super(transport, auth, timeoutMs);
  }

  /** Open an authenticated push stream pinned to one physical generation. */
  stream(): AsyncResult<ReadableStream<Uint8Array>, TransferOperationError> {
    return AsyncResult.from(
      (async (): Promise<
        ResultType<ReadableStream<Uint8Array>, TransferOperationError>
      > => {
        let client: ClientSession | undefined;
        try {
          client = await this.open(this.grant);
          const session = client.session;
          const C = transferConstants();
          let received = 0n;
          let consumed = 0n;
          let size = 0;
          let lane = Promise.resolve();
          let pendingFrames = 0;
          let pendingBytes = 0;
          let consumedBytes = 0;
          const scheduler = new TransferCredit(
            session,
            {
              frameStep: C.creditFrameStep,
              byteStep: C.creditByteStep,
              maxDelayMs: C.creditMaxDelayMs,
            },
            (received, consumed) =>
              client!.control(
                "credit",
                received,
                consumed,
                undefined,
                consumedBytes,
              ),
          );
          const ingress = new TransferIngress(
            this.grant.maxFrameBytes,
            this.grant.windowFrames,
            this.grant.windowBytes,
            (seq, bytes) => {
              consumed = seq;
              consumedBytes += bytes;
              scheduler.note(received, consumed, consumedBytes);
            },
            "download",
          );
          client.ingress = ingress;
          const hash = sha256.create();
          let streamController:
            | ReadableStreamDefaultController<Uint8Array>
            | undefined;
          let streamTerminated = false;
          const abortStream = () => {
            scheduler.close();
            ingress.fail(session.abort.signal.reason);
            if (streamController && !streamTerminated) {
              streamTerminated = true;
              streamController.error(session.abort.signal.reason);
            }
          };
          session.abort.signal.addEventListener("abort", abortStream, {
            once: true,
          });
          session.subscriptions.push(
            session.lease.nc.subscribe(this.grant.dataSubject, {
              callback: (error, msg) => {
                if (error) {
                  session.fail(error);
                  return;
                }
                if (
                  ++pendingFrames + ingress.bufferedFrames >
                    this.grant.windowFrames +
                      (msg.headers?.get("trellis-transfer-control") === "eof"
                        ? 1
                        : 0) ||
                  (pendingBytes += msg.data.length) + ingress.bufferedBytes >
                    this.grant.windowBytes
                ) {
                  session.fail(
                    new Error("transfer receive verification window exceeded"),
                  );
                  return;
                }
                let verificationRetained = true;
                const handoff = () => {
                  if (!verificationRetained) return;
                  verificationRetained = false;
                  recordCatalogUpDown(
                    "trellis.transfer.buffered.bytes",
                    -msg.data.length,
                    { "trellis.direction": "download" },
                  );
                };
                lane = lane.then(async () => {
                  const negotiated = await transferWait(
                    client!.activated.promise,
                    session.abort.signal,
                  );
                  if (msg.data.length > negotiated) {
                    throw new Error("transfer negotiated frame size exceeded");
                  }
                  const raw = msg.headers?.get(C.sequenceHeader);
                  const seq = BigInt(transferParseCounter(raw ?? ""));
                  const control = msg.headers?.get(C.controlHeader) ?? "";
                  if (control !== "data" && control !== "eof") {
                    throw new Error("invalid transfer download frame kind");
                  }
                  const terminal = control === "eof"
                    ? transferParseTerminal(
                      new TextEncoder().encode(
                        msg.headers?.get(C.terminalHeader) ?? "",
                      ),
                    )
                    : undefined;
                  await session.verifyProvider(
                    msg,
                    seq.toString(),
                    control,
                    terminal,
                  );
                  if (control === "eof") {
                    if (
                      msg.data.length !== 0 || seq !== received ||
                      terminal?.finalSeq !== seq.toString() ||
                      terminal.size !== size ||
                      terminal.digest !== this.grant.info.digest ||
                      size !== this.grant.info.size ||
                      `SHA-256=${base64urlEncode(hash.digest())}`.replace(
                          /=+$/,
                          "",
                        ) !== this.grant.info.digest.replace(/=+$/, "")
                    ) {
                      throw new Error("transfer EOF metadata mismatch");
                    }
                    ingress.close(seq);
                  } else {
                    if (control !== "data" || seq !== received + 1n) {
                      throw new Error("transfer DATA sequence gap");
                    }
                    received = seq;
                    size += msg.data.length;
                    if (size > this.grant.info.size) {
                      throw new Error("transfer download size exceeded");
                    }
                    hash.update(msg.data);
                    handoff();
                    ingress.push(seq, msg.data);
                    recordCatalogCounter("trellis.transfer.frames", 1, {
                      "trellis.direction": "download",
                    });
                    recordCatalogCounter(
                      "trellis.transfer.wire.bytes",
                      msg.data.length,
                      { "trellis.direction": "download" },
                    );
                  }
                }).finally(() => {
                  pendingFrames--;
                  pendingBytes -= msg.data.length;
                  handoff();
                });
                recordCatalogUpDown(
                  "trellis.transfer.buffered.bytes",
                  msg.data.length,
                  { "trellis.direction": "download" },
                );
                session.run(lane);
              },
            }),
          );
          await client.activate();
          const iterator = ingress[Symbol.asyncIterator]();
          const ownedClient = client;
          return Result.ok(
            new ReadableStream<Uint8Array>({
              start(controller) {
                streamController = controller;
                if (session.abort.signal.aborted) abortStream();
              },
              async pull(controller) {
                if (streamTerminated) return;
                try {
                  const next = await transferWait(
                    iterator.next(),
                    session.abort.signal,
                  );
                  if (next.done) {
                    await scheduler.flush();
                    await ownedClient.control(
                      "end-ack",
                      received,
                      consumed,
                      undefined,
                      consumedBytes,
                    );
                    scheduler.close();
                    if (streamTerminated) return;
                    streamTerminated = true;
                    session.abort.signal.removeEventListener(
                      "abort",
                      abortStream,
                    );
                    controller.close();
                    session.finish();
                    await session.join();
                  } else if (!streamTerminated) controller.enqueue(next.value);
                } catch (cause) {
                  scheduler.close();
                  session.fail(cause);
                  session.finish();
                  await session.join();
                  if (!streamTerminated) {
                    streamTerminated = true;
                    session.abort.signal.removeEventListener(
                      "abort",
                      abortStream,
                    );
                    controller.error(cause);
                  }
                }
              },
              async cancel() {
                streamTerminated = true;
                session.abort.signal.removeEventListener("abort", abortStream);
                try {
                  await ownedClient.cancel();
                } finally {
                  scheduler.close();
                  session.finish();
                  await session.join();
                }
              },
            }, { highWaterMark: 0 }),
          );
        } catch (cause) {
          client?.session.finish();
          await client?.session.join();
          return Result.err(
            cause instanceof TransferError || cause instanceof TransportError
              ? cause
              : new TransferError({ operation: "stream", cause }),
          );
        }
      })(),
    );
  }

  /** Collect a verified stream into one final contiguous buffer. */
  bytes(): AsyncResult<Uint8Array, TransferOperationError> {
    return AsyncResult.from(
      (async (): Promise<ResultType<Uint8Array, TransferOperationError>> => {
        const started = performance.now();
        const stream = (await this.stream()).take();
        if (isErr(stream)) return Result.err(stream.error);
        const reader = stream.getReader();
        let retainedBytes = 0;
        try {
          const bytes = new Uint8Array(this.grant.info.size);
          retainedBytes = bytes.length;
          recordCatalogUpDown(
            "trellis.transfer.buffered.bytes",
            retainedBytes,
            { "trellis.direction": "download" },
          );
          let offset = 0;
          while (true) {
            const next = await reader.read();
            if (next.done) break;
            bytes.set(next.value, offset);
            offset += next.value.length;
          }
          observe("download", started, true);
          return Result.ok(bytes);
        } catch (cause) {
          await reader.cancel().catch(() => {});
          observe("download", started, false);
          return Result.err(
            cause instanceof TransferError || cause instanceof TransportError
              ? cause
              : new TransferError({ operation: "bytes", cause }),
          );
        } finally {
          recordCatalogUpDown(
            "trellis.transfer.buffered.bytes",
            -retainedBytes,
            { "trellis.direction": "download" },
          );
          reader.releaseLock();
        }
      })(),
    );
  }
}

/** A direction-specific high-level transfer helper. */
export type TransferHandle = SendTransferHandle | ReceiveTransferHandle;
/** Create a helper for a runtime-issued upload grant. */
export function createTransferHandle(
  transport: TrellisTransportProvider,
  auth: TrellisAuth,
  timeoutMs: number,
  grant: SendTransferGrant,
  inboxPrefix?: string,
): SendTransferHandle;
/** Create a helper for a runtime-issued download grant. */
export function createTransferHandle(
  transport: TrellisTransportProvider,
  auth: TrellisAuth,
  timeoutMs: number,
  grant: ReceiveTransferGrant,
  inboxPrefix?: string,
): ReceiveTransferHandle;
/** Create the direction-specific helper without moving an in-flight generation. */
export function createTransferHandle(
  transport: TrellisTransportProvider,
  auth: TrellisAuth,
  timeoutMs: number,
  grant: TransferGrant,
  inboxPrefix?: string,
): TransferHandle;
export function createTransferHandle(
  transport: TrellisTransportProvider,
  auth: TrellisAuth,
  timeoutMs: number,
  grant: TransferGrant,
  inboxPrefix = "_INBOX",
): TransferHandle {
  return grant.direction === "send"
    ? new SendTransferHandle(transport, auth, timeoutMs, grant, inboxPrefix)
    : new ReceiveTransferHandle(transport, auth, timeoutMs, grant, inboxPrefix);
}

function observe(
  direction: "upload" | "download",
  started: number,
  ok: boolean,
): void {
  recordCatalogDuration(
    "trellis.transfer.duration",
    performance.now() - started,
    { "trellis.direction": direction, "trellis.outcome": ok ? "ok" : "error" },
  );
}
