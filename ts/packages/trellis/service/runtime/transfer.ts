import {
  AsyncResult,
  isErr,
  Result,
  type Result as ResultType,
} from "@oatscenter/result";
import {
  headers as natsHeaders,
  type Msg,
  type NatsConnection,
  type Subscription,
} from "@nats-io/nats-core";
import { ulid } from "ulid";
import { sha256 } from "@noble/hashes/sha256";
import { base64urlEncode } from "../../auth/utils.ts";

import type { PermissionAtom } from "../../participant_runtime/api.ts";
import {
  type OperationTransferHandle,
  type RuntimeOperationTransferProgress,
  type TrellisAuth,
  verifyLocalAuthorization,
} from "../../session.ts";
import type { StoreError } from "../../errors/StoreError.ts";
import { TransferError } from "../../errors/TransferError.ts";
import { TypedStore, TypedStoreEntry } from "../../store.ts";
import type {
  FileInfo,
  ReceiveTransferGrant,
  SendTransferGrant,
} from "../../transfer.ts";
import { transferFrameProofPayload } from "../../transfer_protocol.ts";
import {
  AsyncChunkQueue,
  AsyncValueBroadcaster,
  deferred,
} from "./transfer/queue.ts";
import type { DownloadSession } from "./transfer/download.ts";
import {
  DEFAULT_TRANSFER_CHUNK_BYTES,
  DOWNLOAD_SUBJECT_PREFIX,
  fileInfoFromStoreInfo,
  parseSeq,
  publishError,
  replyError,
  TRANSFER_CONTROL_HEADER,
  TRANSFER_EOF_HEADER,
  TRANSFER_SEQUENCE_HEADER,
  UPLOAD_SUBJECT_PREFIX,
} from "./transfer/protocol.ts";
import {
  effectiveUploadMaxBytes,
  raceCancellation,
  type UploadSession,
} from "./transfer/upload.ts";
import { MAX_TRANSFER_CHUNK_BYTES } from "../../transfer.ts";

export type TransferStoreHandle = {
  open(): AsyncResult<TypedStore, StoreError>;
};

export type InitiateUploadArgs = {
  sessionKey: string;
  permission: PermissionAtom | undefined;
  requiredCapabilities: readonly string[];
  store: string;
  key: string;
  expiresInMs: number;
  maxBytes?: number;
  contentType?: string;
  metadata?: Record<string, string>;
  onProgress?: (
    progress: RuntimeOperationTransferProgress,
  ) => Promise<void> | void;
  onComplete?: (info: FileInfo) => Promise<void> | void;
  onError?: (error: TransferError) => Promise<void> | void;
  onStored?: (stored: StoredTransfer) => Promise<void> | void;
};

export type OperationUploadTransfer = {
  grant: SendTransferGrant;
  transfer: OperationTransferHandle;
};

export type InitiateDownloadArgs = {
  sessionKey: string;
  permission: PermissionAtom;
  requiredCapabilities?: readonly string[];
  inboxPrefix: string;
  store: string;
  key: string;
  expiresInMs: number;
};

type ServiceTransferOpts = {
  name: string;
  nc: NatsConnection;
  auth: TrellisAuth;
  stores: Record<string, TransferStoreHandle>;
  chunkBytes?: number;
};

export type StoredTransfer = {
  transferId: string;
  sessionKey: string;
  store: TypedStore;
  entry: TypedStoreEntry;
  info: FileInfo;
};

export class ServiceTransfer {
  readonly #name: string;
  readonly #nc: NatsConnection;
  readonly #auth: TrellisAuth;
  readonly #stores: Record<string, TransferStoreHandle>;
  readonly #chunkBytes: number;
  readonly #uploadSessions = new Map<string, UploadSession>();
  readonly #downloadSessions = new Map<string, DownloadSession>();

