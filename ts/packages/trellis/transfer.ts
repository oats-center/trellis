import {
  AsyncResult,
  isErr,
  Result,
  type Result as ResultType,
} from "@qlever-llc/result";
import {
  createInbox,
  headers as natsHeaders,
  type Msg,
  type MsgHdrs,
  type NatsConnection,
} from "@nats-io/nats-core";
import Type, { type Static } from "typebox";
import { ulid } from "ulid";
import { sha256 as incrementalSha256 } from "@noble/hashes/sha256";
import { buildProofInput } from "./auth/proof.ts";
import { base64urlEncode, sha256 } from "./auth/utils.ts";
import { TransferError } from "./errors/TransferError.ts";
import {
  createNatsHeaderCarrier,
  injectTraceContext,
  recordTrellisError,
} from "./telemetry/mod.ts";
import { transferFrameProofPayload } from "./transfer_protocol.ts";

const TRANSFER_SEQUENCE_HEADER = "trellis-transfer-seq";
const TRANSFER_EOF_HEADER = "trellis-transfer-eof";
const TRANSFER_CONTROL_HEADER = "trellis-transfer-control";
export const MAX_TRANSFER_CHUNK_BYTES = 1024 * 1024;

export const FileInfoSchema = Type.Object({
  key: Type.String({ minLength: 1 }),
  size: Type.Integer({ minimum: 0 }),
  updatedAt: Type.String({ minLength: 1 }),
  digest: Type.Optional(Type.String({ minLength: 1 })),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  metadata: Type.Record(Type.String({ minLength: 1 }), Type.String()),
});

export type FileInfo = Static<typeof FileInfoSchema>;

const TransferGrantBaseSchema = Type.Object({
  type: Type.Literal("TransferGrant"),
  service: Type.String({ minLength: 1 }),
  sessionKey: Type.String({ minLength: 1 }),
  transferId: Type.String({ minLength: 1 }),
  subject: Type.String({ minLength: 1 }),
  expiresAt: Type.String({ minLength: 1 }),
  chunkBytes: Type.Integer({ minimum: 1, maximum: MAX_TRANSFER_CHUNK_BYTES }),
});

export const SendTransferGrantSchema = Type.Object({
  ...TransferGrantBaseSchema.properties,
  direction: Type.Literal("send"),
  maxBytes: Type.Optional(Type.Integer({ minimum: 1 })),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  metadata: Type.Optional(
    Type.Record(Type.String({ minLength: 1 }), Type.String()),
  ),
});

export const ReceiveTransferGrantSchema = Type.Object({
  ...TransferGrantBaseSchema.properties,
  direction: Type.Literal("receive"),
  info: Type.Object({
    ...FileInfoSchema.properties,
    digest: Type.String({ minLength: 1 }),
  }),
});

export const TransferGrantSchema = Type.Union([
  SendTransferGrantSchema,
  ReceiveTransferGrantSchema,
]);

export type SendTransferGrant = Static<typeof SendTransferGrantSchema>;
export type ReceiveTransferGrant = Static<typeof ReceiveTransferGrantSchema>;
export type TransferGrant = Static<typeof TransferGrantSchema>;

export type TransferBody =
  | Uint8Array
  | ArrayBuffer
  | ReadableStream<Uint8Array>
  | AsyncIterable<Uint8Array>;

type TrellisTransferAuth = {
  sessionKey: string;
  sign(data: Uint8Array): Promise<Uint8Array> | Uint8Array;
  currentIat?: () => number;
  contextDigest?: string | (() => string);
};

type TransferAck =
  | { status: "continue" }
  | { status: "complete"; info: FileInfo }
  | { status: "cancelled" };

async function createTransferProof(
  auth: TrellisTransferAuth,
  subject: string,
  reply: string,
  payload: Uint8Array,
): Promise<{
  proof: string;
  iat: number;
  requestId: string;
  contextDigest: string;
}> {
  const contextDigest = typeof auth.contextDigest === "function"
    ? auth.contextDigest()
    : auth.contextDigest;
  if (contextDigest === undefined) {
    throw new Error("contextDigest is required to sign transfer proofs");
  }
  if (reply.length === 0) {
    throw new Error("transfer reply subject must not be empty");
  }
  const payloadHash = await sha256(payload);
  const iat = auth.currentIat?.() ?? Math.floor(Date.now() / 1000);
  const requestId = ulid();
  const proofOk = await auth.sign(
    await sha256(
      buildProofInput(
        contextDigest,
        subject,
        reply,
        payloadHash,
        iat,
        requestId,
      ),
    ),
  );
  return {
    proof: base64urlEncode(proofOk),
    iat,
    requestId,
    contextDigest,
  };
}

