import {
  AsyncResult,
  isErr,
  Result,
  type Result as ResultType,
} from "@oatscenter/result";
import type { Msg } from "@nats-io/nats-core";
import type { TransferFrameDescriptor } from "../../auth/protocol_wasm.ts";
import type { LoggerLike } from "../../globals.ts";
import { sha256 } from "@noble/hashes/sha256";
import { base64urlEncode } from "../../auth/utils.ts";
import type { TrellisTransportProvider } from "../../transport/generations.ts";
import type { PermissionAtom } from "../../participant_runtime/api.ts";
import {
  type OperationTransferHandle,
  type RuntimeOperationTransferProgress,
  toVerifierPermission,
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
  TransferGrant,
} from "../../transfer.ts";
import {
  transferConstants,
  transferGenerateId,
  transferParseComplete,
  transferParseControl,
  transferParseCounter,
  transferParseGrant,
  type TransferSignalWire as TransferSignal,
  transferSubject,
} from "../../auth/protocol_wasm.ts";
import {
  TransferCredit,
  TransferSession,
  transferWait,
} from "../../transfer/session.ts";
import { SenderWindow } from "../../data_plane/flow.ts";
import { LiveAuthorityGuard } from "../../live/authority.ts";
import {
  recordCatalogCounter,
  recordCatalogDuration,
} from "../../telemetry/mod.ts";
import { recordCatalogUpDown } from "../../telemetry/metrics.ts";
import {
  AsyncValueBroadcaster,
  deferred,
  TransferIngress,
} from "./transfer/queue.ts";
import { fileInfoFromStoreInfo } from "./transfer/protocol.ts";

/** Resolved runtime store boundary; physical storage remains behind TypedStore. */
export type TransferStoreHandle = {
  open(): AsyncResult<TypedStore, StoreError>;
};
/** Authoritative caller and Operation lifecycle supplied by the owning runtime. */
export type InitiateUploadArgs = {
  sessionKey: string;
  connectionId: string;
  contextDigest: string;
  permission: PermissionAtom | undefined;
  requiredCapabilities: readonly string[];
  operationId: string;
  expiresInMs: number;
  maxBytes?: number;
  contentType?: string;
  metadata?: Record<string, string>;
  onProgress?: (
    progress: RuntimeOperationTransferProgress,
  ) => Promise<void> | void;
  /** Durable Operation final-progress/fence/state barrier, before client success. */
  commit?: (
    stored: StoredTransfer,
    progress: RuntimeOperationTransferProgress,
  ) => Promise<void>;
  onComplete?: (info: FileInfo) => Promise<void> | void;
  /** Reconcile durable ownership before cleanup; unknown state retains bytes. */
  reconcileCommit?: (
    storageKey: string,
    transferId: string,
  ) => Promise<boolean | undefined>;
  onError?: (error: TransferError) => Promise<void> | void;
  onStored?: (stored: StoredTransfer) => Promise<void> | void;
};
/** Runtime-issued grant plus the provider's durable staged-upload handle. */
export type OperationUploadTransfer = {
  grant: SendTransferGrant;
  transfer: OperationTransferHandle;
};
/** Admitted caller and logical source selected by a download RPC. */
export type InitiateDownloadArgs = {
  sessionKey: string;
  connectionId: string;
  contextDigest: string;
  permission: PermissionAtom;
  requiredCapabilities?: readonly string[];
  inboxPrefix: string;
  store: string;
  key: string;
  expiresInMs: number;
};
type ServiceTransferOpts = {
  log?: LoggerLike;
  name: string;
  connectionId: string;
  transport: TrellisTransportProvider;
  auth: TrellisAuth;
  stores: Record<string, TransferStoreHandle>;
  operationStagingBucket?: string;
};
/** Backend-neutral staged object available only after the required commit. */
export type StoredTransfer = {
  transferId: string;
  sessionKey: string;
  store: TypedStore;
  entry: TypedStoreEntry;
  /** Private attempt-specific locator; never an authored/public FileInfo key. */
  storageKey: string;
  info: FileInfo;
};

type ProviderSession = {
  wire: TransferSession;
  startedAt: number;
  permission: PermissionAtom | undefined;
  requiredCapabilities: readonly string[];
  callerGuard: LiveAuthorityGuard;
  signalLane: Promise<void>;
  terminalSent: boolean;
  controlSeq: bigint;
  lastControl: string;
  active: boolean;
  maxFrameBytes: number;
  received: bigint;
  consumed: bigint;
  consumedBytes: number;
  bytes: number;
  hash: ReturnType<typeof sha256.create>;
  window?: SenderWindow;
  credit?: TransferCredit;
  ingress?: TransferIngress;
  backend?: Promise<void>;
  download?: Promise<void>;
  barrier?: Promise<void>;
  reader?: ReadableStreamDefaultReader<Uint8Array>;
  upload?: InitiateUploadArgs;
  store: TypedStore;
  storageKey: string;
  logicalKey: string;
  committed: boolean;
  cancelling: boolean;
  completing: boolean;
  progress: RuntimeOperationTransferProgress;
  progressTask?: Promise<void>;
  progressPending: boolean;
  settlement: Promise<void>;
  settled: ReturnType<typeof deferred<void>>;
};