  constructor(opts: ServiceTransferOpts) {
    this.#name = opts.name;
    this.#nc = opts.nc;
    this.#auth = opts.auth;
    this.#stores = opts.stores;
    this.#chunkBytes = opts.chunkBytes ?? DEFAULT_TRANSFER_CHUNK_BYTES;
    if (
      !Number.isSafeInteger(this.#chunkBytes) || this.#chunkBytes < 1 ||
      this.#chunkBytes > MAX_TRANSFER_CHUNK_BYTES
    ) {
      throw new RangeError(
        `transfer chunk size must be between 1 and ${MAX_TRANSFER_CHUNK_BYTES} bytes`,
      );
    }
  }

  async initiateUpload(
    args: InitiateUploadArgs,
  ): Promise<ResultType<SendTransferGrant, TransferError>> {
    const store = await this.#openStore(args.store, "initiateUpload");
    const storeValue = store.take();
    if (isErr(storeValue)) {
      return Result.err(storeValue.error);
    }

    const storeStatus = await storeValue.status();
    const storeStatusValue = storeStatus.take();
    if (isErr(storeStatusValue)) {
      return Result.err(
        new TransferError({
          operation: "initiateUpload",
          cause: storeStatusValue.error,
        }),
      );
    }

    const maxBytes = effectiveUploadMaxBytes(
      args.maxBytes,
      storeStatusValue.maxObjectBytes,
    );

    const transferId = ulid();
    const subject = `${UPLOAD_SUBJECT_PREFIX}.${
      this.#auth.sessionKey.slice(0, 16)
    }.${transferId}`;
    const expiresAtMs = Date.now() + args.expiresInMs;
    const queue = new AsyncChunkQueue();
    const subscription = this.#nc.subscribe(subject);
    const putPromise = storeValue.put(args.key, queue, {
      ...(args.contentType ? { contentType: args.contentType } : {}),
      ...(args.metadata ? { metadata: args.metadata } : {}),
    });

    const session: UploadSession = {
      kind: "upload",
      subject,
      transferId,
      sessionKey: args.sessionKey,
      permission: args.permission,
      requiredCapabilities: args.requiredCapabilities,
      expiresAtMs,
      store: storeValue,
      key: args.key,
      ...(maxBytes !== undefined ? { maxBytes } : {}),
      ...(args.contentType ? { contentType: args.contentType } : {}),
      ...(args.metadata ? { metadata: args.metadata } : {}),
      ...(args.onProgress ? { onProgress: args.onProgress } : {}),
      ...(args.onComplete ? { onComplete: args.onComplete } : {}),
      ...(args.onError ? { onError: args.onError } : {}),
      ...(args.onStored ? { onStored: args.onStored } : {}),
      subscription,
      timeoutId: setTimeout(
        () => this.#expireUploadSession(subject),
        args.expiresInMs,
      ),
      queue,
      putPromise,
      cancellation: new AbortController(),
      committing: false,
      nextSeq: 0,
      receivedBytes: 0,
      hasher: sha256.create(),
    };

    this.#uploadSessions.set(subject, session);
    this.#runUploadSession(session);

    if (!this.#uploadSessions.has(subject)) {
      return Result.err(
        new TransferError({
          operation: "initiateUpload",
          context: { reason: "session_closed" },
        }),
      );
    }
    if (Date.now() >= expiresAtMs) {
      const error = new TransferError({
        operation: "initiateUpload",
        context: { reason: "expired" },
      });
      this.#expireUploadSession(subject, error);
      return Result.err(error);
    }

    return Result.ok({
      type: "TransferGrant",
      direction: "send",
      service: this.#name,
      sessionKey: args.sessionKey,
      transferId,
      subject,
      expiresAt: new Date(expiresAtMs).toISOString(),
      chunkBytes: this.#chunkBytes,
      ...(maxBytes !== undefined ? { maxBytes } : {}),
      ...(args.contentType ? { contentType: args.contentType } : {}),
      ...(args.metadata ? { metadata: args.metadata } : {}),
    });
  }

  createOperationUpload(
    args: InitiateUploadArgs,
  ): AsyncResult<OperationUploadTransfer, TransferError> {
    return AsyncResult.from(
      (async (): Promise<
        ResultType<OperationUploadTransfer, TransferError>
      > => {
        const updates = new AsyncValueBroadcaster<
          RuntimeOperationTransferProgress
        >();
        const completed = deferred<ResultType<FileInfo, TransferError>>();
        let settled = false;
        const settle = (value: ResultType<FileInfo, TransferError>) => {
          if (settled) {
            return;
          }
          settled = true;
          updates.close();
          completed.resolve(value);
        };

        const grant = await this.initiateUpload({
          ...args,
          onProgress: async (progress) => {
            updates.push(progress);
            await args.onProgress?.(progress);
          },
          onComplete: async (info) => {
            await args.onComplete?.(info);
            settle(Result.ok(info));
          },
          onError: async (error) => {
            await args.onError?.(error);
            settle(Result.err(error));
          },
          onStored: async (stored) => {
            await args.onStored?.(stored);
          },
        });
        const grantValue = grant.take();
        if (isErr(grantValue)) {
          return Result.err(grantValue.error);
        }

        return Result.ok({
          grant: grantValue,
          transfer: {
            updates: () => updates.subscribe(),
            completed: () => AsyncResult.from(completed.promise),
          },
        });
      })(),
    );
  }

  async initiateDownload(
    args: InitiateDownloadArgs,
  ): Promise<ResultType<ReceiveTransferGrant, TransferError>> {
    const store = await this.#openStore(args.store, "initiateDownload");
    const storeValue = store.take();
    if (isErr(storeValue)) {
      return Result.err(storeValue.error);
    }

    const entry = await storeValue.get(args.key);
    const entryValue = entry.take();
    if (isErr(entryValue)) {
      return Result.err(
        new TransferError({
          operation: "initiateDownload",
          cause: entryValue.error,
        }),
      );
    }
    const info = fileInfoFromStoreInfo(entryValue.info);
    if (info.digest === undefined) {
      return Result.err(
        new TransferError({
          operation: "initiateDownload",
          context: { reason: "missing_digest", key: args.key },
        }),
      );
    }
    const downloadInfo = { ...info, digest: info.digest };

    const transferId = ulid();
    const subject = `${DOWNLOAD_SUBJECT_PREFIX}.${
      this.#auth.sessionKey.slice(0, 16)
    }.${transferId}`;
    const expiresAtMs = Date.now() + args.expiresInMs;
    const subscription = this.#nc.subscribe(subject);
    const session: DownloadSession = {
      kind: "download",
      subject,
      transferId,
      sessionKey: args.sessionKey,
      permission: args.permission,
      requiredCapabilities: [...(args.requiredCapabilities ?? [])],
      inboxPrefix: args.inboxPrefix,
      expiresAtMs,
      store: storeValue,
      key: args.key,
      info: downloadInfo,
      subscription,
      pendingOffset: 0,
      nextSeq: 0,
      sentBytes: 0,
      hasher: sha256.create(),
      cancellation: new AbortController(),
      timeoutId: setTimeout(
        () => this.#cleanupDownloadSession(subject),
        args.expiresInMs,
      ),
    };

    this.#downloadSessions.set(subject, session);
    this.#runDownloadSession(session);

    if (!this.#downloadSessions.has(subject)) {
      return Result.err(
        new TransferError({
          operation: "initiateDownload",
          context: { reason: "session_closed" },
        }),
      );
    }
    if (Date.now() >= expiresAtMs) {
      this.#cleanupDownloadSession(subject);
      return Result.err(
        new TransferError({
          operation: "initiateDownload",
          context: { reason: "expired" },
        }),
      );
    }

    return Result.ok({
      type: "TransferGrant",
      direction: "receive",
      service: this.#name,
      sessionKey: args.sessionKey,
      transferId,
      subject,
      expiresAt: new Date(expiresAtMs).toISOString(),
      chunkBytes: this.#chunkBytes,
      info: downloadInfo,
    });
  }

  async stop(): Promise<void> {
    for (const subject of [...this.#uploadSessions.keys()]) {
      this.#expireUploadSession(subject);
    }
    for (const subject of [...this.#downloadSessions.keys()]) {
      this.#cleanupDownloadSession(subject);
    }
  }

  async #openStore(
    alias: string,
    operation: string,
  ): Promise<ResultType<TypedStore, TransferError>> {
    const handle = this.#stores[alias];
    if (!handle) {
      return Result.err(
        new TransferError({
          operation,
          context: { reason: "unknown_store", store: alias },
        }),
      );
    }

    const store = await handle.open();
    const value = store.take();
    if (isErr(value)) {
      return Result.err(
        new TransferError({
          operation,
          cause: value.error,
          context: { store: alias },
        }),
      );
    }
    return Result.ok(value);
  }

  async #runUploadSession(session: UploadSession): Promise<void> {
    let pending: Promise<void> | undefined;
    try {
      for await (const msg of session.subscription) {
        const cancellation = msg.headers?.get(TRANSFER_CONTROL_HEADER) ===
          "cancel";
        if (pending && !cancellation) {
          replyError(
            msg,
            new TransferError({
              operation: "put",
              context: { reason: "pending_frame" },
            }),
          );
          continue;
        }
        const handling = this.#handleUploadMessage(session, msg);
        if (pending) {
          await handling;
          await pending.catch(() => undefined);
          break;
        }
        let tracked: Promise<void>;
        tracked = handling.catch((cause) => {
          const error = cause instanceof TransferError
            ? cause
            : new TransferError({ operation: "put", cause });
          replyError(msg, error);
          this.#expireUploadSession(session.subject, error);
        }).finally(() => {
          if (pending === tracked) pending = undefined;
        });
        pending = tracked;
      }
    } finally {
      await pending?.catch(() => undefined);
      this.#cleanupUploadSession(session.subject);
    }
  }

  async #handleUploadMessage(session: UploadSession, msg: Msg): Promise<void> {
    if (Date.now() >= session.expiresAtMs) {
      const error = new TransferError({
        operation: "put",
        context: { reason: "expired" },
      });
      replyError(msg, error);
      this.#expireUploadSession(session.subject, error);
      return;
    }

    const seqResult = parseSeq(msg).take();
    if (isErr(seqResult)) {
      replyError(msg, seqResult.error);
      this.#expireUploadSession(session.subject, seqResult.error);
      return;
    }
    const control = msg.headers?.get(TRANSFER_CONTROL_HEADER) || undefined;
    const authenticated = await verifyLocalAuthorization({
      kind: "request",
      cache: this.#auth.authorizationProviderCache,
      message: msg,
      permission: session.permission,
      requiredCapabilities: session.requiredCapabilities,
      proofPayload: transferFrameProofPayload(seqResult, control, msg.data),
    });
    const authenticatedValue = authenticated.take();
    if (
      isErr(authenticatedValue) ||
      authenticatedValue.sessionKey !== session.sessionKey
    ) {
      const error = new TransferError({
        operation: "put",
        context: { reason: "invalid_proof" },
      });
      replyError(msg, error);
      this.#expireUploadSession(session.subject, error);
      return;
    }

    if (msg.headers?.get(TRANSFER_EOF_HEADER) === "true") {
      const error = new TransferError({
        operation: "put",
        context: { reason: "legacy_eof_header" },
      });
      replyError(msg, error);
      this.#expireUploadSession(session.subject, error);
      return;
    }
    if (control === "cancel") {
      try {
        if (
          (JSON.parse(new TextDecoder().decode(msg.data)) as {
            action?: string;
          })
            .action !== "cancel"
        ) {
          throw new Error(
            "cancellation payload does not match its control header",
          );
        }
        if (session.committing) {
          replyError(
            msg,
            new TransferError({
              operation: "put",
              context: { reason: "cancel_after_validated_eof" },
            }),
          );
          return;
        }
        this.#expireUploadSession(
          session.subject,
          new TransferError({
            operation: "put",
            context: { reason: "cancelled" },
          }),
        );
        msg.respond(JSON.stringify({ status: "cancelled" }));
        return;
      } catch (cause) {
        const error = new TransferError({
          operation: "put",
          cause,
          context: { reason: "invalid_control" },
        });
        replyError(msg, error);
        this.#expireUploadSession(session.subject, error);
        return;
      }
    }
    if (control !== undefined && control !== "complete") {
      const error = new TransferError({
        operation: "put",
        context: { reason: "invalid_control", control },
      });
      replyError(msg, error);
      this.#expireUploadSession(session.subject, error);
      return;
    }