function expired(expiresAt: string): boolean {
  return Date.now() >= Date.parse(expiresAt);
}

function asUint8Array(body: Uint8Array | ArrayBuffer): Uint8Array {
  return body instanceof Uint8Array ? body : new Uint8Array(body);
}

function streamFromAsyncIterable(
  iterable: AsyncIterable<Uint8Array>,
): ReadableStream<Uint8Array> {
  const iterator = iterable[Symbol.asyncIterator]();
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      const next = await iterator.next();
      if (next.done) {
        controller.close();
        return;
      }
      controller.enqueue(next.value);
    },
    async cancel(reason) {
      await iterator.return?.(reason);
    },
  });
}

function streamFromBody(body: TransferBody): ReadableStream<Uint8Array> {
  if (body instanceof Uint8Array || body instanceof ArrayBuffer) {
    const bytes = asUint8Array(body);
    return new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(bytes);
        controller.close();
      },
    });
  }
  return body instanceof ReadableStream ? body : streamFromAsyncIterable(body);
}

async function* chunkBody(
  body: TransferBody,
  chunkBytes: number,
): AsyncIterable<Uint8Array> {
  const reader = streamFromBody(body).getReader();
  let completed = false;
  try {
    while (true) {
      const next = await reader.read();
      if (next.done) {
        completed = true;
        return;
      }

      let offset = 0;
      while (offset < next.value.length) {
        const end = Math.min(offset + chunkBytes, next.value.length);
        yield next.value.slice(offset, end);
        offset = end;
      }
    }
  } finally {
    if (!completed) void reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

function parseTransferAck(
  msg: Msg,
  operation: string,
): ResultType<TransferAck, TransferError> {
  if (msg.headers?.get("status") === "error") {
    return Result.err(deserializeTransferError(msg, operation));
  }

  try {
    const value = JSON.parse(msg.string()) as TransferAck;
    return Result.ok(value);
  } catch (cause) {
    return Result.err(new TransferError({ operation, cause }));
  }
}

function deserializeTransferError(msg: Msg, operation: string): TransferError {
  try {
    const value = JSON.parse(msg.string()) as {
      message?: string;
      context?: Record<string, unknown>;
    };
    return new TransferError({
      operation,
      context: value.context,
      cause: typeof value.context?.causeMessage === "string"
        ? new Error(value.context.causeMessage)
        : typeof value.context?.reason === "string"
        ? new Error(
          `${value.message ?? "Transfer failed"}: ${
            JSON.stringify(value.context)
          }`,
        )
        : value.message
        ? new Error(value.message)
        : undefined,
    });
  } catch (cause) {
    return new TransferError({ operation, cause });
  }
}

function recordTransferError(
  error: TransferError,
  direction: "receive" | "send",
  phase: string,
): TransferError {
  recordTrellisError(error, {
    surface: "transfer",
    direction,
    operation: error.operation ?? direction,
    phase,
    messagingSystem: "nats",
  });
  return error;
}

async function requestTransfer(
  nc: NatsConnection,
  subject: string,
  payload: Uint8Array,
  headers: MsgHdrs,
  reply: string,
  timeoutMs: number,
): Promise<Msg> {
  const subscription = nc.subscribe(reply, { max: 1 });
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    nc.publish(subject, payload, { headers, reply });
    return await Promise.race([
      subscription[Symbol.asyncIterator]().next().then((result) => {
        if (result.done) throw new Error("Transfer reply subscription closed");
        return result.value;
      }),
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("Transfer request timed out")),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
    subscription.unsubscribe();
  }
}

function receiveStream(
  grant: ReceiveTransferGrant,
  requestFrame: (seq: number) => Promise<Msg>,
  cancelTransfer: () => Promise<void>,
): ReadableStream<Uint8Array> {
  const hasher = incrementalSha256.create();
  let expectedSeq = 0;
  let receivedBytes = 0;

  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const msg = await requestFrame(expectedSeq);
        if (msg.headers?.get("status") === "error") {
          throw deserializeTransferError(msg, "stream");
        }

        const actualSeq = Number(msg.headers?.get(TRANSFER_SEQUENCE_HEADER));
        if (!Number.isSafeInteger(actualSeq) || actualSeq !== expectedSeq) {
          throw new TransferError({
            operation: "stream",
            context: { reason: "sequence", expectedSeq, actualSeq },
          });
        }
        expectedSeq += 1;

        if (msg.data.length > grant.chunkBytes) {
          throw new TransferError({
            operation: "stream",
            context: {
              reason: "chunk_too_large",
              maxChunkBytes: grant.chunkBytes,
              actualChunkBytes: msg.data.length,
            },
          });
        }
        const eof = msg.headers?.get(TRANSFER_EOF_HEADER) === "true";
        if (eof && msg.data.length !== 0) {
          throw new TransferError({
            operation: "stream",
            context: {
              reason: "nonempty_eof",
              actualChunkBytes: msg.data.length,
            },
          });
        }

        if (!eof && msg.data.length > 0) {
          receivedBytes += msg.data.length;
          if (receivedBytes > grant.info.size) {
            throw new TransferError({
              operation: "stream",
              context: {
                reason: "size_mismatch",
                expectedBytes: grant.info.size,
                actualBytes: receivedBytes,
              },
            });
          }
          hasher.update(msg.data);
          controller.enqueue(msg.data);
        }

        if (eof) {
          if (receivedBytes !== grant.info.size) {
            throw new TransferError({
              operation: "stream",
              context: {
                reason: "size_mismatch",
                expectedBytes: grant.info.size,
                actualBytes: receivedBytes,
              },
            });
          }
          const digest = `SHA-256=${base64urlEncode(hasher.digest())}`;
          if (
            grant.info.digest.replace(/=+$/, "") !== digest.replace(/=+$/, "")
          ) {
            throw new TransferError({
              operation: "stream",
              context: {
                reason: "digest_mismatch",
                expectedDigest: grant.info.digest,
                actualDigest: digest,
              },
            });
          }
          controller.close();
        }
      } catch (cause) {
        await cancelTransfer().catch(() => undefined);
        const error = cause instanceof TransferError
          ? cause
          : new TransferError({ operation: "stream", cause });
        controller.error(recordTransferError(error, "receive", "stream"));
      }
    },
    async cancel() {
      await cancelTransfer();
    },
  });
}