type SignalBody<S = TransferSignal> = S extends TransferSignal
  ? Omit<S, "format" | "transferId" | "signalSeq">
  : never;

/** Storage-neutral, authenticated one-way transfer provider. */
export class ServiceTransfer {
  readonly #sessions = new Map<string, ProviderSession>();
  readonly #owned = new Set<ProviderSession>();
  constructor(readonly opts: ServiceTransferOpts) {}

  async #staging(): Promise<TypedStore> {
    if (!this.opts.operationStagingBucket) {
      throw new Error("operation staging unavailable");
    }
    const store = (await TypedStore.open(
      this.opts.transport,
      this.opts.operationStagingBucket,
      { bindOnly: true },
    )).take();
    if (isErr(store)) throw store.error;
    return store;
  }

  async #prepare(
    grant: TransferGrant,
    args: {
      permission: PermissionAtom | undefined;
      requiredCapabilities: readonly string[];
      contextDigest: string;
    },
    store: TypedStore,
    storageKey: string,
    logicalKey: string,
  ): Promise<ProviderSession> {
    transferParseGrant(new TextEncoder().encode(JSON.stringify(grant)));
    const cache = this.opts.auth.authorizationProviderCache;
    if (!cache) throw new Error("transfer authorization cache unavailable");
    const callerGuard = await LiveAuthorityGuard.retain(
      cache,
      args.contextDigest,
      args.permission
        ? {
          kind: "observer",
          permission: toVerifierPermission(args.permission),
        }
        : { kind: "local-provider" },
    );
    if (
      callerGuard.identity.connectionId !== grant.consumer.connectionId ||
      callerGuard.identity.sessionKey !== grant.consumer.sessionKey
    ) {
      callerGuard.release();
      throw new Error("transfer caller identity mismatch");
    }
    let wire: TransferSession | undefined;
    try {
      const lease = await this.opts.transport.acquireFor({
        subscribe: grant.direction === "send"
          ? [grant.controlSubject, grant.dataSubject]
          : [grant.controlSubject],
        publish: grant.direction === "send"
          ? [grant.signalSubject]
          : [grant.signalSubject, grant.dataSubject],
      }, { deadlineMs: Date.parse(grant.expiresAt) });
      wire = new TransferSession(grant, lease, this.opts.auth, true);
      await wire.retainLocal();
      wire.peerGuard = callerGuard;
      wire.guards.push(callerGuard);
      wire.refreshDeadline();
      const settled = deferred<void>();
      const session: ProviderSession = {
        wire,
        startedAt: performance.now(),
        permission: args.permission,
        requiredCapabilities: args.requiredCapabilities,
        callerGuard,
        signalLane: Promise.resolve(),
        terminalSent: false,
        controlSeq: 0n,
        lastControl: "",
        active: false,
        maxFrameBytes: grant.maxFrameBytes,
        received: 0n,
        consumed: 0n,
        consumedBytes: 0,
        bytes: 0,
        hash: sha256.create(),
        store,
        storageKey,
        logicalKey,
        committed: false,
        cancelling: false,
        completing: false,
        progress: { chunkIndex: 0, chunkBytes: 0, transferredBytes: 0 },
        progressPending: false,
        settlement: settled.promise,
        settled,
      };
      wire.abort.signal.addEventListener("abort", () => {
        session.credit?.close();
        session.ingress?.fail(wire!.abort.signal.reason);
        session.window?.wakeCredit();
        void session.reader?.cancel(wire!.abort.signal.reason).catch(() => {});
        wire!.run(this.#settle(session));
      }, { once: true });
      let controlLane = Promise.resolve();
      let pendingControls = 0;
      wire.subscriptions.push(
        lease.nc.subscribe(grant.controlSubject, {
          callback: (error, msg) => {
            if (error) {
              wire!.fail(error);
              return;
            }
            if (
              ++pendingControls > grant.windowFrames ||
              msg.data.length > transferConstants().maxControlBytes
            ) {
              wire!.fail(new Error("transfer control flood"));
              return;
            }
            controlLane = controlLane.then(() => this.#control(session, msg))
              .finally(() => pendingControls--);
            wire!.run(controlLane);
          },
        }),
      );
      if (grant.direction === "send") {
        let dataLane = Promise.resolve();
        let pendingFrames = 0;
        let pendingBytes = 0;
        wire.subscriptions.push(
          lease.nc.subscribe(grant.dataSubject, {
            callback: (error, msg) => {
              if (error) {
                wire!.fail(error);
                return;
              }
              if (
                ++pendingFrames + (session.ingress?.bufferedFrames ?? 0) >
                  grant.windowFrames +
                    (msg.headers?.get("trellis-transfer-control") === "complete"
                      ? 1
                      : 0) ||
                (pendingBytes += msg.data.length) +
                      (session.ingress?.bufferedBytes ?? 0) >
                  grant.windowBytes +
                    (msg.headers?.get("trellis-transfer-control") === "complete"
                      ? transferConstants().headerReserve
                      : 0)
              ) {
                wire!.fail(
                  new Error("transfer upload verification window exceeded"),
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
                  { "trellis.direction": "upload" },
                );
              };
              dataLane = dataLane.then(() =>
                this.#uploadData(session, msg, handoff)
              )
                .finally(() => {
                  pendingFrames--;
                  pendingBytes -= msg.data.length;
                  handoff();
                });
              recordCatalogUpDown(
                "trellis.transfer.buffered.bytes",
                msg.data.length,
                { "trellis.direction": "upload" },
              );
              wire!.run(dataLane);
            },
          }),
        );
      }
      this.#sessions.set(grant.transferId, session);
      this.#owned.add(session);
      await lease.nc.flush();
      wire.throwIfAborted();
      return session;
    } catch (cause) {
      if (wire) wire.finish();
      else callerGuard.release();
      throw cause;
    }
  }

  #grant(
    direction: "send" | "receive",
    args: { sessionKey: string; connectionId: string; expiresInMs: number },
  ): Omit<SendTransferGrant, "direction"> & { direction: "send" | "receive" } {
    const C = transferConstants();
    const transferId = transferGenerateId();
    const subjects = {
      dataSubject: transferSubject(
        direction === "send" ? "upload-data" : "download-data",
        this.opts.connectionId,
        args.connectionId,
        transferId,
      ),
      controlSubject: transferSubject(
        "control",
        this.opts.connectionId,
        args.connectionId,
        transferId,
      ),
      signalSubject: transferSubject(
        "signal",
        this.opts.connectionId,
        args.connectionId,
        transferId,
      ),
    };
    const maxFrameBytes = Math.min(
      C.maxFrameBytes,
      (this.opts.transport.currentNats().info?.max_payload ?? 0) -
        C.headerReserve,
    );
    if (maxFrameBytes <= 0) {
      throw new Error("NATS payload limit cannot carry transfer frames");
    }
    return {
      format: "trellis.transfer.v2",
      type: "TransferGrant",
      direction,
      service: this.opts.name,
      transferId,
      provider: {
        connectionId: this.opts.connectionId,
        sessionKey: this.opts.auth.sessionKey,
      },
      consumer: {
        connectionId: args.connectionId,
        sessionKey: args.sessionKey,
      },
      ...subjects,
      expiresAt: new Date(Date.now() + args.expiresInMs).toISOString(),
      maxFrameBytes,
      windowFrames: C.windowFrames,
      windowBytes: C.windowBytes,
    };
  }

  /** Prepare exact provider subscriptions before returning an upload grant. */
  async initiateUpload(
    args: InitiateUploadArgs,
  ): Promise<ResultType<SendTransferGrant, TransferError>> {
    try {
      const store = await this.#staging();
      const status = (await store.status()).take();
      if (isErr(status)) throw status.error;
      const maxBytes = args.maxBytes === undefined
        ? status.maxObjectBytes
        : status.maxObjectBytes === undefined
        ? args.maxBytes
        : Math.min(args.maxBytes, status.maxObjectBytes);
      const grant: SendTransferGrant = {
        ...this.#grant("send", args),
        direction: "send",
        ...(maxBytes === undefined ? {} : { maxBytes }),
        ...(args.contentType ? { contentType: args.contentType } : {}),
        ...(args.metadata ? { metadata: args.metadata } : {}),
      };
      const session = await this.#prepare(
        grant,
        args,
        store,
        `_trellis-transfer-v2-${grant.transferId}`,
        args.operationId,
      );
      session.upload = args;
      return Result.ok(grant);
    } catch (cause) {
      return Result.err(
        new TransferError({ operation: "initiateUpload", cause }),
      );
    }
  }

  /** Create the provider's staged-upload helper while keeping public authoring stable. */
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
        const stored = deferred<ResultType<StoredTransfer, TransferError>>();
        const grant = (await this.initiateUpload({
          ...args,
          onProgress: (progress) => {
            updates.push(progress);
            return args.onProgress?.(progress);
          },
          onComplete: async (info) => {
            await args.onComplete?.(info);
          },
          onStored: async (value) => {
            await args.onStored?.(value);
            stored.resolve(Result.ok(value));
            completed.resolve(Result.ok(value.info));
            updates.close();
          },
          onError: async (error) => {
            stored.resolve(Result.err(error));
            completed.resolve(Result.err(error));
            updates.close();
            await args.onError?.(error);
          },
        })).take();
        if (isErr(grant)) return Result.err(grant.error);
        return Result.ok({
          grant,
          transfer: {
            updates: () => updates.subscribe(),
            completed: () => AsyncResult.from(completed.promise),
            stream: () =>
              AsyncResult.from((async () => {
                const value = (await stored.promise).take();
                if (isErr(value)) return Result.err(value.error);
                const stream = (await value.entry.stream()).take();
                return isErr(stream)
                  ? Result.err(
                    new TransferError({
                      operation: "stream",
                      cause: stream.error,
                    }),
                  )
                  : Result.ok(stream);
              })()),
            bytes: () =>
              AsyncResult.from((async () => {
                const value = (await stored.promise).take();
                if (isErr(value)) return Result.err(value.error);
                const bytes = (await value.entry.bytes()).take();
                return isErr(bytes)
                  ? Result.err(
                    new TransferError({
                      operation: "bytes",
                      cause: bytes.error,
                    }),
                  )
                  : Result.ok(bytes);
              })()),
          },
        });
      })(),
    );
  }

  /** Recover a previously committed staged upload; interrupted uploads restart at zero. */
  openStagedOperation(
    storageKey: string,
    committed: FileInfo,
  ): AsyncResult<OperationTransferHandle, TransferError> {
    return AsyncResult.from(
      (async (): Promise<
        ResultType<OperationTransferHandle, TransferError>
      > => {
        try {
          const store = await this.#staging();
          const info = structuredClone(committed);
          const expectedDigest = info.digest?.replace(/=+$/, "");
          if (!expectedDigest) {
            throw new Error("committed upload digest is missing");
          }
          const openStream = (): AsyncResult<
            ReadableStream<Uint8Array>,
            TransferError
          > =>
            AsyncResult.from((async () => {
              try {
                const entry = (await store.get(storageKey)).take();
                if (isErr(entry)) throw entry.error;
                const actual = fileInfoFromStoreInfo(entry.info);
                const metadata = info.metadata ?? {};
                const actualMetadata = actual.metadata ?? {};
                if (
                  actual.key !== storageKey || actual.size !== info.size ||
                  actual.digest?.replace(/=+$/, "") !== expectedDigest ||
                  actual.contentType !== info.contentType ||
                  Object.keys(actualMetadata).length !==
                    Object.keys(metadata).length ||
                  Object.entries(metadata).some(([key, value]) =>
                    actualMetadata[key] !== value
                  )
                ) {
                  throw new Error(
                    "staged upload metadata does not match its durable commit",
                  );
                }
                const source = (await entry.stream()).take();
                if (isErr(source)) throw source.error;
                const hash = sha256.create();
                let size = 0;
                return Result.ok(
                  source.pipeThrough(
                    new TransformStream<Uint8Array, Uint8Array>({
                      transform(chunk, controller) {
                        size += chunk.length;
                        if (size > info.size) {
                          throw new TransferError({
                            operation: "stream",
                            cause: new Error(
                              "staged upload size exceeded its durable commit",
                            ),
                          });
                        }
                        hash.update(chunk);
                        controller.enqueue(chunk);
                      },
                      flush() {
                        if (
                          size !== info.size ||
                          `SHA-256=${base64urlEncode(hash.digest())}` !==
                            expectedDigest
                        ) {
                          throw new TransferError({
                            operation: "stream",
                            cause: new Error(
                              "staged upload bytes do not match its durable commit",
                            ),
                          });
                        }
                      },
                    }),
                  ),
                );
              } catch (cause) {
                return Result.err(
                  new TransferError({ operation: "stream", cause }),
                );
              }
            })());
          // Recovery must reject corrupted persisted bytes before making the
          // handler runnable. Drain without collecting, then verify every later
          // read too because the backend can be overwritten after this check.
          const verified = (await openStream()).take();
          if (isErr(verified)) throw verified.error;
          const reader = verified.getReader();
          try {
            while (!(await reader.read()).done) { /* verification only */ }
          } finally {
            await reader.cancel().catch(() => {});
            reader.releaseLock();
          }
          return Result.ok({
            updates: () => (async function* () {})(),
            completed: () => AsyncResult.ok(structuredClone(info)),
            stream: openStream,
            bytes: () =>
              AsyncResult.from((async () => {
                const source = (await openStream()).take();
                if (isErr(source)) return Result.err(source.error);
                const reader = source.getReader();
                let retainedBytes = 0;
                try {
                  const bytes = new Uint8Array(info.size);
                  retainedBytes = bytes.length;
                  recordCatalogUpDown(
                    "trellis.transfer.buffered.bytes",
                    retainedBytes,
                    { "trellis.direction": "upload" },
                  );
                  let offset = 0;
                  while (true) {
                    const next = await reader.read();
                    if (next.done) break;
                    bytes.set(next.value, offset);
                    offset += next.value.length;
                  }
                  return Result.ok(bytes);
                } catch (cause) {
                  return Result.err(
                    new TransferError({ operation: "bytes", cause }),
                  );
                } finally {
                  recordCatalogUpDown(
                    "trellis.transfer.buffered.bytes",
                    -retainedBytes,
                    { "trellis.direction": "upload" },
                  );
                  await reader.cancel().catch(() => {});
                  reader.releaseLock();
                }
              })()),
          });
        } catch (cause) {
          return Result.err(
            new TransferError({ operation: "openStagedOperation", cause }),
          );
        }
      })(),
    );
  }

  /** Prepare a logical download without opening its backend byte stream before activation. */
  async initiateDownload(
    args: InitiateDownloadArgs,
  ): Promise<ResultType<ReceiveTransferGrant, TransferError>> {
    try {
      const handle = this.opts.stores[args.store];
      if (!handle) throw new Error("unknown transfer store");
      const store = (await handle.open()).take();
      if (isErr(store)) throw store.error;
      const entry = (await store.get(args.key)).take();
      if (isErr(entry)) throw entry.error;
      const info = fileInfoFromStoreInfo(entry.info);
      if (!info.digest) throw new Error("download metadata missing digest");
      const grant: ReceiveTransferGrant = {
        ...this.#grant("receive", args),
        direction: "receive",
        info: { ...info, digest: info.digest },
      };
      await this.#prepare(
        grant,
        { ...args, requiredCapabilities: args.requiredCapabilities ?? [] },
        store,
        args.key,
        args.key,
      );
      return Result.ok(grant);
    } catch (cause) {
      return Result.err(
        new TransferError({ operation: "initiateDownload", cause }),
      );
    }
  }

  async #verify(
    session: ProviderSession,
    msg: Msg,
    frame?: TransferFrameDescriptor,
  ): Promise<void> {
    const wire = session.wire;
    wire.throwIfAborted();
    const cache = this.opts.auth.authorizationProviderCache;
    if (!cache) throw new Error("transfer authorization cache unavailable");
    const proofPayload = frame
      ? await transferWait(
        cache.frameDigest({
          kind: "transfer-frame-digest",
          descriptor: frame,
          payload: msg.data,
        }),
        wire.abort.signal,
      )
      : undefined;
    wire.throwIfAborted();
    if (msg.reply !== wire.grant.signalSubject) {
      throw new Error("transfer signal reply mismatch");
    }
    for (
      const name of [
        "authorization-context",
        "session-key",
        "proof",
        "iat",
        "request-id",
      ]
    ) {
      const values = msg.headers?.values(name);
      if (values?.length !== 1 || !values[0]) {
        throw new Error("ambiguous transfer request proof header");
      }
    }
    const caller = (await transferWait(
      verifyLocalAuthorization({
        kind: "transfer",
        cache: this.opts.auth.authorizationProviderCache,
        message: msg,
        permission: session.permission,
        requiredCapabilities: session.requiredCapabilities,
        transferId: wire.grant.transferId,
        providerConnectionId: wire.grant.provider.connectionId,
        consumerConnectionId: wire.grant.consumer.connectionId,
        ...(proofPayload === undefined ? {} : { proofPayload }),
      }),
      wire.abort.signal,
    )).take();
    if (
      isErr(caller) || caller.sessionKey !== wire.grant.consumer.sessionKey ||
      caller.connectionId !== wire.grant.consumer.connectionId
    ) throw new Error("invalid transfer caller proof");
    if (caller.contextDigest !== session.callerGuard.contextDigest) {
      const replacement = await transferWait(
        session.callerGuard.prepareReplacement(caller.contextDigest).then(
          (replacement) => {
            if (wire.abort.signal.aborted) {
              replacement.release();
              wire.throwIfAborted();
            }
            return replacement;
          },
        ),
        wire.abort.signal,
      );
      const lost = session.callerGuard.commitReplacement(replacement);
      replacement.release();
      if (lost) {
        throw new Error("transfer caller authority changed");
      }
    }
    if (session.callerGuard.checkNow()) {
      throw new Error("transfer caller authority lost");
    }
    wire.refreshDeadline();
    wire.throwIfAborted();
  }

  #signal(
    session: ProviderSession,
    value: SignalBody,
  ): Promise<void> {
    if (value.type === "error") {
      session.signalLane = session.signalLane.catch(() => {});
    }
    session.signalLane = session.signalLane.then(async () => {
      const signal = {
        ...value,
        format: "trellis.transfer.v2",
        transferId: session.wire.grant.transferId,
      };
      const payload = new TextEncoder().encode(JSON.stringify(signal));
      const subject = session.wire.grant.signalSubject;
      const headers = await session.wire.serverHeaders(
        subject,
        payload,
        "0",
        "signal",
        undefined,
        value.type === "error",
      );
      if (value.type !== "error") session.wire.throwIfAborted();
      this.opts.log?.debug({
        operationId: session.upload?.operationId,
        transferId: session.wire.grant.transferId,
        signalType: value.type,
        bodyBytes: payload.length,
        maxBodyBytes: transferConstants().maxControlBytes,
        headerCount: headers.keys().length,
        headerValueBytes: [...headers].reduce(
          (bytes, [, values]) =>
            bytes +
            values.reduce(
              (n, value) => n + new TextEncoder().encode(value).length,
              0,
            ),
          0,
        ),
      }, "Transfer publishing signed signal");
      session.wire.lease.nc.publish(subject, payload, { headers });
      if (
        value.type === "error" || value.type === "cancelled" ||
        value.type === "committed"
      ) session.terminalSent = true;
    });
    return session.signalLane;
  }

  async #control(session: ProviderSession, msg: Msg): Promise<void> {
    const control = transferParseControl(msg.data);
    const C = transferConstants();
    const sequence = msg.headers?.values(C.sequenceHeader);
    const kind = msg.headers?.values(C.controlHeader);
    if (
      sequence?.length !== 1 || sequence[0] !== control.controlSeq ||
      kind?.length !== 1 || kind[0] !== "control" ||
      msg.headers?.values(C.terminalHeader)?.length
    ) throw new Error("transfer control descriptor mismatch");
    if (control.transferId !== session.wire.grant.transferId) {
      throw new Error("transfer control session mismatch");
    }
    await this.#verify(
      session,
      msg,
      {
        transferId: session.wire.grant.transferId,
        direction: session.wire.grant.direction,
        sequence: control.controlSeq,
        kind: "control",
      },
    );
    const seq = BigInt(control.controlSeq);
    const body = JSON.stringify(control);
    if (seq === session.controlSeq && body === session.lastControl) return;
    if (seq !== session.controlSeq + 1n) {
      throw new Error("transfer control sequence gap");
    }
    session.controlSeq = seq;
    session.lastControl = body;
    const received = BigInt(control.receivedSeq);
    const consumed = BigInt(control.consumedSeq);
    if (control.action === "activate") {
      if (
        session.active || received !== 0n || consumed !== 0n ||
        control.receiveMaxFrameBytes === undefined
      ) throw new Error("invalid transfer activation");
      session.maxFrameBytes = Math.min(
        session.maxFrameBytes,
        control.receiveMaxFrameBytes,
        (session.wire.lease.nc.info?.max_payload ?? 0) -
          transferConstants().headerReserve,
      );
      if (session.maxFrameBytes < 1) {
        throw new Error("invalid transfer frame negotiation");
      }
      session.active = true;
      if (session.wire.grant.direction === "send") this.#startUpload(session);
      else {session.window = new SenderWindow({
          maxFrameBytes: session.maxFrameBytes,
          windowFrames: session.wire.grant.windowFrames,
          windowBytes: session.wire.grant.windowBytes,
        });}
      await this.#signal(session, {
        type: "activated",
        controlSeq: control.controlSeq,
        requestId: msg.headers?.get("request-id") ?? "",
        maxFrameBytes: session.maxFrameBytes,
        windowFrames: session.wire.grant.windowFrames,
        windowBytes: session.wire.grant.windowBytes,
      });
      if (session.wire.grant.direction === "receive") {
        session.download = this.#download(session);
        session.wire.run(session.download);
      }
    } else if (control.action === "cancel") {
      if (session.committed) return;
      // Entering the durable barrier is the point after which wire cancellation
      // cannot promise an uncommitted outcome. Let its actual result win.
      if (session.barrier) {
        await session.barrier.catch(() => {});
        return;
      }
      session.cancelling = true;
      await this.#signal(session, { type: "cancelled" });
      session.wire.fail(new Error("transfer cancelled"));
    } else if (control.action === "credit" || control.action === "end-ack") {
      if (
        !session.active || !session.window ||
        session.window.applyCredit(
          received,
          consumed,
          transferParseCounter(control.consumedBytes),
        )
      ) throw new Error("invalid transfer cumulative credit");
      recordCatalogCounter("trellis.transfer.credit.controls", 1, {
        "trellis.direction": "download",
      });
      if (control.action === "end-ack") {
        if (
          !session.completing ||
          session.window.validateComplete(session.window.highestSent)
        ) throw new Error("incomplete transfer end acknowledgement");
        await this.#settle(session);
      }
    }
  }

  #startUpload(session: ProviderSession): void {
    const C = transferConstants();
    const args = session.upload;
    if (!args) throw new Error("upload session not installed");
    session.credit = new TransferCredit(
      session.wire,
      {
        frameStep: C.creditFrameStep,
        byteStep: C.creditByteStep,
        maxDelayMs: C.creditMaxDelayMs,
      },
      (received, consumed) =>
        this.#signal(session, {
          type: "credit",
          receivedSeq: received.toString(),
          consumedSeq: consumed.toString(),
          consumedBytes: String(session.consumedBytes),
        }),
    );
    const submitProgress = () => {
      if (
        session.progressTask || !session.progressPending ||
        session.wire.abort.signal.aborted
      ) return;
      session.progressTask = (async () => {
        while (session.progressPending && !session.wire.abort.signal.aborted) {
          session.progressPending = false;
          const progress = session.progress;
          this.opts.log?.debug({
            operationId: args.operationId,
            transferId: session.wire.grant.transferId,
            consumedSeq: session.consumed.toString(),
            consumedBytes: session.consumedBytes,
            submittedBytes: progress.transferredBytes,
            hasProgressCallback: args.onProgress !== undefined,
          }, "Transfer submitting backend-consumed progress");
          await transferWait(
            Promise.resolve(args.onProgress?.(progress)),
            session.wire.abort.signal,
          );
          this.opts.log?.debug({
            operationId: args.operationId,
            transferId: session.wire.grant.transferId,
            submittedBytes: progress.transferredBytes,
          }, "Transfer progress callback returned");
        }
      })().finally(() => {
        session.progressTask = undefined;
        submitProgress();
      });
      session.wire.run(session.progressTask);
    };
    session.ingress = new TransferIngress(
      session.maxFrameBytes,
      session.wire.grant.windowFrames,
      session.wire.grant.windowBytes,
      (seq, bytes) => {
        const previousBytes = session.consumedBytes;
        session.consumed = seq;
        session.consumedBytes += bytes;
        if (
          Math.floor(previousBytes / (1024 * 1024)) !==
            Math.floor(session.consumedBytes / (1024 * 1024))
        ) {
          this.opts.log?.debug({
            operationId: args.operationId,
            transferId: session.wire.grant.transferId,
            consumedSeq: seq.toString(),
            consumedBytes: session.consumedBytes,
            progressPending: session.progressPending,
            progressTaskActive: session.progressTask !== undefined,
          }, "Transfer backend consumed upload frames");
        }
        session.progress = {
          chunkIndex: Number(seq - 1n),
          chunkBytes: bytes,
          transferredBytes: session.consumedBytes,
        };
        session.credit!.note(session.received, seq, session.consumedBytes);
        // Observation submission never blocks storage consumption or network credit.
        session.progressPending = true;
        submitProgress();
      },
    );
    session.backend = (async () => {
      const result =
        (await session.store.put(session.storageKey, session.ingress!, {
          ...(args.contentType ? { contentType: args.contentType } : {}),
          ...(args.metadata ? { metadata: args.metadata } : {}),
        })).take();
      if (isErr(result)) {
        this.opts.log?.debug({
          operationId: args.operationId,
          transferId: session.wire.grant.transferId,
          consumedBytes: session.consumedBytes,
          err: result.error,
        }, "Transfer backend upload failed");
        throw result.error;
      }
    })();
    session.wire.run(session.backend);
  }

  async #uploadData(
    session: ProviderSession,
    msg: Msg,
    handoff: () => void,
  ): Promise<void> {
    if (!session.active || !session.ingress || session.completing) {
      throw new Error("upload DATA outside active session");
    }
    const C = transferConstants();
    const rawSeq = msg.headers?.values(C.sequenceHeader);
    const rawControl = msg.headers?.values(C.controlHeader);
    if (
      !rawSeq || rawSeq.length !== 1 || !rawControl ||
      rawControl.length !== 1 || msg.headers?.values(C.terminalHeader)?.length
    ) throw new Error("invalid transfer frame headers");
    const seq = BigInt(transferParseCounter(rawSeq[0]));
    const control = rawControl?.[0] ?? "";
    if (control !== "data" && control !== "complete") {
      throw new Error("invalid upload frame kind");
    }
    await this.#verify(
      session,
      msg,
      {
        transferId: session.wire.grant.transferId,
        direction: "send",
        sequence: seq.toString(),
        kind: control,
      },
    );
    if (control === "complete") {
      const complete = transferParseComplete(msg.data);
      const digest = `SHA-256=${base64urlEncode(session.hash.digest())}`;
      if (
        complete.transferId !== session.wire.grant.transferId ||
        BigInt(complete.finalSeq) !== session.received ||
        seq !== session.received || complete.size !== session.bytes ||
        complete.digest !== digest
      ) throw new Error("upload completion mismatch");
      session.completing = true;
      session.ingress.close(session.received);
      await transferWait(session.backend!, session.wire.abort.signal);
      session.wire.throwIfAborted();
      const entry = (await session.store.get(session.storageKey)).take();
      if (isErr(entry)) throw entry.error;
      const info = fileInfoFromStoreInfo(entry.info);
      if (
        info.key !== session.storageKey || info.size !== session.bytes ||
        info.digest?.replace(/=+$/, "") !== digest.replace(/=+$/, "")
      ) throw new Error("upload stored metadata mismatch");
      await session.credit!.flush();
      const stored: StoredTransfer = {
        transferId: session.wire.grant.transferId,
        sessionKey: session.wire.grant.consumer.sessionKey,
        store: session.store,
        entry,
        storageKey: session.storageKey,
        info: { ...info, key: session.logicalKey, digest },
      };
      session.wire.throwIfAborted();
      if (session.cancelling) {
        throw new Error("transfer cancelled before commit");
      }
      session.barrier = (async () => {
        await session.upload?.commit?.(stored, session.progress);
        await session.upload?.onComplete?.(stored.info);
        // Once the required durable transition succeeds, cancellation cannot
        // delete its staged object, even if the carrier dies before signalling.
        session.committed = true;
        await session.upload?.onStored?.(stored);
      })();
      await session.barrier;
      await this.#signal(session, {
        type: "committed",
        finalSeq: session.received.toString(),
        info: { ...info, key: session.logicalKey, digest },
      });
      await this.#settle(session);
      return;
    }
    if (control !== "data" || seq !== session.received + 1n) {
      throw new Error("upload DATA sequence gap");
    }
    session.bytes += msg.data.length;
    const grant = session.wire.grant;
    if (
      !Number.isSafeInteger(session.bytes) ||
      (grant.direction === "send" && grant.maxBytes !== undefined &&
        session.bytes > grant.maxBytes)
    ) throw new Error("upload maximum bytes exceeded");
    session.received = seq;
    session.hash.update(msg.data);
    handoff();
    session.ingress.push(seq, msg.data);
    recordCatalogCounter("trellis.transfer.frames", 1, {
      "trellis.direction": "upload",
    });
    recordCatalogCounter("trellis.transfer.wire.bytes", msg.data.length, {
      "trellis.direction": "upload",
    });
  }

  async #download(session: ProviderSession): Promise<void> {
    const grant = session.wire.grant;
    if (grant.direction !== "receive" || !session.window) {
      throw new Error("invalid download session");
    }
    const window = session.window;
    const entry = (await session.store.get(session.storageKey)).take();
    if (isErr(entry)) throw entry.error;
    const info = fileInfoFromStoreInfo(entry.info);
    if (
      info.size !== grant.info.size || info.digest !== grant.info.digest ||
      info.key !== grant.info.key
    ) throw new Error("download source metadata changed");
    const stream = (await entry.stream()).take();
    if (isErr(stream)) throw stream.error;
    const reader = stream.getReader();
    session.reader = reader;
    try {
      while (true) {
        const next = await transferWait(
          reader.read(),
          session.wire.abort.signal,
        );
        if (next.done) break;
        recordCatalogUpDown(
          "trellis.transfer.buffered.bytes",
          next.value.length,
          { "trellis.direction": "download" },
        );
        try {
          for (
            let offset = 0;
            offset < next.value.length;
            offset += session.maxFrameBytes
          ) {
            const frame = next.value.subarray(
              offset,
              Math.min(next.value.length, offset + session.maxFrameBytes),
            );
            while (window.validateFrameSlot(frame.length) === "window_full") {
              await transferWait(
                window.waitCredit(),
                session.wire.abort.signal,
              );
            }
            const seq = window.nextFrameSeq();
            if (seq === undefined || window.validateFrameSlot(frame.length)) {
              throw new Error("download frame accounting failed");
            }
            const headers = await session.wire.serverHeaders(
              grant.dataSubject,
              frame,
              seq.toString(),
              "data",
            );
            session.wire.throwIfAborted();
            session.wire.lease.nc.publish(grant.dataSubject, frame, {
              headers,
            });
            if (window.commitFrame(seq, frame.length)) {
              throw new Error("download frame handoff failed");
            }
            session.bytes += frame.length;
            if (session.bytes > grant.info.size) {
              throw new Error("download source size exceeded");
            }
            session.hash.update(frame);
            recordCatalogCounter("trellis.transfer.frames", 1, {
              "trellis.direction": "download",
            });
            recordCatalogCounter("trellis.transfer.wire.bytes", frame.length, {
              "trellis.direction": "download",
            });
          }
        } finally {
          recordCatalogUpDown(
            "trellis.transfer.buffered.bytes",
            -next.value.length,
            { "trellis.direction": "download" },
          );
        }
      }
      const digest = `SHA-256=${base64urlEncode(session.hash.digest())}`;
      if (
        session.bytes !== grant.info.size ||
        digest.replace(/=+$/, "") !== grant.info.digest.replace(/=+$/, "")
      ) throw new Error("download source digest mismatch");
      const payload = new Uint8Array();
      const headers = await session.wire.serverHeaders(
        grant.dataSubject,
        payload,
        window.highestSent.toString(),
        "eof",
        {
          finalSeq: window.highestSent.toString(),
          size: session.bytes,
          digest,
        },
      );
      session.completing = true;
      session.wire.throwIfAborted();
      session.wire.lease.nc.publish(grant.dataSubject, payload, { headers });
    } finally {
      await reader.cancel().catch(() => {});
      reader.releaseLock();
      session.reader = undefined;
    }
  }

  async #settle(session: ProviderSession): Promise<void> {
    if (!this.#sessions.delete(session.wire.grant.transferId)) return;
    const log = this.opts.log?.child({
      operationId: session.upload?.operationId,
      transferId: session.wire.grant.transferId,
      providerConnectionId: this.opts.connectionId,
      physicalClientId: session.wire.lease.nc.info?.client_id,
    });
    log?.debug({
      committed: session.committed,
      completing: session.completing,
      consumedBytes: session.consumedBytes,
    }, "Transfer settlement started");
    try {
      session.credit?.close();
      await session.barrier?.catch(() => {});
      log?.debug("Transfer commit barrier joined");
      await session.download?.catch(() => {});
      if (session.wire.abort.signal.aborted && !session.terminalSent) {
        await this.#signal(session, { type: "error", code: "closed" }).catch(
          () => {},
        );
      }
      if (!session.committed && session.wire.grant.direction === "send") {
        const error = session.wire.abort.signal.reason instanceof TransferError
          ? session.wire.abort.signal.reason
          : new TransferError({
            operation: "transfer",
            cause: session.wire.abort.signal.reason,
          });
        session.ingress?.fail(error);
        await session.backend?.catch(() => {});
        log?.debug("Transfer upload backend joined");
        // A lost CAS response is not evidence that the durable write failed.
        // Unknown authoritative state retains this attempt for reconciliation.
        const committed = session.upload?.reconcileCommit
          ? await session.upload.reconcileCommit(
            session.storageKey,
            session.wire.grant.transferId,
          ).catch(() => undefined)
          : false;
        if (committed === true) session.committed = true;
        if (committed === false && session.completing) {
          await session.store.delete(session.storageKey);
        }
        await session.upload?.onError?.(error);
        log?.debug("Transfer upload error callback returned");
      }
    } finally {
      recordCatalogDuration(
        "trellis.transfer.duration",
        performance.now() - session.startedAt,
        {
          "trellis.direction": session.wire.grant.direction === "send"
            ? "upload"
            : "download",
          "trellis.outcome": session.committed ||
              (session.wire.grant.direction === "receive" &&
                session.completing && !session.wire.abort.signal.aborted)
            ? "ok"
            : "error",
        },
      );
      session.wire.finish();
      log?.debug("Transfer guards and pinned generation released");
      session.settled.resolve();
      void session.wire.join().then(() => {
        this.#owned.delete(session);
        log?.debug("Transfer owned tasks joined");
      });
    }
  }

  /** Stop intake, abort backend streams and join every owned session task. */
  async stop(): Promise<void> {
    const sessions = [...this.#owned];
    for (const session of sessions) {
      session.wire.fail(new Error("transfer provider stopped"));
    }
    await Promise.all(sessions.map(async (session) => {
      await session.settlement;
      await session.wire.join();
    }));
  }
}