    const seq = seqResult;
    if (seq !== session.nextSeq) {
      const error = new TransferError({
        operation: "put",
        context: {
          reason: "out_of_order",
          expected: session.nextSeq,
          actual: seq,
        },
      });
      replyError(msg, error);
      this.#expireUploadSession(session.subject, error);
      return;
    }
    const eof = control === "complete";
    if (!eof && msg.data.length > 0) {
      if (msg.data.length > this.#chunkBytes) {
        const error = new TransferError({
          operation: "put",
          context: {
            reason: "chunk_too_large",
            maxChunkBytes: this.#chunkBytes,
          },
        });
        replyError(msg, error);
        this.#expireUploadSession(session.subject, error);
        return;
      }
      session.receivedBytes += msg.data.length;
      if (
        session.maxBytes !== undefined &&
        session.receivedBytes > session.maxBytes
      ) {
        const error = new TransferError({
          operation: "put",
          context: {
            reason: "max_bytes_exceeded",
            maxBytes: session.maxBytes,
            attemptedBytes: session.receivedBytes,
          },
        });
        replyError(msg, error);
        this.#expireUploadSession(session.subject, error);
        return;
      }
      await raceCancellation(
        session.queue.push(msg.data),
        session.cancellation.signal,
      );
      session.hasher.update(msg.data);
      await session.onProgress?.({
        chunkIndex: session.nextSeq,
        chunkBytes: msg.data.length,
        transferredBytes: session.receivedBytes,
      });
    }
    session.nextSeq += 1;