async function collectStream(
  stream: ReadableStream<Uint8Array>,
): Promise<ResultType<Uint8Array, TransferError>> {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;

  try {
    while (true) {
      const next = await reader.read();
      if (next.done) {
        const merged = new Uint8Array(total);
        let offset = 0;
        for (const chunk of chunks) {
          merged.set(chunk, offset);
          offset += chunk.length;
        }
        return Result.ok(merged);
      }
      chunks.push(next.value);
      total += next.value.length;
    }
  } catch (cause) {
    return Result.err(
      cause instanceof TransferError
        ? cause
        : new TransferError({ operation: "bytes", cause }),
    );
  } finally {
    reader.releaseLock();
  }
}

class BaseTransferHandle {
  readonly #nc: NatsConnection;
  readonly #auth: TrellisTransferAuth;
  readonly #timeoutMs: number;
  readonly #inboxPrefix: string;

  protected constructor(
    nc: NatsConnection,
    auth: TrellisTransferAuth,
    timeoutMs: number,
    inboxPrefix = "_INBOX",
  ) {
    this.#nc = nc;
    this.#auth = auth;
    this.#timeoutMs = timeoutMs;
    this.#inboxPrefix = inboxPrefix;
  }

  protected get nc(): NatsConnection {
    return this.#nc;
  }

  protected get inboxPrefix(): string {
    return this.#inboxPrefix;
  }

  protected get auth(): TrellisTransferAuth {
    return this.#auth;
  }

  protected get timeoutMs(): number {
    return this.#timeoutMs;
  }

  protected validateGrant(
    grant: TransferGrant,
    operation: string,
  ): ResultType<void, TransferError> {
    if (expired(grant.expiresAt)) {
      return Result.err(
        new TransferError({
          operation,
          context: { reason: "expired", transferId: grant.transferId },
        }),
      );
    }
    if (grant.sessionKey !== this.#auth.sessionKey) {
      return Result.err(
        new TransferError({
          operation,
          context: {
            reason: "session_mismatch",
            expectedSessionKey: grant.sessionKey,
            actualSessionKey: this.#auth.sessionKey,
          },
        }),
      );
    }
    return Result.ok(undefined);
  }