    if (eof) {
      let completion: { action: "complete"; size: number; digest: string };
      try {
        completion = JSON.parse(new TextDecoder().decode(msg.data));
      } catch (cause) {
        const error = new TransferError({
          operation: "put",
          cause,
          context: { reason: "invalid_completion" },
        });
        replyError(msg, error);
        this.#expireUploadSession(session.subject, error);
        return;
      }
      const digest = `SHA-256=${base64urlEncode(session.hasher.digest())}`;
      if (
        completion.action !== "complete" ||
        completion.size !== session.receivedBytes ||
        completion.digest !== digest
      ) {
        const error = new TransferError({
          operation: "put",
          context: {
            reason: "completion_mismatch",
            expectedSize: session.receivedBytes,
            actualSize: completion.size,
            expectedDigest: digest,
            actualDigest: completion.digest,
          },
        });
        replyError(msg, error);
        this.#expireUploadSession(session.subject, error);
        return;
      }
      session.queue.close();
      session.committing = true;
      clearTimeout(session.timeoutId);
      const putResult = await session.putPromise;
      const putValue = putResult.take();
      if (isErr(putValue)) {
        const error = new TransferError({
          operation: "put",
          cause: putValue.error,
        });
        replyError(msg, error);
        this.#expireUploadSession(session.subject, error);
        return;
      }

      const stored = await session.store.get(session.key);
      const storedValue = stored.take();
      if (isErr(storedValue)) {
        const error = new TransferError({
          operation: "put",
          cause: storedValue.error,
        });
        replyError(msg, error);
        this.#expireUploadSession(session.subject, error);
        return;
      }

      const info = fileInfoFromStoreInfo(storedValue.info);
      if (
        info.key !== session.key || info.size !== session.receivedBytes ||
        info.digest === undefined ||
        info.digest.replace(/=+$/, "") !== digest.replace(/=+$/, "")
      ) {
        const error = new TransferError({
          operation: "put",
          context: {
            reason: "stored_metadata_mismatch",
            expectedKey: session.key,
            actualKey: info.key,
            expectedSize: session.receivedBytes,
            actualSize: info.size,
            expectedDigest: digest,
            actualDigest: info.digest,
          },
        });
        replyError(msg, error);
        this.#expireUploadSession(session.subject, error);
        return;
      }
      msg.respond(JSON.stringify({ status: "complete", info }));
      await session.onComplete?.(info);
      if (session.onStored) {
        void Promise.resolve(session.onStored({
          transferId: session.transferId,
          sessionKey: session.sessionKey,
          store: session.store,
          entry: storedValue,
          info,
        })).catch((error) => {
          console.error("transfer onStored callback failed", error);
        });
      }
      this.#cleanupUploadSession(session.subject);
      return;
    }