  protected async buildHeaders(
    subject: string,
    reply: string,
    payload: Uint8Array,
    seq?: number,
    control?: "complete" | "cancel",
  ): Promise<MsgHdrs> {
    const headers = natsHeaders();
    const authHeaders = await createTransferProof(
      this.#auth,
      subject,
      reply,
      transferFrameProofPayload(seq ?? 0, control, payload),
    );
    headers.set("authorization-context", authHeaders.contextDigest);
    headers.set("session-key", this.#auth.sessionKey);
    headers.set("proof", authHeaders.proof);
    headers.set("iat", String(authHeaders.iat));
    headers.set("request-id", authHeaders.requestId);
    if (seq !== undefined) {
      headers.set(TRANSFER_SEQUENCE_HEADER, String(seq));
    }
    if (control !== undefined) {
      headers.set(TRANSFER_CONTROL_HEADER, control);
    }
    injectTraceContext(createNatsHeaderCarrier(headers));
    return headers;
  }

  protected async cancelTransfer(subject: string): Promise<void> {
    const payload = new TextEncoder().encode(
      JSON.stringify({ action: "cancel" }),
    );
    const reply = createInbox(this.inboxPrefix);
    const headers = await this.buildHeaders(
      subject,
      reply,
      payload,
      0,
      "cancel",
    );
    const response = await requestTransfer(
      this.nc,
      subject,
      payload,
      headers,
      reply,
      this.timeoutMs,
    );
    const ack = parseTransferAck(response, "cancel").take();
    if (isErr(ack) || ack.status !== "cancelled") {
      throw isErr(ack) ? ack.error : new TransferError({
        operation: "cancel",
        context: { reason: "not_acknowledged" },
      });
    }
  }
}

export class SendTransferHandle extends BaseTransferHandle {
  readonly #grant: SendTransferGrant;

  constructor(
    nc: NatsConnection,
    auth: TrellisTransferAuth,
    timeoutMs: number,
    grant: SendTransferGrant,
    inboxPrefix = "_INBOX",
  ) {
    super(nc, auth, timeoutMs, inboxPrefix);
    this.#grant = grant;
  }