    msg.respond(JSON.stringify({ status: "continue" }));
  }

  async #runDownloadSession(session: DownloadSession): Promise<void> {
    let pending: Promise<boolean> | undefined;
    try {
      for await (const msg of session.subscription) {
        const cancellation = msg.headers?.get(TRANSFER_CONTROL_HEADER) ===
          "cancel";
        if (pending && !cancellation) {
          replyError(
            msg,
            new TransferError({
              operation: "get",
              context: { reason: "pending_frame" },
            }),
          );
          continue;
        }
        const handling = this.#handleDownloadRequest(session, msg);
        if (pending) {
          if (await handling) break;
          continue;
        }
        let tracked: Promise<boolean>;
        tracked = handling.then((done) => {
          if (done) this.#cleanupDownloadSession(session.subject);
          return done;
        }).catch(() => {
          this.#cleanupDownloadSession(session.subject);
          return true;
        }).finally(() => {
          if (pending === tracked) pending = undefined;
        });
        pending = tracked;
      }
    } finally {
      await pending?.catch(() => undefined);
      this.#cleanupDownloadSession(session.subject);
    }
  }

  async #handleDownloadRequest(
    session: DownloadSession,
    msg: Msg,
  ): Promise<boolean> {
    const reply = msg.reply;
    if (
      !reply || !reply.startsWith(`${session.inboxPrefix}.`)
    ) {
      replyError(
        msg,
        new TransferError({
          operation: "get",
          context: {
            reason: "reply_subject_mismatch",
            expected: session.inboxPrefix,
            actual: reply,
          },
        }),
      );
      return false;
    }
    if (Date.now() >= session.expiresAtMs) {
      publishError(
        this.#nc,
        reply,
        new TransferError({ operation: "get", context: { reason: "expired" } }),
      );
      return true;
    }

    const parsedSeq = parseSeq(msg).take();
    if (isErr(parsedSeq)) {
      replyError(msg, parsedSeq.error);
      return false;
    }
    const control = msg.headers?.get(TRANSFER_CONTROL_HEADER) || undefined;
    const authenticated = await verifyLocalAuthorization({
      kind: "request",
      cache: this.#auth.authorizationProviderCache,
      message: msg,
      permission: session.permission,
      requiredCapabilities: session.requiredCapabilities,
      proofPayload: transferFrameProofPayload(parsedSeq, control, msg.data),
    });
    const authenticatedValue = authenticated.take();
    if (
      isErr(authenticatedValue) ||
      authenticatedValue.sessionKey !== session.sessionKey
    ) {
      publishError(
        this.#nc,
        reply,
        new TransferError({
          operation: "get",
          context: { reason: "invalid_proof" },
        }),
      );
      return false;
    }

    if (control === "cancel") {
      try {
        if (
          (JSON.parse(new TextDecoder().decode(msg.data)) as {
            action?: string;
          })
            .action !== "cancel"
        ) {
          throw new Error(
            "cancellation payload does not match its control header",
          );
        }
        session.cancellation.abort(
          new TransferError({
            operation: "get",
            context: { reason: "cancelled" },
          }),
        );
        await session.reader?.cancel().catch(() => undefined);
        msg.respond(JSON.stringify({ status: "cancelled" }));
        return true;
      } catch (cause) {
        replyError(
          msg,
          new TransferError({
            operation: "get",
            cause,
            context: { reason: "invalid_control" },
          }),
        );
        return false;
      }
    }
    if (
      control !== undefined ||
      msg.headers?.get(TRANSFER_EOF_HEADER) === "true"
    ) {
      replyError(
        msg,
        new TransferError({
          operation: "get",
          context: { reason: "invalid_control", control },
        }),
      );
      return false;
    }

    if (parsedSeq !== session.nextSeq) {
      replyError(
        msg,
        new TransferError({
          operation: "get",
          context: {
            reason: "sequence",
            expected: session.nextSeq,
            actual: parsedSeq,
          },
        }),
      );
      return false;
    }
    if (msg.data.length !== 0) {
      replyError(
        msg,
        new TransferError({
          operation: "get",
          context: { reason: "invalid_control" },
        }),
      );
      return false;
    }

    try {
      if (!session.reader) {
        const entryValue = (await session.store.get(session.key)).take();
        if (isErr(entryValue)) throw entryValue.error;
        const streamValue = (await entryValue.stream()).take();
        if (isErr(streamValue)) throw streamValue.error;
        session.reader = streamValue.getReader();
      }
      while (
        !session.pending || session.pendingOffset >= session.pending.length
      ) {
        const next = await raceCancellation(
          session.reader.read(),
          session.cancellation.signal,
        );
        if (next.done) {
          if (session.sentBytes !== session.info.size) {
            throw new TransferError({
              operation: "get",
              context: {
                reason: "size_mismatch",
                expected: session.info.size,
                actual: session.sentBytes,
              },
            });
          }
          const digest = `SHA-256=${base64urlEncode(session.hasher.digest())}`;
          if (
            session.info.digest &&
            session.info.digest.replace(/=+$/, "") !== digest.replace(/=+$/, "")
          ) {
            throw new TransferError({
              operation: "get",
              context: {
                reason: "digest_mismatch",
                expected: session.info.digest,
                actual: digest,
              },
            });
          }
          const headers = natsHeaders();
          headers.set(TRANSFER_SEQUENCE_HEADER, String(session.nextSeq));
          headers.set(TRANSFER_EOF_HEADER, "true");
          msg.respond(new Uint8Array(), { headers });
          return true;
        }
        session.pending = next.value;
        session.pendingOffset = 0;
      }

      const chunk = session.pending.slice(
        session.pendingOffset,
        session.pendingOffset + this.#chunkBytes,
      );
      session.pendingOffset += chunk.length;
      session.sentBytes += chunk.length;
      if (session.sentBytes > session.info.size) {
        throw new TransferError({
          operation: "get",
          context: {
            reason: "size_mismatch",
            expected: session.info.size,
            actual: session.sentBytes,
          },
        });
      }
      session.hasher.update(chunk);
      const headers = natsHeaders();
      headers.set(TRANSFER_SEQUENCE_HEADER, String(session.nextSeq));
      msg.respond(chunk, { headers });
      session.nextSeq += 1;
      return false;
    } catch (cause) {
      replyError(
        msg,
        cause instanceof TransferError
          ? cause
          : new TransferError({ operation: "get", cause }),
      );
      return true;
    }
  }

  #expireUploadSession(
    subject: string,
    error = new TransferError({
      operation: "put",
      context: { reason: "expired" },
    }),
  ): void {
    const session = this.#uploadSessions.get(subject);
    if (!session) {
      return;
    }
    session.cancellation.abort(error);
    session.queue.fail(error);
    void session.onError?.(error);
    void session.putPromise;
    this.#cleanupUploadSession(subject);
  }

  #cleanupUploadSession(subject: string): void {
    const session = this.#uploadSessions.get(subject);
    if (!session) {
      return;
    }
    clearTimeout(session.timeoutId);
    session.cancellation.abort();
    session.subscription.unsubscribe();
    this.#uploadSessions.delete(subject);
  }

  #cleanupDownloadSession(subject: string): void {
    const session = this.#downloadSessions.get(subject);
    if (!session) {
      return;
    }
    clearTimeout(session.timeoutId);
    session.cancellation.abort();
    session.subscription.unsubscribe();
    void session.reader?.cancel().catch(() => undefined);
    this.#downloadSessions.delete(subject);
  }
}