  send(body: TransferBody): AsyncResult<FileInfo, TransferError> {
    return AsyncResult.from(
      (async (): Promise<ResultType<FileInfo, TransferError>> => {
        const valid = this.validateGrant(this.#grant, "send").take();
        if (isErr(valid)) {
          return Result.err(recordTransferError(valid.error, "send", "grant"));
        }

        let sentBytes = 0;
        let seq = 0;
        const hasher = incrementalSha256.create();
        const abort = () =>
          this.cancelTransfer(this.#grant.subject).catch(() => {});

        try {
          for await (const chunk of chunkBody(body, this.#grant.chunkBytes)) {
            sentBytes += chunk.length;
            if (
              this.#grant.maxBytes !== undefined &&
              sentBytes > this.#grant.maxBytes
            ) {
              await abort();
              return Result.err(
                recordTransferError(
                  new TransferError({
                    operation: "send",
                    context: {
                      reason: "max_bytes_exceeded",
                      maxBytes: this.#grant.maxBytes,
                      attemptedBytes: sentBytes,
                    },
                  }),
                  "send",
                  "validation",
                ),
              );
            }

            const reply = createInbox(this.inboxPrefix);
            const headers = await this.buildHeaders(
              this.#grant.subject,
              reply,
              chunk,
              seq,
              undefined,
            );
            const response = await AsyncResult.try(() =>
              requestTransfer(
                this.nc,
                this.#grant.subject,
                chunk,
                headers,
                reply,
                this.timeoutMs,
              )
            ).take();
            if (isErr(response)) {
              await abort();
              return Result.err(
                recordTransferError(
                  new TransferError({
                    operation: "send",
                    cause: response.error,
                  }),
                  "send",
                  "send",
                ),
              );
            }

            const ack = parseTransferAck(response, "send").take();
            if (isErr(ack)) {
              await abort();
              return Result.err(recordTransferError(ack.error, "send", "ack"));
            }
            if (ack.status === "complete") {
              await abort();
              return Result.err(
                recordTransferError(
                  new TransferError({
                    operation: "send",
                    context: { reason: "premature_completion" },
                  }),
                  "send",
                  "ack",
                ),
              );
            }
            hasher.update(chunk);
            seq += 1;
          }
        } catch (cause) {
          await abort();
          return Result.err(
            recordTransferError(
              new TransferError({ operation: "send", cause }),
              "send",
              "source",
            ),
          );
        }

        const sentDigest = `SHA-256=${base64urlEncode(hasher.digest())}`;
        const completion = new TextEncoder().encode(JSON.stringify({
          action: "complete",
          size: sentBytes,
          digest: sentDigest,
        }));
        const reply = createInbox(this.inboxPrefix);
        const finalHeaders = await this.buildHeaders(
          this.#grant.subject,
          reply,
          completion,
          seq,
          "complete",
        );
        const finalResponse = await AsyncResult.try(() =>
          requestTransfer(
            this.nc,
            this.#grant.subject,
            completion,
            finalHeaders,
            reply,
            this.timeoutMs,
          )
        ).take();
        if (isErr(finalResponse)) {
          return Result.err(
            recordTransferError(
              new TransferError({
                operation: "send",
                cause: finalResponse.error,
              }),
              "send",
              "send",
            ),
          );
        }

        const finalAck = parseTransferAck(finalResponse, "send").take();
        if (isErr(finalAck)) {
          return Result.err(recordTransferError(finalAck.error, "send", "ack"));
        }
        if (finalAck.status !== "complete") {
          return Result.err(
            recordTransferError(
              new TransferError({
                operation: "send",
                context: { reason: "missing_completion" },
              }),
              "send",
              "ack",
            ),
          );
        }
        if (
          finalAck.info.size !== sentBytes ||
          finalAck.info.digest?.replace(/=+$/, "") !==
            sentDigest.replace(/=+$/, "")
        ) {
          return Result.err(
            recordTransferError(
              new TransferError({
                operation: "send",
                context: {
                  reason: "result_metadata_mismatch",
                  expectedSize: sentBytes,
                  actualSize: finalAck.info.size,
                  expectedDigest: sentDigest,
                  actualDigest: finalAck.info.digest,
                },
              }),
              "send",
              "ack",
            ),
          );
        }
        return Result.ok(finalAck.info);
      })(),
    );
  }
}

export class ReceiveTransferHandle extends BaseTransferHandle {
  readonly #grant: ReceiveTransferGrant;

  constructor(
    nc: NatsConnection,
    auth: TrellisTransferAuth,
    timeoutMs: number,
    grant: ReceiveTransferGrant,
    inboxPrefix = "_INBOX",
  ) {
    super(nc, auth, timeoutMs, inboxPrefix);
    this.#grant = grant;
  }

  stream(): AsyncResult<ReadableStream<Uint8Array>, TransferError> {
    return AsyncResult.from(
      (async (): Promise<
        ResultType<ReadableStream<Uint8Array>, TransferError>
      > => {
        const valid = this.validateGrant(this.#grant, "stream").take();
        if (isErr(valid)) {
          return Result.err(
            recordTransferError(valid.error, "receive", "grant"),
          );
        }

        return Result.ok(receiveStream(
          this.#grant,
          async (seq) => {
            const payload = new Uint8Array();
            const reply = createInbox(this.inboxPrefix);
            const headers = await this.buildHeaders(
              this.#grant.subject,
              reply,
              payload,
              seq,
              undefined,
            );
            return await requestTransfer(
              this.nc,
              this.#grant.subject,
              payload,
              headers,
              reply,
              this.timeoutMs,
            );
          },
          () => this.cancelTransfer(this.#grant.subject),
        ));
      })(),
    );
  }

  bytes(): AsyncResult<Uint8Array, TransferError> {
    return AsyncResult.from(
      (async (): Promise<ResultType<Uint8Array, TransferError>> => {
        const streamResult = await this.stream().take();
        if (isErr(streamResult)) {
          return Result.err(streamResult.error);
        }
        return await collectStream(streamResult);
      })(),
    );
  }
}

export type TransferHandle = SendTransferHandle | ReceiveTransferHandle;

export function createTransferHandle(
  nc: NatsConnection,
  auth: TrellisTransferAuth,
  timeoutMs: number,
  grant: SendTransferGrant,
  inboxPrefix?: string,
): SendTransferHandle;
export function createTransferHandle(
  nc: NatsConnection,
  auth: TrellisTransferAuth,
  timeoutMs: number,
  grant: ReceiveTransferGrant,
  inboxPrefix?: string,
): ReceiveTransferHandle;
export function createTransferHandle(
  nc: NatsConnection,
  auth: TrellisTransferAuth,
  timeoutMs: number,
  grant: TransferGrant,
  inboxPrefix?: string,
): TransferHandle;
export function createTransferHandle(
  nc: NatsConnection,
  auth: TrellisTransferAuth,
  timeoutMs: number,
  grant: TransferGrant,
  inboxPrefix = "_INBOX",
): TransferHandle {
  return grant.direction === "send"
    ? new SendTransferHandle(nc, auth, timeoutMs, grant, inboxPrefix)
    : new ReceiveTransferHandle(nc, auth, timeoutMs, grant, inboxPrefix);
}
