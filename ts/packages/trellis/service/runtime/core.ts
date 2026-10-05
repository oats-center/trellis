import {
  createInbox,
  headers,
  type Msg,
  type NatsConnection,
} from "@nats-io/nats-core";
import { Pointer, Value } from "typebox/value";
import {
  type PermissionAtom,
  routeQueueGroup,
  type RuntimeApi,
} from "../../participant_runtime/api.ts";
import {
  AsyncResult,
  BaseError,
  err,
  isErr,
  ok,
  type Result,
} from "@oatscenter/result";
import { ulid } from "ulid";
import { base64urlEncode, utf8 } from "../../auth/utils.ts";

import { encodeSchema, type JsonValue, parseSchema } from "../../codec.ts";
import {
  canonicalizeJson,
  digestJson,
} from "../../participant_runtime/json.ts";
import {
  AuthError,
  OperationAlreadyTerminalError,
  OperationNotFoundError,
  TransferError,
  TransportError,
  UnexpectedError,
  ValidationError,
} from "../../errors/index.ts";
import { LiveProvider } from "../../live/provider.ts";
import {
  parseOperationWatchOpen,
  runDelayedOperationSource,
} from "../../live/operation_source.ts";
import type { LoggerLike } from "../../globals.ts";
import { serviceRuntimeLogger } from "./logger.ts";
import {
  recordTrellisError,
  type TrellisErrorMetricAttributes,
} from "../../telemetry/mod.ts";
import {
  recordCatalogCounter,
  recordCatalogDuration,
  recordCatalogUpDown,
  routeToken,
} from "../../telemetry/metrics.ts";
import {
  createMapCarrier,
  extractTraceContext,
  validateTraceparent,
  validateTracestate,
} from "../../telemetry/carrier.ts";
import { getTrellisTracer } from "../../telemetry/trace.ts";
import {
  type Context,
  context as otelContext,
  ROOT_CONTEXT,
  SpanKind,
  trace,
} from "@opentelemetry/api";
import {
  annotateHandlerBoundaryError,
  buildRuntimeOperationSnapshot,
  type DurableOperationRecord,
  type HandlerFn,
  isOperationDeferred,
  isResultLike,
  isTerminalRuntimeOperationSnapshot,
  type MethodsOf,
  type OperationHandlerContext,
  type OperationInputOf,
  type OperationOutputOf,
  type OperationProgressOf,
  type OperationRegistration,
  type OperationRuntimeHandle,
  type OperationsOf,
  type OperationTransferContextOf,
  type OperationTransferHandle,
  type OperationUpdateOf,
  type RuntimeOperationAcceptedEnvelope,
  type RuntimeOperationController,
  type RuntimeOperationControlRequest,
  type RuntimeOperationDesc,
  type RuntimeOperationRecord,
  type RuntimeOperationSignal,
  type RuntimeOperationSnapshot,
  type RuntimeOperationState,
  safeJson,
  toVerifierPermission,
  Trellis,
  type TrellisAuth,
  type TrellisMode,
  type TrellisOpts,
  type VerifiedCaller,
  verifyLocalAuthorization,
} from "../../session.ts";
import { LiveAuthorityGuard } from "../../live/authority.ts";
import type { StoredTransfer } from "./transfer.ts";
import type { ProviderAuthorityPort } from "../../live/provider.ts";
import {
  fixedTransportProvider,
  TransportGenerationManager,
  type TransportLease,
  type TrellisTransportProvider,
} from "../../transport/generations.ts";
import { TypedKV } from "../../kv.ts";
import {
  type FileInfo,
  FileInfoSchema,
  type SendTransferGrant,
} from "../../transfer.ts";

type TrellisServiceRuntimeOpts<TA extends RuntimeApi> =
  & Omit<TrellisOpts<TA>, "api">
  & {
    api: TA;
    transferSupport?: RuntimeOperationTransferSupport;
    version?: string;
    operationDeploymentId?: string;
    operationConnectionId?: string;
  };

export type TrellisServiceRuntimeFor<TA extends RuntimeApi = RuntimeApi> =
  & Omit<TrellisServiceRuntime, "mount" | "operationHandle">
  & {
    mount<M extends MethodsOf<TA>>(
      method: M,
      fn: HandlerFn<TA, M>,
    ): Promise<void>;
    operationHandle<O extends OperationsOf<TA>>(
      operation: O,
    ): OperationRegistration<
      OperationInputOf<TA, O>,
      OperationProgressOf<TA, O>,
      OperationOutputOf<TA, O>,
      OperationTransferContextOf<TA, O>,
      BaseError,
      OperationUpdateOf<TA, O>
    >;
  };

type RegisteredRuntimeOperationDesc = RuntimeOperationDesc & {
  permissions?: RuntimeApi["operations"][string]["permissions"];
  callerCapabilities?: readonly string[];
  observeCapabilities?: readonly string[];
  cancelCapabilities?: readonly string[];
  controlCapabilities?: readonly string[];
};

type RuntimeOperationTransferSession = {
  grant: SendTransferGrant;
  transfer: OperationTransferHandle;
};

type RuntimeOperationTransferSupport = {
  openOperationTransfer(args: {
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
    commit?: (
      stored: StoredTransfer,
      progress: NonNullable<RuntimeOperationSnapshot["transfer"]>,
    ) => Promise<void>;
    onError?: (error: TransferError) => Promise<void>;
    onProgress?: (
      progress: NonNullable<RuntimeOperationSnapshot["transfer"]>,
    ) => void;
  }): AsyncResult<RuntimeOperationTransferSession, TransferError>;
  /** Reads the exact committed attempt key without re-initiating a transfer. */
  openStagedOperation(
    stagingKey: string,
    committed: FileInfo,
  ): AsyncResult<OperationTransferHandle, TransferError>;
};

type RuntimeOperationFence = Readonly<{
  ownerInstanceId: string;
  ownerConnectionId: string;
  ownerEpoch: number;
}>;

const CANONICAL_POSITIVE_INTEGER = /^[1-9][0-9]*$/u;

/** Maximum admitted-but-not-yet-emitted observer entries. */
const OBSERVER_ARBITER_BOUND = 16;

/**
 * Serializes one Operation observer's backend notifications.
 *
 * A snapshot waiting to be emitted is coalesced to the latest authoritative
 * one; admitted updates are never dropped. The bounded buffer rejects further
 * updates so only that observer fails with a slow-consumer outcome.
 */
export class OperationObserverArbiter {
  readonly #emit: (value: unknown) => Promise<void>;
  readonly #updates: unknown[] = [];
  #pendingSnapshot: unknown | undefined;
  #snapshotDeferred: PromiseWithResolvers<void> | undefined;
  #draining = false;
  #failed = false;

  constructor(emit: (value: unknown) => Promise<void>) {
    this.#emit = emit;
  }

  /** Queue one authoritative snapshot, coalescing with a pending one. */
  snapshot(value: unknown): Promise<void> {
    this.#pendingSnapshot = value;
    // Capture the deferred locally: the drain loop starts synchronously inside
    // `#kick` and may clear `#snapshotDeferred` before this returns.
    const deferred = (this.#snapshotDeferred ??= Promise.withResolvers<void>());
    this.#kick();
    return deferred.promise;
  }

  /** Admit one transient update; false means the bound was exceeded. */
  update(value: unknown): boolean {
    if (this.#failed || this.#updates.length >= OBSERVER_ARBITER_BOUND) {
      return false;
    }
    this.#updates.push(value);
    this.#kick();
    return true;
  }

  #kick(): void {
    if (this.#draining || this.#failed) return;
    this.#draining = true;
    void (async () => {
      try {
        while (true) {
          const nextUpdate = this.#updates.shift();
          if (nextUpdate !== undefined) {
            await this.#emit(nextUpdate);
            continue;
          }
          if (this.#pendingSnapshot !== undefined) {
            const deferred = this.#snapshotDeferred;
            const value = this.#pendingSnapshot;
            this.#pendingSnapshot = undefined;
            this.#snapshotDeferred = undefined;
            await this.#emit(value);
            deferred?.resolve();
            continue;
          }
          // Clear the draining flag and re-check without awaiting, so a
          // snapshot or update queued during the last emit is never lost.
          this.#draining = false;
          if (
            this.#updates.length === 0 && this.#pendingSnapshot === undefined
          ) {
            return;
          }
          this.#draining = true;
        }
      } catch {
        this.#failed = true;
        this.#updates.splice(0);
        this.#snapshotDeferred?.reject(
          new Error("operation observer emit failed"),
        );
        this.#snapshotDeferred = undefined;
      } finally {
        this.#draining = false;
      }
    })();
  }
}

function asOptionalStringPointerValue(
  input: unknown,
  pointer?: `/${string}`,
): Result<string | undefined, TransferError> {
  if (!pointer) {
    return ok(undefined);
  }
  const value = Pointer.Get(input as Record<string, unknown>, pointer);
  if (value === undefined) {
    return ok(undefined);
  }
  if (typeof value !== "string" || value.length === 0) {
    return err(
      new TransferError({
        operation: "transfer",
        context: { reason: "invalid_input", field: pointer, pointer },
      }),
    );
  }
  return ok(value);
}

function asOptionalStringRecordPointerValue(
  input: unknown,
  pointer?: `/${string}`,
): Result<Record<string, string> | undefined, TransferError> {
  if (!pointer) {
    return ok(undefined);
  }
  const value = Pointer.Get(input as Record<string, unknown>, pointer);
  if (value === undefined) {
    return ok(undefined);
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return err(
      new TransferError({
        operation: "transfer",
        context: { reason: "invalid_input", field: pointer, pointer },
      }),
    );
  }

  const entries = Object.entries(value as Record<string, unknown>);
  if (
    entries.some(([key, item]) => key.length === 0 || typeof item !== "string")
  ) {
    return err(
      new TransferError({
        operation: "transfer",
        context: { reason: "invalid_input", field: pointer, pointer },
      }),
    );
  }

  return ok(Object.fromEntries(entries) as Record<string, string>);
}

function traceIdFromTraceparent(
  traceparent: string | undefined,
): string | undefined {
  const [version, traceId, parentId, flags, extra] = traceparent?.split("-") ??
    [];
  if (
    extra !== undefined ||
    !/^[0-9a-f]{2}$/u.test(version ?? "") ||
    version === "ff" ||
    !/^[0-9a-f]{32}$/u.test(traceId ?? "") ||
    traceId === "00000000000000000000000000000000" ||
    !/^[0-9a-f]{16}$/u.test(parentId ?? "") ||
    parentId === "0000000000000000" ||
    !/^[0-9a-f]{2}$/u.test(flags ?? "")
  ) {
    return undefined;
  }
  return traceId;
}

function recordOperationServiceError(
  error: unknown,
  attributes: TrellisErrorMetricAttributes,
): void {
  recordTrellisError(error, {
    messagingSystem: "nats",
    surface: "operation",
    direction: "server",
    ...attributes,
  });
}

function isOperationRevisionConflict(cause: unknown): boolean {
  const message = cause instanceof Error
    ? `${cause.message} ${
      cause.cause instanceof Error ? cause.cause.message : ""
    }`
      .toLowerCase()
    : "";
  return message.includes("operation revision conflict") ||
    message.includes("wrong last sequence") ||
    message.includes("wrong last revision") ||
    message.includes("revision mismatch") ||
    message.includes("sequence mismatch");
}

/**
 * Whether a verified caller is the creator of one durable operation.
 *
 * Observation and control authority is scoped to the exact creating principal
 * and participant. A foreign principal, or the same principal on a different
 * participant, is denied — the operation is not disclosed to a guesser.
 */
export function operationObserveAuthorized(
  runtime: { creatorPrincipalId: string; creatorParticipantId: string },
  caller?: { principalId: string; participantId: string },
): boolean {
  return !caller ||
    (runtime.creatorPrincipalId === caller.principalId &&
      runtime.creatorParticipantId === caller.participantId);
}

/**
 * Whether a durable-operation update's executor fence still owns the operation.
 *
 * A legitimately signed internal update from a former executor — a stale epoch,
 * or a different instance — is rejected before any outer Live delivery.
 */
export function operationOwnerFenceHolds(
  runtime: { ownerInstanceId: string; ownerEpoch: number },
  fence: { ownerInstanceId: string; ownerEpoch: number },
): boolean {
  return runtime.ownerInstanceId === fence.ownerInstanceId &&
    runtime.ownerEpoch === fence.ownerEpoch;
}

/**
 * Decouples per-generation intake subscriptions from a single shared consumer
 * loop, so a subscription can be installed on every admitted generation while
 * the operation protocol logic stays registered once. @internal
 */
function createOperationIntakeQueue<T>(
  release?: (item: T) => void,
): AsyncIterable<T> & {
  push(value: T): void;
  close(): void;
} {
  const items: T[] = [];
  let wake: (() => void) | undefined;
  let closed = false;
  return {
    push(value) {
      if (closed) {
        // The consumer is gone: release any held pin immediately.
        release?.(value);
        return;
      }
      items.push(value);
      const resume = wake;
      wake = undefined;
      resume?.();
    },
    close() {
      closed = true;
      // Release every delivery that was accepted but never consumed.
      while (items.length > 0) release?.(items.shift() as T);
      const resume = wake;
      wake = undefined;
      resume?.();
    },
    async *[Symbol.asyncIterator]() {
      // Each delivery's held generation lease is retained through its full
      // handling and released when the consumer advances past it, unless an
      // accepted execution took ownership (which clears the lease).
      let previous: T | undefined;
      let started = false;
      const releasePrevious = () => {
        if (!started) return;
        release?.(previous as T);
        started = false;
        previous = undefined;
      };
      try {
        while (true) {
          // The consumer asked for the next item: the previous delivery's
          // handling is complete, so release its held generation pin now.
          if (items.length === 0) {
            releasePrevious();
            if (closed) return;
            await new Promise<void>((resolve) => {
              wake = resolve;
            });
          }
          while (items.length > 0) {
            releasePrevious();
            const item = items.shift() as T;
            previous = item;
            started = true;
            yield item;
          }
        }
      } finally {
        releasePrevious();
      }
    },
  };
}

/**
 * One message received by an operation intake, tagged with its receiving
 * generation and holding that generation's lease until handling completes.
 */
type OperationIntakeDelivery = {
  nc: NatsConnection;
  lease?: TransportLease;
};

/**
 * One generation's operation observation provider and the cleanup for its
 * wildcard control subscription, retained until the generation retires.
 */
type OperationLiveObservation = {
  provider: LiveProvider;
  /**
   * Ordered disposal: terminate owned sessions (publishing their signed ENDs),
   * then release retained authority. Idempotent on repeat.
   */
  close: () => Promise<void>;
};

export class TrellisServiceRuntime extends Trellis<RuntimeApi, TrellisMode> {
  #nats: NatsConnection;
  #version?: string;
  #log: LoggerLike;
  #operations = new Map<string, RuntimeOperationRecord>();
  #activeOperationFences = new Map<string, RuntimeOperationFence>();
  #runningOperationHandlers = new Set<string>();
  #operationExecutions = new Map<string, Promise<void>>();
  #operationReconciliation = new Map<
    string,
    (
      operationId: string,
      ownerEpoch?: number,
    ) => Promise<RuntimeOperationSnapshot>
  >();
  #operationTransferSessions = new Map<
    string,
    RuntimeOperationTransferSession
  >();
  #executionObservations = new Map<
    RuntimeOperationRecord,
    {
      fence: RuntimeOperationFence;
      attemptContext: Context;
      finish: (
        outcome:
          | "completed"
          | "failed"
          | "cancelled"
          | "lease_lost"
          | "interrupted"
          | "error",
      ) => void;
    }
  >();
  #operationUpdateSequences = new Map<string, number>();
  // Mounted operation-control subscriptions, keyed by control subject, with the
  // loop that serves them. Shutdown leaves this queue group and settles the
  // loops and any detached cancel task before the connection drains.
  #mountedOperationControls = new Map<
    string,
    { unsubscribe: () => void; loop: Promise<unknown> }
  >();
  #operationControlTasks = new Set<Promise<unknown>>();
  /** Coalesces concurrent per-operation control-loop installs by subject. */
  #operationControlInstalls = new Map<string, Promise<void>>();
  /**
   * Per accepted operation, the receiving generation's connection and a lease
   * that pins that generation through terminal work. Released when the
   * operation's fence is released.
   */
  #operationTransport = new Map<
    string,
    { nc: NatsConnection; lease?: TransportLease }
  >();
  /** Closes shared operation intake queues, releasing any queued delivery pins. */
  #operationIntakeClosers = new Set<() => void>();
  #operationRecoverers = new Map<
    string,
    (record: DurableOperationRecord) => Promise<void>
  >();
  #operationRecoveryIndexes = new Set<{
    nc: NatsConnection;
    records: Map<string, DurableOperationRecord>;
  }>();
  #operationRecoveryAbort = new AbortController();
  #operationRecoveryInstall?: Promise<void>;
  #operationRecoveryScan?: ReturnType<typeof setInterval>;
  #operationRecoveryTask?: Promise<void>;
  /** Drains fixed-connection intake subscriptions on stop. */
  #operationIntakeDrains = new Set<() => Promise<void>>();
  #stopPromise?: Promise<void>;
  #transferSupport?: RuntimeOperationTransferSupport;
  /**
   * Logical transport provider owning this runtime's physical generations. When
   * present, runtime lifecycle and cross-connection requests resolve through it
   * rather than the initial physical socket, which may itself be reaped.
   */
  #transport?: TrellisTransportProvider;

  /**
   * Whether the runtime's logical transport is already closed. Resolved through
   * the provider's logical lifecycle, never a raw seed socket that may itself
   * have been reaped.
   */
  #transportClosed(): boolean {
    return this.#transport?.isClosed() ?? this.#nats.isClosed();
  }
  #operationOwnerId: string;
  #operationConnectionId: string;
  #operationDeploymentId?: string;
  readonly operations: RuntimeOperationController;

  private constructor(
    name: string,
    nats: NatsConnection,
    auth: TrellisAuth,
    opts?: TrellisServiceRuntimeOpts<RuntimeApi>,
  ) {
    super(name, nats, auth, {
      ...opts,
      log: opts?.log ?? serviceRuntimeLogger,
    });
    if (opts?.operationDeploymentId) {
      this.setOperationDeploymentId(opts.operationDeploymentId);
    }

    this.#nats = nats;
    this.#transport = opts?.transport;
    this.#version = opts?.version;
    this.#log = (opts?.log ?? serviceRuntimeLogger).child({
      lib: "trellis-service-runtime",
    });
    this.#transferSupport = opts?.transferSupport;
    this.#operationOwnerId = ulid();
    this.#operationConnectionId = opts?.operationConnectionId ?? "";
    this.#operationDeploymentId = opts?.operationDeploymentId;
    this.operations = {
      get: (operationId) =>
        AsyncResult.from((async () => {
          const runtime = await this.#resolveOperation(operationId);
          if (!runtime) {
            return err(this.#operationNotFoundError(operationId));
          }
          return ok(runtime.snapshot);
        })()),
      started: (operationId) =>
        this.#applyOwnedOperationUpdate(operationId, "running", {
          event: { type: "started" },
        }),
      progress: (operationId, progress) =>
        this.#applyOwnedOperationUpdate(operationId, "running", {
          patch: { progress },
          event: { type: "progress", progress },
        }),
      complete: (operationId, output) =>
        this.#applyOwnedOperationUpdate(operationId, "completed", {
          patch: { output },
          event: { type: "completed" },
        }),
      fail: (operationId, error) =>
        this.#applyOwnedOperationUpdate(operationId, "failed", {
          patch: { error: error.toSerializable() },
          event: { type: "failed" },
        }),
      cancel: (operationId) =>
        AsyncResult.from((async () => {
          const runtime = await this.#resolveOperation(operationId);
          const fence = this.#activeOperationFences.get(operationId);
          if (!runtime || !fence) {
            return err(this.#operationNotFoundError(operationId));
          }
          return await this.#requestOwnedOperationCancellation(runtime, fence);
        })()),
      signals: (operationId) => this.#signals(operationId),
      nextSignal: (operationId, name) => this.#nextSignal(operationId, name),
    };
  }

  async *#signals(operationId: string): AsyncIterable<RuntimeOperationSignal> {
    let cursor = 0;
    while (true) {
      const next = await this.#nextSignalAfter(operationId, cursor).take();
      if (isErr(next)) {
        throw next.error;
      }
      cursor = next.sequence;
      yield next;
    }
  }

  #nextSignal(
    operationId: string,
    name?: string,
  ): AsyncResult<RuntimeOperationSignal, BaseError> {
    return AsyncResult.from((async () => {
      let cursor = 0;
      while (true) {
        const next = await this.#nextSignalAfter(operationId, cursor).take();
        if (isErr(next)) return next;
        cursor = next.sequence;
        if (!name || next.signal === name) return ok(next);
      }
    })());
  }

  #nextSignalAfter(
    operationId: string,
    afterSequence: number,
  ): AsyncResult<RuntimeOperationSignal, BaseError> {
    return AsyncResult.from((async () => {
      while (true) {
        const runtime = await this.#resolveOperation(operationId);
        if (!runtime) {
          return err(this.#operationNotFoundError(operationId));
        }

        const durable = await this.loadOperationRecord(operationId);
        if (durable && durable.revision > runtime.revision) {
          runtime.revision = durable.revision;
          runtime.snapshot = durable.snapshot;
          runtime.sequence = durable.sequence;
          runtime.signalSequence = durable.signalSequence;
          runtime.signals = durable.signals;
          runtime.terminal = durable.snapshot.state === "completed" ||
            durable.snapshot.state === "failed" ||
            durable.snapshot.state === "cancelled";
          if (durable.cancelRequestedAt) {
            runtime.cancelRequestedAt = durable.cancelRequestedAt;
          }
        }

        const queued = runtime.signals.find((signal) =>
          !signal.acknowledged && signal.sequence > afterSequence
        );
        if (queued) {
          return ok(queued);
        }
        if (runtime.terminal) {
          return err(this.#operationAlreadyTerminalError(runtime));
        }
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    })());
  }

  async #resolveOperation(
    operationId: string,
  ): Promise<RuntimeOperationRecord | null> {
    const existing = this.#operations.get(operationId);
    const durable = await this.loadOperationRecord(operationId);
    if (!durable) return null;

    if (
      existing?.ownerInstanceId === this.#operationOwnerId &&
      durable.ownerInstanceId === this.#operationOwnerId &&
      durable.ownerEpoch === existing.ownerEpoch
    ) {
      if (durable.revision > existing.revision) {
        existing.revision = durable.revision;
        existing.leaseExpiresAt = durable.leaseExpiresAt;
        existing.cancelRequestedAt = durable.cancelRequestedAt;
        existing.snapshot = durable.snapshot;
        existing.sequence = durable.sequence;
        existing.signalSequence = durable.signalSequence;
        existing.signals = durable.signals;
        existing.transferGrant = durable.transferGrant;
        existing.uploadStorageKey = durable.uploadStorageKey;
        existing.terminal = durable.snapshot.state === "completed" ||
          durable.snapshot.state === "failed" ||
          durable.snapshot.state === "cancelled";
      }
      if (existing.cancelRequestedAt || existing.terminal) {
        existing.cancellation.abort("operation cancelled");
      }
      return existing;
    }
    if (existing) {
      if (existing.ownerInstanceId === this.#operationOwnerId) {
        existing.cancellation.abort("operation superseded by another owner");
      }
      this.#operations.delete(operationId);
    }

    const cancellation = new AbortController();
    const runtime: RuntimeOperationRecord = {
      id: durable.invocationId,
      service: durable.snapshot.service,
      operation: durable.snapshot.operation,
      callerSessionKey: durable.callerSessionKey,
      invocationDigest: durable.invocationDigest,
      caller: durable.caller,
      creatorPrincipalId: durable.creatorPrincipalId,
      creatorParticipantId: durable.creatorParticipantId,
      apiId: durable.apiId,
      input: durable.input,
      ...(durable.telemetry ? { telemetry: durable.telemetry } : {}),
      revision: durable.revision,
      ownerInstanceId: durable.ownerInstanceId,
      ownerConnectionId: durable.ownerConnectionId,
      ownerEpoch: durable.ownerEpoch,
      leaseExpiresAt: durable.leaseExpiresAt,
      ...(durable.cancelRequestedAt
        ? { cancelRequestedAt: durable.cancelRequestedAt }
        : {}),
      ...(durable.transferGrant
        ? { transferGrant: durable.transferGrant }
        : {}),
      ...(durable.uploadStorageKey
        ? { uploadStorageKey: durable.uploadStorageKey }
        : {}),
      snapshot: durable.snapshot,
      sequence: durable.sequence,
      terminal: durable.snapshot.state === "completed" ||
        durable.snapshot.state === "failed" ||
        durable.snapshot.state === "cancelled",
      signalSequence: durable.signalSequence,
      signals: durable.signals,
      watchers: new Map(),
      frameQueue: Promise.resolve(),
      signalWaiters: new Set(),
      cancellation,
    };
    if (runtime.cancelRequestedAt || runtime.terminal) {
      cancellation.abort("operation cancelled");
    }
    this.#operations.set(operationId, runtime);
    return runtime;
  }

  async #recoverExpiredOperations(): Promise<void> {
    const signal = this.#operationRecoveryAbort.signal;
    if (signal.aborted) return;
    // currentNats() permits an unpublished fallback on the managed provider.
    // Recovery claims require its actual published generation instead.
    const publishedNats = () =>
      this.#transport instanceof TransportGenerationManager
        ? this.#transport.currentGeneration()?.nc
        : this.#transport?.currentNats() ?? this.#nats;
    const nc = publishedNats();
    if (!nc) return;
    const index = [...this.#operationRecoveryIndexes].find((entry) =>
      entry.nc === nc
    );
    if (!index) return;
    for (const record of [...index.records.values()]) {
      if (
        signal.aborted || publishedNats() !== nc
      ) break;
      if (Date.parse(record.leaseExpiresAt) > Date.now()) continue;
      for (const recover of this.#operationRecoverers.values()) {
        if (
          signal.aborted ||
          publishedNats() !== nc
        ) break;
        // Recovery still rereads the authoritative record and claims via CAS;
        // the watch is only an index of candidates, never an ownership fence.
        await recover(record);
      }
    }
  }

  async #ensureOperationRecovery(): Promise<void> {
    this.#operationRecoveryInstall ??= (async () => {
      const logicalStore = await this.operationStoreHandle();
      const install = async (target: { nc: NatsConnection }) => {
        const controller = new AbortController();
        const index = {
          nc: target.nc,
          records: new Map<string, DurableOperationRecord>(),
        };
        let done: Promise<void> | undefined;
        const stop = async () => {
          controller.abort();
          this.#operationRecoveryIndexes.delete(index);
          index.records.clear();
          await done;
        };
        const abort = () => {
          controller.abort();
        };
        this.#operationRecoveryAbort.signal.addEventListener("abort", abort, {
          once: true,
        });
        try {
          this.#operationRecoveryAbort.signal.throwIfAborted();
          const store = await TypedKV.open(
            fixedTransportProvider(target.nc),
            logicalStore.name,
            logicalStore.representation,
            {
              bindOnly: true,
              ttl: 30 * 24 * 60 * 60 * 1_000,
              maxValueBytes: 1024 * 1024,
            },
          ).orThrow();
          controller.signal.throwIfAborted();
          let watcher = await store.watch(undefined, {
            signal: controller.signal,
          }).orThrow();
          // Readiness is consumer establishment, not completion of retained replay.
          this.#operationRecoveryIndexes.add(index);
          done = (async () => {
            try {
              while (!controller.signal.aborted) {
                try {
                  for await (const item of watcher) {
                    if (controller.signal.aborted) break;
                    const entry = item.orThrow();
                    const record = entry.value;
                    if (
                      record &&
                      !isTerminalRuntimeOperationSnapshot(record.snapshot)
                    ) index.records.set(entry.key, record);
                    else index.records.delete(entry.key);
                  }
                } catch (error) {
                  if (!controller.signal.aborted) {
                    this.#log.warn(
                      { error },
                      "Operation recovery watch failed",
                    );
                  }
                }
                index.records.clear();
                if (controller.signal.aborted) break;
                // Re-establish an unexpectedly ended index without enumerating
                // keys. Retirement interrupts the backoff and joins this task.
                await new Promise<void>((resolve) => {
                  const finish = () => {
                    clearTimeout(timer);
                    controller.signal.removeEventListener("abort", finish);
                    resolve();
                  };
                  const timer = setTimeout(finish, 1_000);
                  controller.signal.addEventListener("abort", finish, {
                    once: true,
                  });
                  if (controller.signal.aborted) finish();
                });
                if (controller.signal.aborted) break;
                try {
                  watcher = await store.watch(undefined, {
                    signal: controller.signal,
                  }).orThrow();
                } catch (error) {
                  if (!controller.signal.aborted) {
                    this.#log.warn(
                      { error },
                      "Operation recovery watch restart failed",
                    );
                  }
                }
              }
            } finally {
              controller.abort();
              this.#operationRecoveryIndexes.delete(index);
              index.records.clear();
              this.#operationRecoveryAbort.signal.removeEventListener(
                "abort",
                abort,
              );
            }
          })();
          if (controller.signal.aborted) {
            await stop();
            controller.signal.throwIfAborted();
          }
          return { drain: stop, done, dispose: stop };
        } catch (error) {
          await stop();
          this.#operationRecoveryAbort.signal.removeEventListener(
            "abort",
            abort,
          );
          throw error;
        }
      };
      if (this.adaptiveTransport) {
        await this.declareGenerationIntake("operation-recovery", install);
      } else {
        const installed = await install({ nc: this.#nats });
        this.#operationIntakeClosers.add(() => {
          void installed.drain();
        });
        this.#operationIntakeDrains.add(installed.drain);
      }
    })();
    await this.#operationRecoveryInstall;
    const scan = () => {
      if (
        !this.#operationRecoveryTask &&
        !this.#operationRecoveryAbort.signal.aborted
      ) {
        this.#operationRecoveryTask = this.#recoverExpiredOperations().catch(
          (error) => {
            if (!this.#operationRecoveryAbort.signal.aborted) {
              this.#log.warn({ error }, "Operation recovery scan failed");
            }
          },
        ).finally(() => {
          this.#operationRecoveryTask = undefined;
        });
      }
      return this.#operationRecoveryTask;
    };
    this.#operationRecoveryScan ??= setInterval(() => {
      void scan();
    }, 1_000);
    await scan();
  }

  async #acquireOperation(
    durable: DurableOperationRecord,
  ): Promise<RuntimeOperationRecord | null> {
    if (
      durable.snapshot.state === "completed" ||
      durable.snapshot.state === "failed" ||
      durable.snapshot.state === "cancelled" ||
      Date.parse(durable.leaseExpiresAt) > Date.now()
    ) return null;

    const existing = await this.#resolveOperation(durable.invocationId);
    if (!existing || existing.revision !== durable.revision) return null;
    // Supersede any prior local execution before taking a new epoch so an
    // earlier execution's continuations cannot receive this execution's
    // cancellation or borrow its owner fence.
    if (existing.ownerInstanceId === this.#operationOwnerId) {
      existing.cancellation.abort("operation superseded by new acquisition");
      this.#operations.delete(existing.id);
    }
    const acquired = await this.#resolveOperation(durable.invocationId);
    if (!acquired || acquired.revision !== durable.revision) return null;
    const runtime = acquired;
    runtime.ownerInstanceId = this.#operationOwnerId;
    runtime.ownerConnectionId = this.#operationConnectionId;
    runtime.ownerEpoch = durable.ownerEpoch + 1;
    runtime.leaseExpiresAt = new Date(Date.now() + 30_000).toISOString();
    try {
      await this.saveOperationRecord(runtime);
    } catch (cause) {
      recordCatalogCounter("trellis.operation.ownership.events", 1, {
        "trellis.action": "recover",
        "trellis.outcome": isOperationRevisionConflict(cause)
          ? "conflict"
          : "error",
      });
      if (this.#operations.get(runtime.id) === runtime) {
        this.#operations.delete(runtime.id);
      }
      return null;
    }
    runtime.reclaimed = true;
    this.#activeOperationFences.set(runtime.id, this.#operationFence(runtime));
    recordCatalogCounter("trellis.operation.ownership.events", 1, {
      "trellis.action": "recover",
      "trellis.outcome": "ok",
    });
    return runtime;
  }

  #operationFence(runtime: RuntimeOperationRecord): RuntimeOperationFence {
    return {
      ownerInstanceId: runtime.ownerInstanceId,
      ownerConnectionId: runtime.ownerConnectionId,
      ownerEpoch: runtime.ownerEpoch,
    };
  }

  #ownsOperation(
    durable: Pick<
      DurableOperationRecord,
      "ownerInstanceId" | "ownerConnectionId" | "ownerEpoch" | "leaseExpiresAt"
    >,
    fence: RuntimeOperationFence,
  ): boolean {
    return durable.ownerInstanceId === fence.ownerInstanceId &&
      durable.ownerConnectionId === fence.ownerConnectionId &&
      durable.ownerEpoch === fence.ownerEpoch &&
      Date.parse(durable.leaseExpiresAt) > Date.now();
  }

  #releaseOperationFence(
    operationId: string,
    fence: RuntimeOperationFence,
  ): void {
    const active = this.#activeOperationFences.get(operationId);
    if (
      active?.ownerInstanceId === fence.ownerInstanceId &&
      active.ownerConnectionId === fence.ownerConnectionId &&
      active.ownerEpoch === fence.ownerEpoch
    ) {
      this.#log.debug(
        { operationId, ...fence },
        "Operation owner fence released",
      );
      this.#activeOperationFences.delete(operationId);
      this.#operationTransferSessions.delete(operationId);
      this.#releaseOperationTransport(operationId);
    } else {
      this.#log.debug({
        operationId,
        ...fence,
        activeOwnerEpoch: active?.ownerEpoch,
      }, "Operation owner fence release did not match active owner");
    }
  }

  /** Release the receiving-generation pin once an operation can no longer run. */
  #releaseOperationTransport(operationId: string): void {
    const held = this.#operationTransport.get(operationId);
    if (!held) return;
    this.#operationTransport.delete(operationId);
    held.lease?.release();
    this.#log.debug({
      operationId,
      physicalClientId: held.nc.info?.client_id,
      leased: held.lease !== undefined,
    }, "Operation execution transport pin released");
  }

  /** The connection that should carry this operation's execution support traffic. */
  #operationNats(operationId: string): NatsConnection {
    const pinned = this.#operationTransport.get(operationId)?.nc;
    if (pinned) return pinned;
    // An operation that is not pinned to the generation that accepted it (for
    // example one recovered on this process) uses the current admitted
    // generation, never the initial seed socket, which may already be reaped.
    if (this.adaptiveTransport && this.#transport) {
      return this.#transport.currentNats();
    }
    return this.#nats;
  }

  /**
   * Pin the current admitted generation for a recovered execution that has no
   * accepting-generation pin, so its support traffic cannot outlive the
   * generation that carries it. A no-op when already pinned or fixed.
   */
  async #pinRecoveredOperation(operationId: string): Promise<void> {
    if (this.#operationTransport.has(operationId)) return;
    if (!this.adaptiveTransport || !this.#transport) return;
    const lease = await this.#transport.acquireCurrent({
      deadlineMs: Date.now() + 30_000,
    });
    if (this.#operationTransport.has(operationId)) {
      lease.release();
      return;
    }
    this.#operationTransport.set(operationId, { nc: lease.nc, lease });
    this.#log.debug({
      operationId,
      ...this.#activeOperationFences.get(operationId),
      physicalClientId: lease.nc.info?.client_id,
    }, "Recovered operation execution transport pin captured");
  }

  /** The runtime's own current authorization context digest, if any. */
  #operationContextDigest(): string {
    const digest = this.auth.contextDigest;
    if (typeof digest === "function") return digest() ?? "";
    return digest ?? "";
  }

  #applyOperationUpdate(
    runtime: RuntimeOperationRecord,
    fence: RuntimeOperationFence,
    state: RuntimeOperationState,
    opts: {
      patch?: Partial<RuntimeOperationSnapshot>;
      event: Record<string, unknown> & { type: string };
      uploadCommit?: { info: FileInfo; storageKey: string };
    },
  ): AsyncResult<RuntimeOperationSnapshot, BaseError> {
    return AsyncResult.from((async () => {
      return await this.#queueOperationFrame(runtime, async () => {
        for (let retry = 0;; retry++) {
          const durable = await this.loadOperationRecord(runtime.id);
          if (!durable || !this.#ownsOperation(durable, fence)) {
            recordCatalogCounter("trellis.operation.ownership.events", 1, {
              "trellis.action": "control",
              "trellis.outcome": "lost",
            });
            runtime.cancellation.abort("operation ownership lost");
            return err(this.#operationAlreadyTerminalError(runtime));
          }
          if (
            durable.snapshot.state === "completed" ||
            durable.snapshot.state === "failed" ||
            durable.snapshot.state === "cancelled" ||
            durable.cancelRequestedAt ||
            runtime.cancellation.signal.aborted
          ) {
            recordCatalogCounter("trellis.operation.ownership.events", 1, {
              "trellis.action": "control",
              "trellis.outcome": "rejected",
            });
            return err(this.#operationAlreadyTerminalError(runtime));
          }
          runtime.revision = durable.revision;
          runtime.snapshot = durable.snapshot;
          runtime.sequence = durable.sequence + 1;
          runtime.signalSequence = durable.signalSequence;
          runtime.signals = durable.signals;
          runtime.uploadStorageKey = durable.uploadStorageKey;
          if (opts.uploadCommit) {
            if (!durable.transferGrant) {
              throw new Error("operation upload grant is missing");
            }
            runtime.transferGrant = { ...durable.transferGrant };
            Reflect.set(
              runtime.transferGrant,
              "committed",
              opts.uploadCommit.info,
            );
            runtime.uploadStorageKey = opts.uploadCommit.storageKey;
          }
          runtime.leaseExpiresAt = new Date(Date.now() + 30_000).toISOString();
          runtime.snapshot = buildRuntimeOperationSnapshot(
            runtime,
            state,
            opts.patch,
          );
          runtime.terminal = state === "completed" || state === "failed" ||
            state === "cancelled";
          try {
            await this.saveOperationRecord(runtime);
            recordCatalogCounter("trellis.operation.ownership.events", 1, {
              "trellis.action": "control",
              "trellis.outcome": "ok",
            });
            break;
          } catch (cause) {
            if (retry >= 3 || !isOperationRevisionConflict(cause)) {
              recordCatalogCounter("trellis.operation.ownership.events", 1, {
                "trellis.action": "control",
                "trellis.outcome": isOperationRevisionConflict(cause)
                  ? "conflict"
                  : "error",
              });
              runtime.cancellation.abort("operation ownership lost");
              throw cause;
            }
          }
        }

        if (runtime.terminal) {
          const observation = this.#executionObservations.get(runtime);
          if (
            observation &&
            observation.fence.ownerInstanceId === fence.ownerInstanceId &&
            observation.fence.ownerConnectionId === fence.ownerConnectionId &&
            observation.fence.ownerEpoch === fence.ownerEpoch &&
            (state === "completed" || state === "failed" ||
              state === "cancelled")
          ) observation.finish(state);
          this.#releaseOperationFence(runtime.id, fence);
          this.#rejectSignalWaiters(runtime);
        }

        return ok(runtime.snapshot);
      });
    })());
  }

  #operationUploadPersistence(
    getRuntime: () => RuntimeOperationRecord,
    getFence: () => RuntimeOperationFence,
  ) {
    type Progress = NonNullable<RuntimeOperationSnapshot["transfer"]>;
    let latest: Progress | undefined;
    let persistedBytes = 0;
    let pendingSince: number | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let worker: Promise<void> | undefined;
    let failure: unknown;
    let stopped = false;
    const drain = () => {
      if (worker || stopped || failure) return;
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
      worker = (async () => {
        while (!stopped && latest && latest.transferredBytes > persistedBytes) {
          const remaining = 250 -
            (performance.now() - (pendingSince ?? performance.now()));
          if (
            latest.transferredBytes - persistedBytes < 1024 * 1024 &&
            remaining > 0
          ) {
            timer = setTimeout(drain, remaining);
            return;
          }
          const progress = latest;
          pendingSince = undefined;
          const result = await this.#applyOperationUpdate(
            getRuntime(),
            getFence(),
            "pending",
            {
              patch: { transfer: progress },
              event: { type: "transfer", transfer: progress },
            },
          ).take();
          if (isErr(result)) throw result.error;
          persistedBytes = progress.transferredBytes;
          this.#log.debug({
            operationId: getRuntime().id,
            transferredBytes: persistedBytes,
            revision: result.revision,
          }, "Operation upload progress checkpoint written");
          if (latest.transferredBytes <= persistedBytes) {
            pendingSince = undefined;
          }
        }
      })().catch((cause) => {
        failure = cause;
        this.#log.error(
          { cause, operationId: getRuntime().id },
          "Operation upload progress persistence failed",
        );
        getRuntime().cancellation.abort(
          "operation upload progress persistence failed",
        );
      }).finally(() => {
        worker = undefined;
        if (
          !stopped && latest && latest.transferredBytes > persistedBytes &&
          (timer === undefined ||
            latest.transferredBytes - persistedBytes >= 1024 * 1024)
        ) drain();
      });
    };
    const stop = () => {
      stopped = true;
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    };
    let listening = false;
    return {
      onProgress: (progress: Progress) => {
        this.#log.debug({
          operationId: getRuntime().id,
          transferredBytes: progress.transferredBytes,
          chunkIndex: progress.chunkIndex,
        }, "Operation upload consumed progress submitted");
        if (
          stopped ||
          latest && progress.transferredBytes <= latest.transferredBytes
        ) return;
        if (!listening) {
          getRuntime().cancellation.signal.addEventListener("abort", stop, {
            once: true,
          });
          listening = true;
        }
        latest = progress;
        pendingSince ??= performance.now();
        drain();
      },
      commit: async (
        stored: StoredTransfer,
        progress: NonNullable<RuntimeOperationSnapshot["transfer"]>,
      ) => {
        stop();
        await worker;
        getRuntime().cancellation.signal.removeEventListener("abort", stop);
        if (failure) throw failure;
        if (progress.transferredBytes !== stored.info.size) {
          throw new Error(
            "final operation upload progress does not match stored size",
          );
        }
        // One fenced CAS flushes exact progress, the commit metadata, and the
        // handler-runnable snapshot before the transfer sends committed.
        const result = await this.#applyOperationUpdate(
          getRuntime(),
          getFence(),
          "running",
          {
            patch: { transfer: progress },
            event: { type: "transfer", transfer: progress },
            uploadCommit: { info: stored.info, storageKey: stored.storageKey },
          },
        ).take();
        if (isErr(result)) throw result.error;
      },
      onError: async (_error: TransferError) => {
        stop();
        await worker;
        const runtime = getRuntime();
        runtime.cancellation.signal.removeEventListener("abort", stop);
        await this.#queueOperationFrame(runtime, async () => {
          const durable = await this.loadOperationRecord(runtime.id);
          if (
            !durable || !this.#ownsOperation(durable, getFence()) ||
            isTerminalRuntimeOperationSnapshot(durable.snapshot) ||
            durable.cancelRequestedAt ||
            durable.transferGrant &&
              Value.Check(
                FileInfoSchema,
                Reflect.get(durable.transferGrant, "committed"),
              )
          ) return;
          runtime.revision = durable.revision;
          runtime.snapshot = durable.snapshot;
          runtime.transferGrant = durable.transferGrant;
          runtime.uploadStorageKey = durable.uploadStorageKey;
          runtime.sequence = durable.sequence;
          runtime.signalSequence = durable.signalSequence;
          runtime.signals = durable.signals;
          runtime.leaseExpiresAt = new Date().toISOString();
          await this.saveOperationRecord(runtime);
          runtime.cancellation.abort("operation upload interrupted");
          this.#releaseOperationFence(runtime.id, getFence());
        });
      },
      reconcileCommit: async (storageKey: string, transferId: string) => {
        const runtime = getRuntime();
        const durable = await this.loadOperationRecord(runtime.id);
        if (!durable) return undefined;
        const committed = durable.transferGrant &&
          Reflect.get(durable.transferGrant, "committed");
        if (
          durable.uploadStorageKey === storageKey &&
          durable.transferGrant?.transferId === transferId &&
          Value.Check(FileInfoSchema, committed)
        ) return true;
        // An unchanged read cannot rule out an in-flight CAS. Only a newer
        // uncommitted revision fences that write; unknown/corrupt commit state
        // retains bytes rather than deleting a potentially committed object.
        return committed === undefined && durable.revision > runtime.revision
          ? false
          : undefined;
      },
    };
  }

  #applyOwnedOperationUpdate(
    operationId: string,
    state: RuntimeOperationState,
    opts: {
      patch?: Partial<RuntimeOperationSnapshot>;
      event: Record<string, unknown> & { type: string };
    },
  ): AsyncResult<RuntimeOperationSnapshot, BaseError> {
    return AsyncResult.from((async () => {
      const runtime = await this.#resolveOperation(operationId);
      if (!runtime) return err(this.#operationNotFoundError(operationId));
      const fence = this.#activeOperationFences.get(operationId);
      if (!fence) return err(this.#operationNotFoundError(operationId));
      return await this.#applyOperationUpdate(
        runtime,
        fence,
        state,
        opts,
      );
    })());
  }

  async #queueOperationFrame<T>(
    runtime: RuntimeOperationRecord,
    task: () => Promise<T>,
  ): Promise<T> {
    const previous = runtime.frameQueue;
    let release: (() => void) | undefined;
    runtime.frameQueue = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous;
    try {
      return await task();
    } finally {
      release?.();
    }
  }

  async #requestOperationCancellation(
    runtime: RuntimeOperationRecord,
    admission = false,
  ): Promise<RuntimeOperationSnapshot> {
    // Join the same per-operation frame queue as progress/complete/fail so a
    // cancellation request and a handler completion cannot interleave on the
    // shared mutable record and persist a logically inconsistent combination.
    await this.#queueOperationFrame(runtime, async () => {
      for (let retry = 0;; retry++) {
        const durable = await this.loadOperationRecord(runtime.id);
        if (!durable) throw this.#operationNotFoundError(runtime.id);
        runtime.revision = durable.revision;
        runtime.snapshot = durable.snapshot;
        runtime.terminal = isTerminalRuntimeOperationSnapshot(durable.snapshot);
        if (runtime.terminal) {
          if (!admission && !durable.cancelRequestedAt) {
            throw this.#operationAlreadyTerminalError(runtime);
          }
          runtime.cancelRequestedAt = durable.cancelRequestedAt;
          break;
        }
        if (durable.cancelRequestedAt) {
          runtime.cancelRequestedAt = durable.cancelRequestedAt;
          break;
        }
        runtime.sequence = durable.sequence + 1;
        runtime.signalSequence = durable.signalSequence;
        runtime.signals = durable.signals;
        runtime.ownerInstanceId = durable.ownerInstanceId;
        runtime.ownerConnectionId = durable.ownerConnectionId;
        runtime.ownerEpoch = durable.ownerEpoch;
        runtime.transferGrant = durable.transferGrant;
        runtime.leaseExpiresAt = durable.leaseExpiresAt;
        runtime.cancelRequestedAt = new Date().toISOString();
        runtime.snapshot = buildRuntimeOperationSnapshot(
          runtime,
          runtime.snapshot.state,
          { updatedAt: runtime.cancelRequestedAt },
        );
        try {
          await this.saveOperationRecord(runtime);
          recordCatalogCounter("trellis.operation.ownership.events", 1, {
            "trellis.action": "control",
            "trellis.outcome": "ok",
          });
          break;
        } catch (cause) {
          if (retry >= 3 || !isOperationRevisionConflict(cause)) throw cause;
        }
      }
    });
    runtime.cancellation.abort("operation cancelled");
    const durable = await this.loadOperationRecord(runtime.id);
    if (!durable) throw this.#operationNotFoundError(runtime.id);
    return durable.snapshot;
  }

  #requestOwnedOperationCancellation(
    runtime: RuntimeOperationRecord,
    fence: RuntimeOperationFence,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError> {
    return AsyncResult.from((async () => {
      const durable = await this.loadOperationRecord(runtime.id);
      if (
        !durable || this.#activeOperationFences.get(runtime.id) !== fence ||
        !this.#ownsOperation(durable, fence)
      ) return err(this.#operationNotFoundError(runtime.id));
      try {
        return ok(await this.#requestOperationCancellation(runtime));
      } catch (cause) {
        if (cause instanceof OperationAlreadyTerminalError) return err(cause);
        return err(new UnexpectedError({ cause }));
      }
    })());
  }

  async #finalizeOperationCancellation(
    runtime: RuntimeOperationRecord,
    fence: RuntimeOperationFence,
  ): Promise<void> {
    await this.#queueOperationFrame(runtime, async () => {
      if (this.#runningOperationHandlers.has(runtime.id)) return;
      for (let retry = 0;; retry++) {
        const durable = await this.loadOperationRecord(runtime.id);
        if (
          !durable || !this.#ownsOperation(durable, fence) ||
          !durable.cancelRequestedAt ||
          isTerminalRuntimeOperationSnapshot(durable.snapshot)
        ) return;
        runtime.revision = durable.revision;
        runtime.cancelRequestedAt = durable.cancelRequestedAt;
        runtime.sequence = durable.sequence + 1;
        runtime.leaseExpiresAt = new Date(Date.now() + 30_000).toISOString();
        runtime.snapshot = buildRuntimeOperationSnapshot(runtime, "cancelled", {
          completedAt: new Date().toISOString(),
        });
        runtime.terminal = true;
        try {
          await this.saveOperationRecord(runtime);
          this.#log.debug({
            operationId: runtime.id,
            ...fence,
            revision: runtime.snapshot.revision,
          }, "Operation cancellation durably finalized");
          recordCatalogDuration(
            "trellis.operation.cancellation.cleanup.duration",
            Math.max(0, Date.now() - Date.parse(durable.cancelRequestedAt)),
            { "trellis.route": routeToken("operation", runtime.operation) },
          );
          this.#releaseOperationFence(runtime.id, fence);
          this.#rejectSignalWaiters(runtime);
          this.#executionObservations.get(runtime)?.finish("cancelled");
          return;
        } catch (cause) {
          if (retry >= 3 || !isOperationRevisionConflict(cause)) throw cause;
        }
      }
    });
  }

  #emitOperationUpdate(
    runtime: RuntimeOperationRecord,
    fence: RuntimeOperationFence,
    ctx: RegisteredRuntimeOperationDesc,
    update: unknown,
  ): Promise<Result<void, BaseError>> {
    return this.#queueOperationFrame(runtime, async () => {
      const durable = await this.loadOperationRecord(runtime.id);
      if (
        runtime.terminal || runtime.cancellation.signal.aborted || !durable ||
        durable.cancelRequestedAt || durable.snapshot.state === "cancelled" ||
        !this.#ownsOperation(durable, fence)
      ) {
        runtime.cancellation.abort("operation cancelled or ownership lost");
        return err(this.#operationAlreadyTerminalError(runtime));
      }
      if (ctx.update === undefined) {
        return err(
          new ValidationError({
            errors: [{
              path: "/",
              message:
                `Operation '${runtime.operation}' does not declare updates`,
            }],
            context: { operation: runtime.operation },
          }),
        );
      }
      const parsed = this.#encodeOperationValue(ctx, "update", update).take();
      if (isErr(parsed)) return parsed;

      const key = `${runtime.id}:${fence.ownerEpoch}`;
      const sequence = (this.#operationUpdateSequences.get(key) ?? 0) + 1;
      this.#operationUpdateSequences.set(key, sequence);
      const metadata = headers();
      const occurredAt = new Date().toISOString();
      const subject = `${ctx.subject}.updates.${runtime.id}`;
      const payload = JSON.stringify({
        operationId: runtime.id,
        apiId: runtime.apiId,
        operation: runtime.operation,
        deploymentId: this.#operationDeploymentId,
        ownerExecutorId: fence.ownerInstanceId,
        ownerConnectionId: runtime.ownerConnectionId,
        ownerEpoch: String(fence.ownerEpoch),
        sequence: String(sequence),
        occurredAt,
        update: parsed,
      });
      const contextDigest = typeof this.auth.contextDigest === "function"
        ? this.auth.contextDigest()
        : this.auth.contextDigest;
      const cache = this.auth.authorizationProviderCache;
      if (!contextDigest || !cache) {
        return err(new AuthError({ reason: "authorization_unavailable" }));
      }
      const ownContext = await cache.resolveContext(contextDigest);
      const reply = createInbox(ownContext.context.inboxPrefix);
      const authorization = await this.createRequestProof(
        subject,
        payload,
        reply,
      );
      metadata.set("proof", authorization.proof);
      metadata.set("iat", String(authorization.iat));
      metadata.set("request-id", authorization.requestId);
      metadata.set("authorization-context", authorization.contextDigest);
      metadata.set("session-key", this.auth.sessionKey);
      this.#log.info({
        operationId: runtime.id,
        operation: runtime.operation,
        ownerExecutorId: fence.ownerInstanceId,
        ownerConnectionId: runtime.ownerConnectionId,
        ownerEpoch: fence.ownerEpoch,
        sequence,
        subject,
        reply,
      }, "Publishing transient operation update");
      // Publish on the receiving generation so an observation opened on that
      // generation keeps seeing transient updates after a newer generation
      // takes over new intake.
      const transportNats = this.#operationNats(runtime.id);
      await transportNats.publish(
        subject,
        payload,
        { headers: metadata, reply },
      );
      await transportNats.flush();
      this.#log.info({
        operationId: runtime.id,
        ownerEpoch: fence.ownerEpoch,
        sequence,
      }, "Published transient operation update");
      return ok(undefined);
    });
  }

  #encodeOperationValue(
    ctx: RegisteredRuntimeOperationDesc,
    kind: "progress" | "update" | "output",
    value: unknown,
  ): Result<unknown, BaseError> {
    const schema = kind === "progress"
      ? ctx.progress
      : kind === "update"
      ? ctx.update
      : ctx.output;
    if (schema === undefined) return ok(value);
    try {
      if (
        schema && typeof schema === "object" &&
        typeof Reflect.get(schema, "encode") === "function"
      ) {
        return ok(Reflect.get(schema, "encode").call(schema, value));
      }
      const encoded = encodeSchema(
        schema as Parameters<typeof encodeSchema>[0],
        value,
      ).take();
      return isErr(encoded) ? encoded : ok(JSON.parse(encoded));
    } catch (cause) {
      return err(new UnexpectedError({ cause }));
    }
  }

  #decodeOperationValue(
    ctx: RegisteredRuntimeOperationDesc,
    kind: "progress" | "update" | "output",
    value: unknown,
  ): Result<unknown, BaseError> {
    const schema = kind === "progress"
      ? ctx.progress
      : kind === "update"
      ? ctx.update
      : ctx.output;
    if (schema === undefined) return ok(value);
    try {
      if (
        schema && typeof schema === "object" &&
        typeof Reflect.get(schema, "decode") === "function"
      ) {
        return ok(Reflect.get(schema, "decode").call(schema, value));
      }
      return parseSchema(
        schema as Parameters<typeof parseSchema>[0],
        value as JsonValue,
      );
    } catch (cause) {
      return err(new UnexpectedError({ cause }));
    }
  }

  #applyControlledOperationUpdate(
    runtime: RuntimeOperationRecord,
    fence: RuntimeOperationFence,
    ctx: RegisteredRuntimeOperationDesc,
    state: RuntimeOperationState,
    opts: {
      patch?: Partial<RuntimeOperationSnapshot>;
      event: Record<string, unknown> & { type: string };
    },
  ): AsyncResult<RuntimeOperationSnapshot, BaseError> {
    return AsyncResult.from((async () => {
      if (!operationOwnerFenceHolds(runtime, fence)) {
        recordCatalogCounter("trellis.operation.ownership.events", 1, {
          "trellis.action": "control",
          "trellis.outcome": "lost",
        });
        return err(
          new UnexpectedError({
            cause: new Error("operation update rejected by owner fence"),
          }),
        );
      }
      runtime.leaseExpiresAt = new Date(Date.now() + 30_000).toISOString();
      if (opts.patch?.progress !== undefined) {
        const parsed = this.#encodeOperationValue(
          ctx,
          "progress",
          opts.patch.progress,
        ).take();
        if (isErr(parsed)) return parsed;
        opts.patch.progress = parsed;
        if ("progress" in opts.event) opts.event.progress = parsed;
      }
      if (opts.patch?.output !== undefined) {
        const parsed = this.#encodeOperationValue(
          ctx,
          "output",
          opts.patch.output,
        ).take();
        if (isErr(parsed)) return parsed;
        opts.patch.output = parsed;
      }
      return await this.#applyOperationUpdate(
        runtime,
        fence,
        state,
        opts,
      );
    })());
  }

  #controlOperation(
    operation: string,
    ctx: RegisteredRuntimeOperationDesc,
    operationId: string,
  ): AsyncResult<
    OperationRuntimeHandle<unknown, unknown, BaseError>,
    BaseError
  > {
    return AsyncResult.from((async () => {
      const runtime = await this.#resolveOperation(operationId);
      if (!runtime) {
        return err(this.#operationNotFoundError(operationId));
      }
      if (!this.#matchesOperationRoute(runtime, operation, ctx)) {
        return err(this.#operationNotFoundError(operationId));
      }
      const fence = this.#activeOperationFences.get(operationId);
      if (!fence) return err(this.#operationNotFoundError(operationId));
      return ok(this.#makeControlledOperation(runtime, fence, ctx));
    })());
  }

  #makeControlledOperation(
    runtime: RuntimeOperationRecord,
    fence: RuntimeOperationFence,
    ctx: RegisteredRuntimeOperationDesc,
  ): OperationRuntimeHandle<unknown, unknown, BaseError> {
    return {
      id: runtime.id,
      started: () =>
        this.#applyControlledOperationUpdate(runtime, fence, ctx, "running", {
          event: { type: "started" },
        }),
      progress: (value: unknown) =>
        this.#applyControlledOperationUpdate(runtime, fence, ctx, "running", {
          patch: { progress: value },
          event: { type: "progress", progress: value },
        }),
      emitUpdate: (value: unknown) =>
        AsyncResult.from(this.#emitOperationUpdate(runtime, fence, ctx, value)),
      complete: (value: unknown) =>
        this.#applyControlledOperationUpdate(runtime, fence, ctx, "completed", {
          patch: { output: value },
          event: { type: "completed" },
        }),
      fail: (error: BaseError) =>
        this.#applyControlledOperationUpdate(runtime, fence, ctx, "failed", {
          patch: { error: error.toSerializable() },
          event: { type: "failed" },
        }),
      cancel: () => this.#requestOwnedOperationCancellation(runtime, fence),
      attach: (job: { wait(): AsyncResult<unknown, BaseError> }) =>
        AsyncResult.from((async () => {
          const waited = await job.wait();
          const waitedValue = waited.take();
          if (isErr(waitedValue)) {
            return err(new UnexpectedError({ cause: waitedValue.error }));
          }

          const finalRuntime = await this.#resolveOperation(runtime.id);
          if (!finalRuntime || !finalRuntime.terminal) {
            return err(
              new UnexpectedError({
                cause: new Error(
                  "attached job completed without terminal operation state",
                ),
              }),
            );
          }

          return ok(finalRuntime.snapshot);
        })()),
      signals: () => this.#signals(runtime.id),
      nextSignal: (name?: string) => this.#nextSignal(runtime.id, name),
      acknowledgeSignal: (sequence: number) =>
        this.#acknowledgeSignal(runtime, fence, sequence),
      defer: () => ({ kind: "deferred" as const }),
    };
  }

  #operationNotFoundError(operationId: string): OperationNotFoundError {
    return new OperationNotFoundError({ operationId });
  }

  #operationAlreadyTerminalError(
    runtime: RuntimeOperationRecord,
  ): OperationAlreadyTerminalError {
    return new OperationAlreadyTerminalError({
      operationId: runtime.id,
      state: runtime.snapshot.state,
      operation: runtime.operation,
      service: runtime.service,
    });
  }

  #matchesOperationRoute(
    runtime: RuntimeOperationRecord | DurableOperationRecord,
    operation: string,
    ctx: RegisteredRuntimeOperationDesc,
    caller?: VerifiedCaller,
  ): boolean {
    const invocationId = "invocationId" in runtime
      ? runtime.invocationId
      : runtime.id;
    const apiId = `${ctx.permissions?.invoke.apiId ?? ""}@${
      ctx.permissions?.invoke.apiVersion ?? ""
    }`;
    return runtime.apiId === apiId && runtime.operation === operation &&
      runtime.snapshot.id === invocationId &&
      runtime.snapshot.operation === operation &&
      operationObserveAuthorized(runtime, caller);
  }

  #rejectSignalWaiters(runtime: RuntimeOperationRecord): void {
    const result = err(this.#operationAlreadyTerminalError(runtime));
    for (const waiter of runtime.signalWaiters) {
      waiter(result);
    }
    runtime.signalWaiters.clear();
  }

  async #acceptSignal(
    runtime: RuntimeOperationRecord,
    ctx: RegisteredRuntimeOperationDesc,
    control: Extract<RuntimeOperationControlRequest, { action: "signal" }>,
    requestId: string,
    retry = 0,
  ): Promise<
    Result<{
      kind: "signal-accepted";
      operationId: string;
      signal: string;
      signalSequence: number;
      acceptedAt: string;
      snapshot: RuntimeOperationSnapshot;
    }, BaseError>
  > {
    if (runtime.terminal) {
      return err(this.#operationAlreadyTerminalError(runtime));
    }
    const replay = runtime.signals.find((signal) =>
      signal.requestId === requestId
    );
    if (replay) {
      if (
        replay.signal !== control.signal ||
        canonicalizeJson((replay.input ?? null) as JsonValue) !==
          canonicalizeJson((control.input ?? null) as JsonValue)
      ) {
        return err(
          new ValidationError({
            errors: [{
              path: "/requestId",
              message:
                "Signal request id was already accepted with different input",
            }],
          }),
        );
      }
      return ok({
        kind: "signal-accepted",
        operationId: runtime.id,
        signal: replay.signal,
        signalSequence: replay.sequence,
        acceptedAt: replay.acceptedAt,
        snapshot: runtime.snapshot,
      });
    }
    if (runtime.signals.length >= 100) {
      return err(
        new ValidationError({
          errors: [{
            path: "/signal",
            message: "Operation signal limit exceeded",
          }],
        }),
      );
    }

    const descriptor = ctx.signals?.[control.signal];
    if (!descriptor) {
      return err(
        new ValidationError({
          errors: [{
            path: "/signal",
            message: `Unknown operation signal '${control.signal}'`,
          }],
          context: { operation: runtime.operation, signal: control.signal },
        }),
      );
    }

    const input = control.input as JsonValue;
    const parsed = parseSchema(
      descriptor.input as Parameters<typeof parseSchema>[0],
      input,
    ).take();
    if (isErr(parsed)) {
      return err(parsed.error as ValidationError | UnexpectedError);
    }

    const encodedInput = new TextEncoder().encode(
      JSON.stringify(control.input ?? null),
    ).byteLength;
    if (encodedInput > 64 * 1024) {
      return err(
        new ValidationError({
          errors: [{
            path: "/input",
            message: "Operation signal payload exceeds 64 KiB",
          }],
        }),
      );
    }

    runtime.signalSequence += 1;
    const acceptedAt = new Date().toISOString();
    const signal: RuntimeOperationSignal = {
      operationId: runtime.id,
      sequence: runtime.signalSequence,
      requestId,
      signal: control.signal,
      ...(control.input !== undefined ? { input: control.input } : {}),
      acceptedAt,
      acknowledged: false,
    };
    runtime.signals.push(signal);
    try {
      await this.saveOperationRecord(runtime);
    } catch (cause) {
      this.#operations.delete(runtime.id);
      if (retry < 3 && isOperationRevisionConflict(cause)) {
        const current = await this.#resolveOperation(runtime.id);
        if (current) {
          return await this.#acceptSignal(
            current,
            ctx,
            control,
            requestId,
            retry + 1,
          );
        }
      }
      recordCatalogCounter("trellis.operation.ownership.events", 1, {
        "trellis.action": "control",
        "trellis.outcome": isOperationRevisionConflict(cause)
          ? "conflict"
          : "error",
      });
      throw cause;
    }
    recordCatalogCounter("trellis.operation.ownership.events", 1, {
      "trellis.action": "control",
      "trellis.outcome": "ok",
    });
    const result = ok(signal);
    for (const waiter of runtime.signalWaiters) {
      waiter(result);
    }

    return ok({
      kind: "signal-accepted",
      operationId: runtime.id,
      signal: signal.signal,
      signalSequence: signal.sequence,
      acceptedAt,
      snapshot: runtime.snapshot,
    });
  }

  #acknowledgeSignal(
    runtime: RuntimeOperationRecord,
    fence: RuntimeOperationFence,
    sequence: number,
  ): AsyncResult<void, BaseError> {
    return AsyncResult.from((async () => {
      return await this.#queueOperationFrame(runtime, async () => {
        for (let retry = 0;; retry++) {
          const durable = await this.loadOperationRecord(runtime.id);
          if (
            !durable || !this.#ownsOperation(durable, fence) ||
            durable.snapshot.state === "completed" ||
            durable.snapshot.state === "failed" ||
            durable.snapshot.state === "cancelled" || durable.cancelRequestedAt
          ) {
            runtime.cancellation.abort("operation ownership lost");
            return err(this.#operationAlreadyTerminalError(runtime));
          }
          runtime.revision = durable.revision;
          runtime.snapshot = durable.snapshot;
          runtime.sequence = durable.sequence;
          runtime.signalSequence = durable.signalSequence;
          runtime.signals = durable.signals;
          const signal = runtime.signals.find((item) =>
            item.sequence === sequence
          );
          if (!signal) {
            return err(
              new ValidationError({
                errors: [{
                  path: "/sequence",
                  message: `Operation signal ${sequence} was not found`,
                }],
              }),
            );
          }
          if (signal.acknowledged) return ok(undefined);
          signal.acknowledged = true;
          try {
            await this.saveOperationRecord(runtime);
            recordCatalogCounter("trellis.operation.ownership.events", 1, {
              "trellis.action": "control",
              "trellis.outcome": "ok",
            });
            return ok(undefined);
          } catch (cause) {
            if (retry >= 3 || !isOperationRevisionConflict(cause)) {
              recordCatalogCounter("trellis.operation.ownership.events", 1, {
                "trellis.action": "control",
                "trellis.outcome": isOperationRevisionConflict(cause)
                  ? "conflict"
                  : "error",
              });
              runtime.cancellation.abort("operation ownership lost");
              throw cause;
            }
          }
        }
      });
    })());
  }

  async #authenticateOperationMessage(
    msg: Msg,
    ctx: RegisteredRuntimeOperationDesc,
    parseInput: boolean,
    permission: PermissionAtom | undefined = ctx.permissions?.invoke,
    requiredCapabilities: readonly string[] = ctx.callerCapabilities ?? [],
  ): Promise<
    Result<{
      input: unknown;
      invocationId?: string;
      cancellationRequested?: boolean;
      caller: VerifiedCaller;
      sessionKey: string;
    }, UnexpectedError | AuthError | ValidationError>
  > {
    const jsonData = safeJson(msg).take();
    if (isErr(jsonData)) return jsonData;

    let parsedInput: unknown;
    let invocationId: string | undefined;
    let cancellationRequested = false;
    if (parseInput) {
      if (
        !jsonData || typeof jsonData !== "object" || Array.isArray(jsonData) ||
        typeof (jsonData as Record<string, unknown>).invocationId !==
          "string" ||
        !/^[0-9A-HJKMNP-TV-Z]{26}$/u.test(
          (jsonData as Record<string, string>).invocationId,
        ) || !("input" in jsonData)
      ) {
        return err(
          new ValidationError({
            errors: [{
              path: "",
              message: "Operation start requires a ULID invocationId and input",
            }],
          }),
        );
      }
      invocationId = (jsonData as Record<string, string>).invocationId;
      const cancellation = Reflect.get(jsonData, "cancellationRequested");
      if (cancellation !== undefined && typeof cancellation !== "boolean") {
        return err(
          new ValidationError({
            errors: [{
              path: "/cancellationRequested",
              message: "Cancellation intent must be a boolean",
            }],
          }),
        );
      }
      cancellationRequested = cancellation === true;
      const parsedInputResult = parseSchema(
        ctx.input as Parameters<typeof parseSchema>[0],
        (jsonData as Record<string, JsonValue>).input,
      ).take();
      if (isErr(parsedInputResult)) {
        return err(
          parsedInputResult.error as ValidationError | UnexpectedError,
        );
      }
      parsedInput = parsedInputResult;
    } else {
      parsedInput = jsonData;
    }

    const auth = await verifyLocalAuthorization({
      kind: "request",
      cache: this.auth.authorizationProviderCache,
      message: msg,
      permission,
      requiredCapabilities,
    });
    const authValue = auth.take();
    if (isErr(authValue)) return err(authValue.error);
    if (cancellationRequested) {
      const cancellationAuth = await verifyLocalAuthorization({
        kind: "request",
        cache: this.auth.authorizationProviderCache,
        message: msg,
        permission: ctx.permissions?.cancel,
        requiredCapabilities: ctx.cancelCapabilities ?? [],
      });
      const cancellationValue = cancellationAuth.take();
      if (isErr(cancellationValue)) return err(cancellationValue.error);
    }

    return ok({
      input: parsedInput,
      ...(invocationId ? { invocationId } : {}),
      cancellationRequested,
      caller: authValue,
      sessionKey: authValue.sessionKey,
    });
  }

  async #ensureOperationControlLoop(
    operation: string,
    ctx: RegisteredRuntimeOperationDesc,
  ): Promise<void> {
    const controlSubject = `${ctx.subject}.control`;
    if (this.#mountedOperationControls.has(controlSubject)) {
      return;
    }
    const pending = this.#operationControlInstalls.get(controlSubject);
    if (pending) {
      await pending;
      return;
    }
    // Coalesce every concurrent install synchronously, before the first await,
    // so a second caller cannot build a duplicate intake queue, subscription, or
    // observation provider. A failed attempt clears the slot so a later caller
    // can retry.
    const done = Promise.withResolvers<void>();
    this.#operationControlInstalls.set(controlSubject, done.promise);
    try {
      const respondControlError = (msg: Msg, error: Error | BaseError) => {
        const trellisError = error instanceof BaseError
          ? error
          : new UnexpectedError({ cause: error });
        recordOperationServiceError(trellisError, {
          operation,
          phase: "control",
        });
        msg.respond(JSON.stringify({
          kind: "error",
          error: trellisError.toSerializable(),
        }));
      };

      // Per-generation control intake: the control subscription is installed on
      // every admitted generation with a stable queue group, and its live
      // observation provider is built on that same receiving connection from the
      // generation's exact admitted context. Each control delivery holds that
      // generation's lease through handling; observation sessions take their own
      // pin through the provider host and keep the wildcard control subscription
      // alive until the generation physically retires.
      const controlIntake = createOperationIntakeQueue<{
        msg: Msg;
        providerPromise: Promise<OperationLiveObservation | undefined>;
        lease?: TransportLease;
      }>((delivery) => delivery.lease?.release());
      this.#operationIntakeClosers.add(() => controlIntake.close());
      const installControl = async (target: {
        id: number;
        nc: NatsConnection;
        contextDigest: string;
        lease?: () => TransportLease;
      }): Promise<{
        drain: () => Promise<void>;
        done: Promise<void>;
        dispose: () => Promise<void>;
      }> => {
        const controlSub = target.nc.subscribe(controlSubject, {
          queue: routeQueueGroup(controlSubject),
        });
        // Await full readiness before returning so the candidate cannot activate
        // with a half-installed control intake (the barrier flushes after this).
        let liveObservation: OperationLiveObservation | undefined;
        try {
          liveObservation = await this.#createOperationLiveProvider(ctx, {
            nc: target.nc,
            lease: target.lease,
          });
        } catch (error) {
          // A generation-owned candidate must not activate with a half-built
          // observation provider. Retire the already-created control subscription
          // (and any wildcard controls the provider created before failing) and
          // rethrow so the candidate fails; a fixed connection keeps the
          // historical tolerant behavior.
          if (this.adaptiveTransport) {
            await controlSub.drain().catch(() => undefined);
            throw error;
          }
          this.#log.warn(
            { error, operation: String(operation) },
            "Operation live observation unavailable",
          );
        }
        const providerPromise = Promise.resolve(liveObservation);
        const pump = (async () => {
          for await (const msg of controlSub) {
            let lease: TransportLease | undefined;
            if (target.lease) {
              try {
                lease = target.lease();
              } catch {
                respondControlError(
                  msg,
                  new UnexpectedError({
                    cause: new Error(
                      "operation control generation unavailable",
                    ),
                  }),
                );
                continue;
              }
            }
            controlIntake.push({
              msg,
              providerPromise,
              ...(lease ? { lease } : {}),
            });
          }
        })();
        return {
          drain: () => controlSub.drain().catch(() => undefined),
          done: pump.then(() => undefined),
          dispose: async () => {
            await liveObservation?.close();
          },
        };
      };
      const controlLoop = (async () => {
        for await (const delivery of controlIntake) {
          const { msg, providerPromise } = delivery;
          const liveObservation = await providerPromise;
          const liveProvider = liveObservation?.provider;
          const request = safeJson(msg).take();
          if (isErr(request)) {
            respondControlError(msg, request.error);
            continue;
          }

          if (
            !request ||
            typeof request !== "object" ||
            !["get", "watch", "signal", "cancel"].includes(
              String((request as RuntimeOperationControlRequest).action),
            ) ||
            typeof (request as RuntimeOperationControlRequest).operationId !==
              "string" ||
            ((request as RuntimeOperationControlRequest).action === "signal" &&
              typeof (request as { signal?: unknown }).signal !== "string")
          ) {
            respondControlError(
              msg,
              new UnexpectedError({
                cause: new Error("Invalid operation control request"),
              }),
            );
            continue;
          }

          const control = request as RuntimeOperationControlRequest;
          if (control.action === "signal" && !ctx.signals?.[control.signal]) {
            respondControlError(
              msg,
              new ValidationError({
                errors: [{
                  path: "/signal",
                  message: `Unknown operation signal '${control.signal}'`,
                }],
              }),
            );
            continue;
          }
          let permission = ctx.permissions?.observe;
          let capabilities = ctx.observeCapabilities ?? [];
          if (control.action === "cancel") {
            permission = ctx.permissions?.cancel;
            capabilities = ctx.cancelCapabilities ?? [];
          } else if (control.action === "signal") {
            permission = ctx.permissions?.control[control.signal];
            capabilities = ctx.controlCapabilities ?? [];
          }
          const validated = await this.#authenticateOperationMessage(
            msg,
            ctx,
            false,
            permission,
            capabilities,
          );
          const value = validated.take();
          if (isErr(value)) {
            respondControlError(msg, value.error);
            continue;
          }

          let runtime: RuntimeOperationRecord | null;
          try {
            runtime = await this.#resolveOperation(control.operationId);
          } catch (cause) {
            respondControlError(
              msg,
              cause instanceof Error ? cause : new Error(String(cause)),
            );
            continue;
          }
          if (!runtime) {
            respondControlError(
              msg,
              this.#operationNotFoundError(control.operationId),
            );
            continue;
          }

          if (
            !this.#matchesOperationRoute(runtime, operation, ctx, value.caller)
          ) {
            respondControlError(
              msg,
              this.#operationNotFoundError(control.operationId),
            );
            continue;
          }
          if (control.action === "watch") {
            const opening = parseOperationWatchOpen(request);
            if (!opening) {
              respondControlError(
                msg,
                new TransportError({
                  code: "trellis.live.invalid_request",
                  message:
                    "Operation watch requires a live observation envelope.",
                  hint: "Use the live Operation watch client.",
                }),
              );
              continue;
            }
            if (!liveProvider) {
              respondControlError(
                msg,
                new AuthError({ reason: "authorization_unavailable" }),
              );
              continue;
            }
            try {
              await liveProvider.offer(
                msg,
                ctx.subject,
                opening.observation,
                {
                  connectionId: value.caller.connectionId,
                  sessionKey: value.caller.sessionKey,
                  principalId: value.caller.principalId,
                  participantId: value.caller.participantId,
                  deploymentId: value.caller.deploymentId ?? undefined,
                  instanceId: value.caller.instanceId ?? undefined,
                  contextDigest: value.caller.contextDigest,
                },
                async (session) => {
                  await runDelayedOperationSource(
                    session,
                    async ({ callerAuthority, emit, signal }) => {
                      await this.#runOperationWatchSource({
                        operation,
                        ctx,
                        operationId: opening.operationId,
                        includeUpdates: opening.includeUpdates,
                        caller: value.caller,
                        callerAuthority,
                        emit,
                        signal,
                      });
                    },
                  );
                },
                "operation",
              );
            } catch (cause) {
              respondControlError(
                msg,
                cause instanceof Error ? cause : new Error(String(cause)),
              );
            }
            continue;
          }

          if (control.action === "get") {
            msg.respond(
              JSON.stringify({ kind: "snapshot", snapshot: runtime.snapshot }),
            );
            continue;
          }

          if (control.action === "cancel") {
            if (runtime.terminal) {
              if (runtime.cancelRequestedAt) {
                msg.respond(JSON.stringify({
                  kind: "snapshot",
                  snapshot: runtime.snapshot,
                }));
                continue;
              }
              respondControlError(
                msg,
                this.#operationAlreadyTerminalError(runtime),
              );
              continue;
            }
            // Cancellation continues after shared intake advances, so transfer
            // this delivery's receiving-generation lease through its final reply.
            const lease = delivery.lease;
            delivery.lease = undefined;
            const cancelTask = (async () => {
              try {
                const snapshot = await this.#requestOperationCancellation(
                  runtime,
                );
                msg.respond(JSON.stringify({
                  kind: "snapshot",
                  snapshot,
                }));
              } catch (cause) {
                respondControlError(msg, new UnexpectedError({ cause }));
              } finally {
                lease?.release();
              }
            })();
            this.#operationControlTasks.add(cancelTask);
            const forgetCancelTask = () =>
              this.#operationControlTasks.delete(cancelTask);
            void cancelTask.then(forgetCancelTask, forgetCancelTask);
            continue;
          }

          if (control.action === "signal") {
            if (!runtime) {
              respondControlError(
                msg,
                new UnexpectedError({
                  cause: new Error("operation is not running in this process"),
                }),
              );
              continue;
            }

            try {
              const accepted = await this.#acceptSignal(
                runtime,
                ctx,
                control,
                msg.headers?.get("request-id") ?? ulid(),
              );
              const acceptedValue = accepted.take();
              if (isErr(acceptedValue)) {
                respondControlError(msg, acceptedValue.error);
                continue;
              }
              msg.respond(JSON.stringify(acceptedValue));
            } catch (cause) {
              respondControlError(
                msg,
                cause instanceof Error ? cause : new Error(String(cause)),
              );
              continue;
            }
            continue;
          }

          respondControlError(
            msg,
            new UnexpectedError({
              cause: new Error(
                `Unknown operation control action '${control.action}' for '${operation}'`,
              ),
            }),
          );
        }
      })();
      this.#mountedOperationControls.set(controlSubject, {
        // Per-generation control subscriptions are retired by the generation
        // manager; shutdown ends the shared control consumer so `stop()` can
        // await it.
        unsubscribe: () => controlIntake.close(),
        loop: controlLoop,
      });
      // The control intake must be fully installed and ready before the caller
      // can rely on it, so this is awaited by the registration path (and by the
      // activation barrier for a new generation).
      if (this.adaptiveTransport) {
        await this.declareGenerationIntake(
          `operation:${operation}:control`,
          installControl,
        );
      } else {
        const installed = await installControl({
          id: 0,
          nc: this.#nats,
          contextDigest: this.#operationContextDigest(),
        });
        this.#operationIntakeDrains.add(installed.drain);
        // Fixed connections have no per-generation retirement, so dispose the
        // control intake and its observation provider (guards included) on stop
        // rather than leaking them.
        this.#operationIntakeDrains.add(installed.dispose);
      }
    } finally {
      this.#operationControlInstalls.delete(controlSubject);
      done.resolve();
    }
  }

  async #createOperationLiveProvider(
    ctx: RegisteredRuntimeOperationDesc,
    target: {
      nc: NatsConnection;
      lease?: () => TransportLease;
    },
  ): Promise<OperationLiveObservation | undefined> {
    const cache = this.auth.authorizationProviderCache;
    // Reconstruct from the connection's *installed application authority*,
    // captured once before any await, never the historic immutable CONNECT
    // digest this physical generation was admitted with. On a safe survivor
    // after a growth or reduction that admitted digest can name an evicted or
    // superseded context, so it must not gate a healthy survivor's provider;
    // the socket and lease below still belong to this generation.
    const digest = typeof this.auth.contextDigest === "function"
      ? this.auth.contextDigest()
      : this.auth.contextDigest;
    if (!cache || !digest) return undefined;
    const own = await cache.resolveContext(digest);
    const observe = ctx.permissions?.observe;
    if (!observe) return undefined;
    const ownGuard = await LiveAuthorityGuard.retain(cache, digest, {
      kind: "local-provider",
    });
    let provider: LiveProvider | undefined;
    let observeSub: ReturnType<NatsConnection["subscribe"]> | undefined;
    try {
      provider = new LiveProvider({
        nats: target.nc,
        // Each admitted observation session pins the generation that served it.
        lease: target.lease,
        identity: {
          connectionId: own.context.connectionId,
          sessionKey: own.context.sessionKey,
          principalId: own.context.principalId,
          participantId: own.context.participantId,
          deploymentId: own.context.deploymentId ?? "",
          instanceId: own.context.instanceId ?? "",
        },
        sign: async (bytes) => await this.auth.sign(bytes),
        ownGuard,
        refreshOwnAuthority: async () => {
          const current = typeof this.auth.contextDigest === "function"
            ? this.auth.contextDigest()
            : this.auth.contextDigest;
          if (!current) return;
          const candidate = await ownGuard.prepareReplacement(current);
          if (ownGuard.commitReplacement(candidate)) {
            throw new Error("own authority replacement was rejected");
          }
        },
        permission: toVerifierPermission(observe),
        retainCallerAuthority: (callerDigest, permission) =>
          LiveAuthorityGuard.retain(cache, callerDigest, {
            kind: "observer",
            permission,
          }),
        manager: this.connection.live,
      });
      observeSub = target.nc.subscribe(
        provider.wildcardSubject(ctx.subject),
      );
      // Register at readiness so this generation owns the route's retained
      // receipts even before it has served a session; dispose() unregisters
      // transactionally on a failed install.
      provider.registerRoute(ctx.subject);
    } catch (cause) {
      // Roll back a partial provider so a failed candidate leaves no retained
      // authority, wildcard control subscription, or manager registration
      // behind. Terminate/unregister first, then release authority.
      void observeSub?.drain().catch(() => undefined);
      await provider?.dispose().catch(() => undefined);
      ownGuard.release();
      throw cause;
    }
    const observeDone = (async () => {
      for await (const msg of observeSub) {
        await provider.handleControl(msg, async (controlMsg) => {
          const validated = await this.#authenticateOperationMessage(
            controlMsg,
            ctx,
            false,
            ctx.permissions?.observe,
            ctx.observeCapabilities ?? [],
          );
          const authenticated = validated.take();
          if (isErr(authenticated)) return undefined;
          const caller = authenticated.caller;
          return {
            connectionId: caller.connectionId,
            sessionKey: caller.sessionKey,
            principalId: caller.principalId,
            participantId: caller.participantId,
            deploymentId: caller.deploymentId ?? undefined,
            instanceId: caller.instanceId ?? undefined,
            contextDigest: caller.contextDigest,
          };
        });
      }
    })();
    return {
      provider,
      // Retained until physical retirement so already-admitted observation
      // sessions keep their control channel; drained only when the generation
      // closes. Ordered so each owned session's signed END is published with a
      // live guard before the authority is released; idempotent on repeat.
      close: async () => {
        void observeSub.drain().catch(() => undefined);
        await provider.dispose().catch(() => undefined);
        await observeDone.catch(() => undefined);
        ownGuard.release();
      },
    };
  }

  async #runOperationWatchSource(args: {
    operation: string;
    ctx: RegisteredRuntimeOperationDesc;
    operationId: string;
    includeUpdates: boolean;
    caller: VerifiedCaller;
    /**
     * The live session's caller-authority port, replaced in place on an
     * identity-preserving caller refresh, so the durable watch follows the
     * caller's renewed authority instead of expiring with its opening digest.
     */
    callerAuthority: () => ProviderAuthorityPort;
    emit: (value: unknown) => Promise<void>;
    signal: AbortSignal;
  }): Promise<void> {
    const {
      operation,
      ctx,
      operationId,
      includeUpdates,
      caller,
      callerAuthority,
      emit,
      signal,
    } = args;
    if (signal.aborted) throw new Error("operation watch aborted");
    const cache = this.auth.authorizationProviderCache;
    const observe = ctx.permissions?.observe;
    if (!cache || !observe) throw new Error("authorization unavailable");
    // The observer authority is the live session's port, which the provider
    // replaces in place on an identity-preserving caller refresh; `caller` is
    // only the opening evidence recorded on the session.
    let updateSub: ReturnType<NatsConnection["subscribe"]> | undefined;
    let durableWatch: AsyncIterator<unknown> | undefined;
    let updates: Promise<void> = Promise.resolve();
    let terminalSeen = false;
    const stop = () => {
      updateSub?.unsubscribe();
      void durableWatch?.return?.();
    };
    signal.addEventListener("abort", stop, { once: true });
    // Both backend sources funnel through this single arbiter; the provider's
    // concurrent-emit rule is never violated, snapshots conflate to the latest
    // authoritative value, and admitted updates are never dropped.
    const arbiter = new OperationObserverArbiter(emit);

    try {
      // Subscribe on the operation's receiving generation so transient updates
      // published there after a newer generation takes over are still observed.
      const watchNats = this.#operationNats(operationId);
      updateSub = includeUpdates
        ? watchNats.subscribe(`${ctx.subject}.updates.${operationId}`)
        : undefined;
      const watchIterator = (await (await this.operationStoreHandle())
        .watch(operationId)
        .orThrow())[Symbol.asyncIterator]();
      durableWatch = watchIterator as AsyncIterator<unknown>;
      if (signal.aborted) throw new Error("operation watch aborted");
      if (updateSub) await watchNats.flush();
      const current = await this.#resolveOperation(operationId);
      if (
        !current ||
        !this.#matchesOperationRoute(current, operation, ctx, caller)
      ) {
        throw this.#operationNotFoundError(operationId);
      }
      await arbiter.snapshot({ kind: "snapshot", snapshot: current.snapshot });
      this.#log.debug({
        operationId,
        transferredBytes: current.snapshot.transfer?.transferredBytes,
        revision: current.snapshot.revision,
      }, "Operation observer initial snapshot emitted");
      if (
        current.snapshot.state === "completed" ||
        current.snapshot.state === "failed" ||
        current.snapshot.state === "cancelled"
      ) {
        return;
      }

      let ownerEpoch = 0;
      let updateSequence = 0;
      let revision = current.revision;
      let lastSnapshot = JSON.stringify(current.snapshot);
      updates = updateSub
        ? (async () => {
          for await (const updateMsg of updateSub) {
            if (signal.aborted) break;
            if (callerAuthority().checkNow() || terminalSeen) {
              stop();
              break;
            }
            const providerAuthorization = await verifyLocalAuthorization({
              kind: "request",
              cache: this.auth.authorizationProviderCache,
              message: updateMsg,
              permission: undefined,
              requiredCapabilities: [],
              identityOnly: true,
            });
            const provider = providerAuthorization.take();
            if (isErr(provider)) continue;
            const decoded = safeJson(updateMsg).take();
            if (
              isErr(decoded) || !decoded ||
              typeof decoded !== "object" || Array.isArray(decoded)
            ) continue;
            const envelope = decoded as Record<string, unknown>;
            const latest = await this.loadOperationRecord(operationId);
            const executor = envelope.ownerExecutorId;
            const connectionId = envelope.ownerConnectionId;
            const epoch = typeof envelope.ownerEpoch === "string" &&
                CANONICAL_POSITIVE_INTEGER.test(envelope.ownerEpoch)
              ? Number(envelope.ownerEpoch)
              : NaN;
            const sequence = typeof envelope.sequence === "string" &&
                CANONICAL_POSITIVE_INTEGER.test(envelope.sequence)
              ? Number(envelope.sequence)
              : NaN;
            const parsedUpdate = this.#decodeOperationValue(
              ctx,
              "update",
              envelope.update,
            ).take();
            // A verified peer's update that does not match the declared codec
            // is a protocol failure, not silently discarded traffic.
            if (isErr(parsedUpdate)) {
              throw new Error(
                "operation update did not match the declared codec",
              );
            }
            if (
              typeof executor !== "string" ||
              typeof connectionId !== "string" ||
              typeof envelope.occurredAt !== "string" ||
              envelope.operationId !== operationId ||
              envelope.apiId !== latest?.apiId ||
              envelope.operation !== latest?.operation ||
              envelope.deploymentId !== this.#operationDeploymentId ||
              !latest ||
              !this.#matchesOperationRoute(
                latest,
                operation,
                ctx,
                caller,
              ) || latest.snapshot.state === "completed" ||
              latest.snapshot.state === "failed" ||
              latest.snapshot.state === "cancelled" ||
              latest.cancelRequestedAt ||
              Date.parse(latest.leaseExpiresAt) <= Date.now() ||
              latest.ownerEpoch !== epoch ||
              latest.ownerInstanceId !== executor ||
              latest.ownerConnectionId !== connectionId ||
              provider.participantId !== this.contractId ||
              provider.deploymentId !== this.#operationDeploymentId ||
              provider.connectionId !== latest.ownerConnectionId ||
              !Number.isSafeInteger(epoch) ||
              !Number.isSafeInteger(sequence) ||
              epoch < ownerEpoch ||
              (epoch === ownerEpoch && sequence <= updateSequence)
            ) continue;
            ownerEpoch = epoch;
            updateSequence = sequence;
            const wireUpdate = this.#encodeOperationValue(
              ctx,
              "update",
              parsedUpdate,
            ).take();
            if (isErr(wireUpdate)) {
              throw new Error("operation update could not be re-encoded");
            }
            if (terminalSeen) break;
            if (
              !arbiter.update({
                kind: "event",
                sequence: latest.sequence,
                event: {
                  type: "update",
                  update: wireUpdate,
                  snapshot: latest.snapshot,
                },
              })
            ) {
              // The bounded observer buffer cannot admit another valid update:
              // fail only this observer with a slow-consumer outcome.
              throw new Error("operation observer update buffer overflow");
            }
          }
        })()
        : Promise.resolve();

      for (;;) {
        if (signal.aborted) throw new Error("operation watch aborted");
        const next = await watchIterator.next();
        if (next.done) {
          throw new Error("operation watch source ended");
        }
        const changed = next.value;
        const durable = changed.take();
        if (isErr(durable)) throw durable.error;
        if (!durable.value) {
          throw new Error("operation watch source ended");
        }
        if (durable.value.revision <= revision) continue;
        revision = durable.value.revision;
        if (
          !this.#matchesOperationRoute(
            durable.value,
            operation,
            ctx,
            caller,
          )
        ) {
          throw new Error("operation watch source ended");
        }
        if (callerAuthority().checkNow()) {
          throw new Error("authorization unavailable");
        }
        const terminal = durable.value.snapshot.state === "completed" ||
          durable.value.snapshot.state === "failed" ||
          durable.value.snapshot.state === "cancelled";
        // Lease-only writes advance the storage revision without changing the
        // business snapshot; they must not invent progress.
        const snapshotJson = JSON.stringify(durable.value.snapshot);
        if (snapshotJson === lastSnapshot && !terminal) continue;
        lastSnapshot = snapshotJson;
        if (terminal) {
          // Freeze further update admission before the authoritative terminal
          // snapshot; already admitted updates are already queued ahead of it.
          terminalSeen = true;
        }
        await arbiter.snapshot({
          kind: "snapshot",
          snapshot: durable.value.snapshot,
        });
        this.#log.debug({
          operationId,
          transferredBytes: durable.value.snapshot.transfer?.transferredBytes,
          revision: durable.value.snapshot.revision,
        }, "Operation observer durable snapshot emitted");
        if (terminal) return;
      }
    } finally {
      stop();
      signal.removeEventListener("abort", stop);
      await updates.catch(() => {});
    }
  }

  mountRuntime(
    method: string,
    fn: Parameters<Trellis<RuntimeApi, TrellisMode>["mount"]>[1],
  ): Promise<void> {
    return super.mount(method, fn);
  }

  static create<TA extends RuntimeApi>(
    name: string,
    nats: NatsConnection,
    auth: TrellisAuth,
    opts: TrellisServiceRuntimeOpts<TA>,
  ): TrellisServiceRuntimeFor<TA> {
    const runtime = new TrellisServiceRuntime(
      name,
      nats,
      auth,
      opts as TrellisServiceRuntimeOpts<RuntimeApi>,
    );
    return runtime as TrellisServiceRuntime & TrellisServiceRuntimeFor<TA>;
  }

  override operationHandle(
    operation: string,
  ): OperationRegistration<unknown, unknown, unknown, undefined, BaseError> {
    const ctx = this.api["operations"]
      ?.[operation as keyof typeof this.api.operations] as
        | RegisteredRuntimeOperationDesc
        | undefined;
    if (!ctx) {
      throw new Error(
        `Unknown operation '${operation.toString()}'. Did you forget to include its API module?`,
      );
    }
    const route = routeToken("operation", String(operation));

    return {
      control: (operationId) =>
        AsyncResult.from((async () => {
          await this.#ensureOperationControlLoop(String(operation), ctx);
          return await this.#controlOperation(
            String(operation),
            ctx,
            operationId,
          );
        })()),
      reconcile: (operationId) =>
        AsyncResult.from((async () => {
          const reconcile = this.#operationReconciliation.get(ctx.subject);
          if (!reconcile) {
            return err(
              new UnexpectedError({
                cause: new Error("operation handler is not registered"),
              }),
            );
          }
          try {
            return ok(await reconcile(operationId));
          } catch (cause) {
            if (cause instanceof OperationNotFoundError) return err(cause);
            return err(new UnexpectedError({ cause }));
          }
        })()),
      handle: async (
        handler: (
          context: OperationHandlerContext<
            unknown,
            unknown,
            unknown,
            OperationTransferHandle | undefined,
            BaseError
          >,
        ) => unknown | Promise<unknown>,
      ) => {
        const startSubject = ctx.subject;
        const now = () => new Date().toISOString();

        const makeOperation = (
          runtime: RuntimeOperationRecord,
          fence: RuntimeOperationFence,
          context: { requestId?: string; traceId?: string },
        ) => {
          return {
            id: runtime.id,
            started: () =>
              this.#applyControlledOperationUpdate(
                runtime,
                fence,
                ctx,
                "running",
                {
                  event: { type: "started" },
                },
              ),
            progress: (value: unknown) =>
              this.#applyControlledOperationUpdate(
                runtime,
                fence,
                ctx,
                "running",
                {
                  patch: { progress: value },
                  event: { type: "progress", progress: value },
                },
              ),
            emitUpdate: (value: unknown) =>
              AsyncResult.from(
                this.#emitOperationUpdate(runtime, fence, ctx, value),
              ),
            complete: (value: unknown) =>
              this.#applyControlledOperationUpdate(
                runtime,
                fence,
                ctx,
                "completed",
                {
                  patch: { output: value, completedAt: now() },
                  event: { type: "completed" },
                },
              ),
            fail: (error: BaseError) =>
              AsyncResult.from((async () => {
                const annotatedError = annotateHandlerBoundaryError(error, {
                  operation: String(operation),
                  requestId: context.requestId,
                  service: this.name,
                  contractId: this.contractId,
                  contractDigest: this.contractDigest,
                  traceId: context.traceId,
                });
                return await this.#applyControlledOperationUpdate(
                  runtime,
                  fence,
                  ctx,
                  "failed",
                  {
                    patch: {
                      error: annotatedError.toSerializable(),
                      completedAt: now(),
                    },
                    event: { type: "failed" },
                  },
                );
              })()),
            cancel: () =>
              this.#requestOwnedOperationCancellation(runtime, fence),
            attach: (job: { wait: () => AsyncResult<unknown, BaseError> }) =>
              AsyncResult.from((async () => {
                const waited = await job.wait();
                const waitedValue = waited.take();
                if (isErr(waitedValue)) {
                  return err(new UnexpectedError({ cause: waitedValue.error }));
                }

                const finalRuntime = await this.loadOperationRecord(runtime.id);
                if (
                  !finalRuntime || !this.#ownsOperation(finalRuntime, fence) ||
                  !isTerminalRuntimeOperationSnapshot(finalRuntime.snapshot)
                ) {
                  return err(
                    new UnexpectedError({
                      cause: new Error(
                        "attached job completed without terminal operation state",
                      ),
                    }),
                  );
                }

                return ok(finalRuntime.snapshot);
              })()),
            signals: () => this.#signals(runtime.id),
            nextSignal: (name?: string) => this.#nextSignal(runtime.id, name),
            acknowledgeSignal: (sequence: number) =>
              this.#acknowledgeSignal(runtime, fence, sequence),
            defer: () => ({ kind: "deferred" as const }),
          };
        };

        const executeHandler = async (
          runtime: RuntimeOperationRecord,
          fence: RuntimeOperationFence,
          caller: VerifiedCaller,
          transferSession?: RuntimeOperationTransferSession,
          operationContext: { requestId?: string; traceId?: string } = {},
          resuming = false,
        ) => {
          if (transferSession && !runtime.cancellation.signal.aborted) {
            let interrupted: (() => void) | undefined;
            const cancelled = new Promise<undefined>((resolve) => {
              interrupted = () => resolve(undefined);
              runtime.cancellation.signal.addEventListener(
                "abort",
                interrupted,
                { once: true },
              );
              if (runtime.cancellation.signal.aborted) interrupted();
            });
            try {
              await Promise.race([
                transferSession.transfer.completed().take(),
                cancelled,
              ]);
            } finally {
              if (interrupted) {
                runtime.cancellation.signal.removeEventListener(
                  "abort",
                  interrupted,
                );
              }
            }
          }
          if (
            transferSession && runtime.cancellation.signal.aborted &&
            !runtime.cancelRequestedAt
          ) return;
          const admitted = await this.loadOperationRecord(runtime.id);
          if (
            !admitted || !this.#ownsOperation(admitted, fence) ||
            admitted.snapshot.state === "completed" ||
            admitted.snapshot.state === "failed" ||
            admitted.snapshot.state === "cancelled" ||
            transferSession && !admitted.cancelRequestedAt &&
              (!admitted.transferGrant ||
                !Value.Check(
                  FileInfoSchema,
                  Reflect.get(admitted.transferGrant, "committed"),
                )) ||
            !this.#matchesOperationRoute(
              admitted,
              String(operation),
              ctx,
              caller,
            )
          ) {
            runtime.cancellation.abort("operation ownership lost");
            this.#releaseOperationFence(runtime.id, fence);
            return;
          }
          if (admitted.cancelRequestedAt) {
            runtime.cancelRequestedAt = admitted.cancelRequestedAt;
            runtime.cancellation.abort("operation cancelled");
          }
          runtime.revision = admitted.revision;
          runtime.leaseExpiresAt = admitted.leaseExpiresAt;
          runtime.snapshot = admitted.snapshot;
          const carrier = createMapCarrier();
          if (admitted.telemetry) {
            carrier.set("traceparent", admitted.telemetry.traceparent);
            if (admitted.telemetry.tracestate) {
              carrier.set("tracestate", admitted.telemetry.tracestate);
            }
          }
          const producer = trace.getSpanContext(extractTraceContext(carrier));
          const links = producer ? [{ context: producer }] : [];
          const startedAt = performance.now();
          const existingObservation = this.#executionObservations.get(runtime);
          const startSpan = existingObservation
            ? undefined
            : getTrellisTracer().startSpan(
              "trellis.operation.execute.start",
              {
                kind: SpanKind.CONSUMER,
                attributes: { "trellis.route": route },
                links,
              },
              ROOT_CONTEXT,
            );
          const observation = existingObservation ?? {
            fence,
            attemptContext: trace.setSpanContext(
              ROOT_CONTEXT,
              startSpan!.spanContext(),
            ),
            finish: (
              outcome:
                | "completed"
                | "failed"
                | "cancelled"
                | "lease_lost"
                | "interrupted"
                | "error",
            ) => {
              if (this.#executionObservations.get(runtime) !== observation) {
                return;
              }
              this.#executionObservations.delete(runtime);
              runtime.cancellation.signal.removeEventListener("abort", onAbort);
              recordCatalogDuration(
                "trellis.operation.execution.duration",
                performance.now() - startedAt,
                { "trellis.route": route, "trellis.outcome": outcome },
              );
              recordCatalogUpDown("trellis.operation.active", -1, {
                "trellis.route": route,
              });
              otelContext.with(observation.attemptContext, () => {
                getTrellisTracer().startSpan(
                  "trellis.operation.execute.finish",
                  {
                    kind: SpanKind.CONSUMER,
                    attributes: {
                      "trellis.route": route,
                      "trellis.outcome": outcome,
                    },
                    links,
                  },
                  observation.attemptContext,
                ).end();
              });
            },
          };
          const onAbort = () => {
            if (!runtime.cancelRequestedAt) observation.finish("lease_lost");
          };
          if (!existingObservation) {
            this.#executionObservations.set(runtime, observation);
            recordCatalogUpDown("trellis.operation.active", 1, {
              "trellis.route": route,
            });
            runtime.cancellation.signal.addEventListener("abort", onAbort, {
              once: true,
            });
            if (runtime.cancellation.signal.aborted) onAbort();
          }
          const op = makeOperation(runtime, fence, operationContext);
          let deferred = false;
          let cleanupSucceeded = false;
          let unfinishedOutcome: "interrupted" | "error" = "interrupted";
          this.#runningOperationHandlers.add(runtime.id);
          try {
            const execution = otelContext.with(
              observation.attemptContext,
              async () => {
                const progress = runtime.snapshot.progress === undefined
                  ? undefined
                  : this.#decodeOperationValue(
                    ctx,
                    "progress",
                    runtime.snapshot.progress,
                  ).orThrow();
                const handlerResult: unknown = await handler(
                  transferSession
                    ? {
                      input: runtime.input,
                      op,
                      caller,
                      signal: runtime.cancellation.signal,
                      resuming,
                      ...(progress !== undefined ? { progress } : {}),
                      transfer: transferSession.transfer,
                    }
                    : {
                      input: runtime.input,
                      op,
                      caller,
                      signal: runtime.cancellation.signal,
                      resuming,
                      ...(progress !== undefined ? { progress } : {}),
                    },
                );
                const handlerOutcome = isResultLike(handlerResult)
                  ? handlerResult.take()
                  : handlerResult;
                if (isErr(handlerOutcome)) {
                  unfinishedOutcome = "error";
                  const error = annotateHandlerBoundaryError(
                    handlerOutcome.error,
                    {
                      operation: String(operation),
                      requestId: operationContext.requestId,
                      service: this.name,
                      contractId: this.contractId,
                      contractDigest: this.contractDigest,
                      traceId: operationContext.traceId,
                    },
                  );
                  recordOperationServiceError(error, {
                    operation: String(operation),
                    phase: "handler_result",
                  });
                  await op.fail(error);
                  return;
                }
                if (isOperationDeferred(handlerOutcome)) {
                  deferred = true;
                  return;
                }
                cleanupSucceeded = true;
                if (isTerminalRuntimeOperationSnapshot(handlerOutcome)) {
                  return;
                }
                if (!runtime.terminal && !runtime.cancellation.signal.aborted) {
                  await op.complete(handlerOutcome);
                }
              },
            );
            startSpan?.end();
            await execution;
          } catch (cause) {
            unfinishedOutcome = "error";
            if (runtime.cancellation.signal.aborted) return;
            const error = annotateHandlerBoundaryError(cause, {
              operation: String(operation),
              requestId: operationContext.requestId,
              service: this.name,
              contractId: this.contractId,
              contractDigest: this.contractDigest,
              traceId: operationContext.traceId,
            });
            recordOperationServiceError(error, {
              operation: String(operation),
              phase: "handler_throw",
            });
            try {
              await op.fail(error).take();
            } catch (failure) {
              recordOperationServiceError(failure, {
                operation: String(operation),
                phase: "failure_persist",
              });
            }
          } finally {
            startSpan?.end();
            this.#runningOperationHandlers.delete(runtime.id);
            if (cleanupSucceeded && !deferred) {
              try {
                await this.#finalizeOperationCancellation(runtime, fence);
              } catch (cause) {
                recordOperationServiceError(cause, {
                  operation: String(operation),
                  phase: "cancellation_persist",
                });
              }
            }
            if (!runtime.terminal && !deferred) {
              observation.finish(
                runtime.cancellation.signal.aborted &&
                  !runtime.cancelRequestedAt
                  ? "lease_lost"
                  : unfinishedOutcome,
              );
            }
          }
        };

        const scheduleHandler = (
          ...args: Parameters<typeof executeHandler>
        ): Promise<void> => {
          const id = args[0].id;
          const previous = this.#operationExecutions.get(id) ??
            Promise.resolve();
          const next = previous.catch(() => {}).then(() =>
            executeHandler(...args)
          );
          this.#operationExecutions.set(id, next);
          void next.then(() => {
            if (this.#operationExecutions.get(id) === next) {
              this.#operationExecutions.delete(id);
            }
          }, (cause) => {
            if (this.#operationExecutions.get(id) === next) {
              this.#operationExecutions.delete(id);
            }
            recordOperationServiceError(cause, {
              operation: String(operation),
              phase: "handler_execution",
            });
          });
          return next;
        };

        const startLeaseHeartbeat = (
          runtime: RuntimeOperationRecord,
          fence: RuntimeOperationFence,
        ) => {
          const leaseHeartbeat = setInterval(() => {
            // Cancellation aborts the handler signal so cleanup can observe it,
            // but cleanup must keep renewing the lease until it finalizes. Only
            // a terminal operation (or lost ownership, handled below) stops
            // renewal, matching the owner-continues-renewing contract.
            if (runtime.terminal) {
              clearInterval(leaseHeartbeat);
              return;
            }
            void this.#queueOperationFrame(runtime, async () => {
              let outcome = "error";
              try {
                for (let retry = 0;; retry++) {
                  const durable = await this.loadOperationRecord(runtime.id);
                  if (
                    !durable ||
                    durable.snapshot.state === "cancelled" ||
                    !this.#ownsOperation(durable, fence)
                  ) {
                    outcome = "lost";
                    throw new Error("operation ownership lost");
                  }
                  runtime.revision = durable.revision;
                  runtime.snapshot = durable.snapshot;
                  runtime.cancelRequestedAt = durable.cancelRequestedAt;
                  runtime.sequence = durable.sequence;
                  runtime.signalSequence = durable.signalSequence;
                  runtime.signals = durable.signals;
                  runtime.leaseExpiresAt = new Date(Date.now() + 30_000)
                    .toISOString();
                  try {
                    await this.saveOperationRecord(runtime);
                    outcome = "ok";
                    return;
                  } catch (cause) {
                    if (retry >= 3 || !isOperationRevisionConflict(cause)) {
                      outcome = isOperationRevisionConflict(cause)
                        ? "conflict"
                        : "error";
                      throw cause;
                    }
                  }
                }
              } finally {
                recordCatalogCounter("trellis.operation.ownership.events", 1, {
                  "trellis.action": "renew",
                  "trellis.outcome": outcome,
                });
              }
            }).catch(() => {
              runtime.cancellation.abort("operation ownership lost");
              if (this.#operations.get(runtime.id) === runtime) {
                this.#operations.delete(runtime.id);
              }
              this.#releaseOperationFence(runtime.id, fence);
              clearInterval(leaseHeartbeat);
            });
          }, 10_000);
        };

        const watchCancellation = (
          runtime: RuntimeOperationRecord,
          fence: RuntimeOperationFence,
        ) => {
          let reading = false;
          let cleanupScheduled = !!runtime.cancelRequestedAt;
          const cancellationWatch = setInterval(() => {
            if (
              reading || runtime.terminal || cleanupScheduled
            ) {
              if (runtime.terminal || cleanupScheduled) {
                clearInterval(cancellationWatch);
              }
              return;
            }
            reading = true;
            void this.loadOperationRecord(runtime.id).then((durable) => {
              if (!durable || !this.#ownsOperation(durable, fence)) {
                runtime.cancellation.abort(
                  "operation cancelled or ownership lost",
                );
                clearInterval(cancellationWatch);
              } else if (durable.cancelRequestedAt) {
                runtime.cancelRequestedAt = durable.cancelRequestedAt;
                runtime.cancellation.abort("operation cancelled");
                cleanupScheduled = true;
                if (!this.#operationExecutions.has(runtime.id)) {
                  void scheduleHandler(
                    runtime,
                    fence,
                    runtime.caller,
                    this.#operationTransferSessions.get(runtime.id),
                    {},
                    true,
                  );
                }
              }
            }).catch(() => {
              runtime.cancellation.abort("operation state unavailable");
              clearInterval(cancellationWatch);
            }).finally(() => {
              reading = false;
            });
          }, 100);
        };

        const authenticate = (msg: Msg, parseInput = true) =>
          this.#authenticateOperationMessage(msg, ctx, parseInput);

        this.#log.info(
          { operation: String(operation) },
          `Mounting ${String(operation)} operation handler`,
        );

        await this.#ensureOperationControlLoop(String(operation), ctx);
        const recover = async (durable: DurableOperationRecord) => {
          if (!this.#matchesOperationRoute(durable, String(operation), ctx)) {
            return;
          }
          const runtime = await this.#acquireOperation(durable);
          if (!runtime?.reclaimed) return;
          if (this.#operationRecoveryAbort.signal.aborted) return;
          runtime.reclaimed = false;
          // Recovered execution pins the current generation so its support
          // traffic does not resurrect the seed socket or outlive its carrier.
          await this.#pinRecoveredOperation(runtime.id);
          if (runtime.transferGrant && !runtime.cancelRequestedAt) {
            const fence = this.#operationFence(runtime);
            const committed = Reflect.get(runtime.transferGrant, "committed");
            if (Value.Check(FileInfoSchema, committed)) {
              if (!this.#transferSupport) return;
              if (!runtime.uploadStorageKey) return;
              const staged = await this.#transferSupport
                .openStagedOperation(runtime.uploadStorageKey, committed)
                .take();
              if (isErr(staged)) return;
              startLeaseHeartbeat(runtime, fence);
              watchCancellation(runtime, fence);
              const transferSession = {
                grant: runtime.transferGrant,
                transfer: staged,
              };
              this.#operationTransferSessions.set(runtime.id, transferSession);
              void scheduleHandler(
                runtime,
                fence,
                runtime.caller,
                transferSession,
                {},
                true,
              );
              return;
            }
            if (!ctx.transfer || !this.#transferSupport) return;
            const contentType = asOptionalStringPointerValue(
              runtime.input,
              ctx.transfer.contentType,
            ).take();
            const metadata = asOptionalStringRecordPointerValue(
              runtime.input,
              ctx.transfer.metadata,
            ).take();
            if (isErr(contentType) || isErr(metadata)) return;
            const reopened = await this.#transferSupport
              .openOperationTransfer({
                sessionKey: runtime.callerSessionKey,
                connectionId: runtime.caller.connectionId,
                contextDigest: runtime.caller.contextDigest,
                permission: ctx.permissions?.invoke,
                requiredCapabilities: ctx.callerCapabilities ?? [],
                operationId: runtime.id,
                expiresInMs: ctx.transfer.expiresInMs ?? 60_000,
                ...(ctx.transfer.maxBytes !== undefined
                  ? { maxBytes: ctx.transfer.maxBytes }
                  : {}),
                ...(contentType !== undefined ? { contentType } : {}),
                ...(metadata !== undefined ? { metadata } : {}),
                ...this.#operationUploadPersistence(() => runtime, () => fence),
              }).take();
            if (isErr(reopened)) return;
            runtime.transferGrant = reopened.grant;
            delete runtime.uploadStorageKey;
            const current = await this.loadOperationRecord(runtime.id);
            if (!current || !this.#ownsOperation(current, fence)) {
              runtime.cancellation.abort("operation ownership lost");
              return;
            }
            runtime.revision = current.revision;
            runtime.snapshot = { ...current.snapshot };
            delete runtime.snapshot.transfer;
            runtime.snapshot.revision++;
            runtime.snapshot.updatedAt = now();
            await this.saveOperationRecord(runtime);
            startLeaseHeartbeat(runtime, fence);
            watchCancellation(runtime, fence);
            this.#operationTransferSessions.set(runtime.id, reopened);
            void scheduleHandler(
              runtime,
              fence,
              runtime.caller,
              reopened,
              {},
              true,
            );
            return;
          }
          const fence = this.#operationFence(runtime);
          startLeaseHeartbeat(runtime, fence);
          watchCancellation(runtime, fence);
          void scheduleHandler(
            runtime,
            fence,
            runtime.caller,
            undefined,
            {},
            true,
          );
        };
        this.#operationReconciliation.set(
          ctx.subject,
          async (operationId, expectedEpoch) => {
            for (let attempt = 0; attempt < 3; attempt++) {
              let runtime = await this.#resolveOperation(operationId);
              if (
                !runtime ||
                !this.#matchesOperationRoute(
                  runtime,
                  String(operation),
                  ctx,
                )
              ) throw this.#operationNotFoundError(operationId);
              if (runtime.terminal) return runtime.snapshot;
              if (
                expectedEpoch !== undefined &&
                runtime.ownerEpoch !== expectedEpoch
              ) {
                throw new Error(
                  "operation owner changed before reconciliation",
                );
              }
              let recovered = false;
              if (Date.parse(runtime.leaseExpiresAt) <= Date.now()) {
                const durable = await this.loadOperationRecord(operationId);
                if (!durable) throw this.#operationNotFoundError(operationId);
                await recover(durable);
                recovered = true;
                runtime = await this.#resolveOperation(operationId);
                if (!runtime) throw this.#operationNotFoundError(operationId);
              }
              const fence = this.#activeOperationFences.get(operationId);
              if (
                runtime.ownerInstanceId === this.#operationOwnerId &&
                fence && this.#ownsOperation(runtime, fence)
              ) {
                if (recovered) {
                  await this.#operationExecutions.get(operationId);
                } else {
                  const transfer = this.#operationTransferSessions.get(
                    operationId,
                  );
                  if (
                    runtime.transferGrant && !transfer &&
                    !runtime.cancelRequestedAt
                  ) {
                    throw new Error(
                      "operation transfer is not ready for reconciliation",
                    );
                  }
                  await scheduleHandler(
                    runtime,
                    fence,
                    runtime.caller,
                    transfer,
                    {},
                    true,
                  );
                }
                const latest = await this.loadOperationRecord(operationId);
                if (
                  !latest || !this.#matchesOperationRoute(
                    latest,
                    String(operation),
                    ctx,
                  )
                ) throw this.#operationNotFoundError(operationId);
                return latest.snapshot;
              }
              if (expectedEpoch !== undefined) {
                throw new Error("reconciliation reached a non-owner executor");
              }
              const subject = `${ctx.subject}.reconcile.${
                base64urlEncode(utf8(runtime.ownerConnectionId))
              }`;
              // Hold one generation lease through the whole reconcile exchange:
              // an adaptive failure to acquire fails the exchange rather than
              // falling back to a seed socket, and the reply cannot outlive the
              // generation that carried it.
              const reconcileDeadlineMs = Date.now() + 30_000;
              const reconcileLease = this.adaptiveTransport && this.#transport
                ? await this.#transport.acquireCurrent({
                  deadlineMs: reconcileDeadlineMs,
                })
                : { nc: this.#nats, release: () => {} };
              try {
                const reply = await reconcileLease.nc.request(
                  subject,
                  JSON.stringify({
                    operationId,
                    ownerEpoch: runtime.ownerEpoch,
                  }),
                  {
                    timeout: Math.max(0, reconcileDeadlineMs - Date.now()),
                  },
                );
                const frame: unknown = JSON.parse(
                  new TextDecoder().decode(reply.data),
                );
                if (
                  frame && typeof frame === "object" &&
                  typeof Reflect.get(frame, "error") === "string"
                ) throw new Error(Reflect.get(frame, "error"));
                if (
                  !frame || typeof frame !== "object" ||
                  Reflect.get(frame, "kind") !== "reconciled"
                ) throw new Error("invalid operation reconciliation response");
                const latest = await this.loadOperationRecord(operationId);
                if (
                  !latest || !this.#matchesOperationRoute(
                    latest,
                    String(operation),
                    ctx,
                  )
                ) throw this.#operationNotFoundError(operationId);
                if (
                  latest.ownerEpoch !== runtime.ownerEpoch ||
                  latest.ownerConnectionId !== runtime.ownerConnectionId
                ) continue;
                return latest.snapshot;
              } catch (cause) {
                const latest = await this.loadOperationRecord(operationId);
                if (
                  latest?.ownerEpoch === runtime.ownerEpoch &&
                  latest.ownerConnectionId === runtime.ownerConnectionId
                ) throw cause;
              } finally {
                reconcileLease.release();
              }
            }
            throw new Error("operation owner changed during reconciliation");
          },
        );
        // Per-generation generic intake: the start and reconcile subscriptions
        // are installed on every admitted generation with stable queue groups
        // and pumped into a single shared consumer, so the operation protocol
        // logic is registered once and each request is delivered exactly once.
        const startIntake = createOperationIntakeQueue<
          OperationIntakeDelivery & { msg: Msg }
        >((delivery) => delivery.lease?.release());
        const reconcileIntake = createOperationIntakeQueue<
          OperationIntakeDelivery & { msg: Msg }
        >((delivery) => delivery.lease?.release());
        this.#operationIntakeClosers.add(() => {
          startIntake.close();
          reconcileIntake.close();
        });
        const installIntake = (target: {
          id: number;
          nc: NatsConnection;
          contextDigest: string;
          lease?: () => TransportLease;
        }): Promise<{
          drain: () => Promise<void>;
          done: Promise<void>;
          dispose: () => Promise<void>;
        }> => {
          const reconcileSubject = `${ctx.subject}.reconcile.${
            base64urlEncode(utf8(this.#operationConnectionId))
          }`;
          const startSub = target.nc.subscribe(startSubject, {
            queue: routeQueueGroup(startSubject),
          });
          const reconcileSub = target.nc.subscribe(reconcileSubject, {
            queue: routeQueueGroup(reconcileSubject),
          });
          const acquireDeliveryLease = (
            msg: Msg,
          ): TransportLease | undefined => {
            if (!target.lease) return undefined;
            try {
              return target.lease();
            } catch {
              // The receiving generation can no longer be pinned: never hand the
              // delivery to a consumer that would execute it unpinned.
              msg.respond(JSON.stringify({
                error: "operation intake generation unavailable",
              }));
              return undefined;
            }
          };
          const startPump = (async () => {
            for await (const msg of startSub) {
              const lease = acquireDeliveryLease(msg);
              if (!lease && target.lease) continue;
              startIntake.push({
                msg,
                nc: target.nc,
                ...(lease ? { lease } : {}),
              });
            }
          })();
          const reconcilePump = (async () => {
            for await (const msg of reconcileSub) {
              const lease = acquireDeliveryLease(msg);
              if (!lease && target.lease) continue;
              reconcileIntake.push({
                msg,
                nc: target.nc,
                ...(lease ? { lease } : {}),
              });
            }
          })();
          return Promise.resolve({
            drain: async () => {
              await startSub.drain().catch(() => undefined);
              await reconcileSub.drain().catch(() => undefined);
            },
            done: Promise.all([startPump, reconcilePump]).then(() => undefined),
            dispose: async () => {
              await startSub.drain().catch(() => undefined);
              await reconcileSub.drain().catch(() => undefined);
            },
          });
        };
        void (async () => {
          for await (const delivery of reconcileIntake) {
            const message = delivery.msg;
            try {
              const value: unknown = JSON.parse(
                new TextDecoder().decode(message.data),
              );
              if (
                !value || typeof value !== "object" ||
                typeof Reflect.get(value, "operationId") !== "string" ||
                !Number.isSafeInteger(Reflect.get(value, "ownerEpoch"))
              ) throw new Error("invalid reconciliation request");
              const reconcile = this.#operationReconciliation.get(ctx.subject);
              if (!reconcile) {
                throw new Error("operation handler is not registered");
              }
              await reconcile(
                Reflect.get(value, "operationId"),
                Reflect.get(value, "ownerEpoch"),
              );
              message.respond(JSON.stringify({ kind: "reconciled" }));
            } catch (cause) {
              message.respond(JSON.stringify({
                error: cause instanceof Error ? cause.message : String(cause),
              }));
            } finally {
              // Release the receiving generation's pin once the reconcile
              // request has been fully answered.
              delivery.lease?.release();
            }
          }
        })();
        this.#operationRecoverers.set(ctx.subject, recover);
        await this.#ensureOperationRecovery();
        void (async () => {
          for await (const delivery of startIntake) {
            const { msg, nc } = delivery;
            const validated = await authenticate(msg, true);
            const value = validated.take();
            if (isErr(value)) {
              recordOperationServiceError(value.error, {
                operation: String(operation),
                phase: "start",
              });
              this.respondWithError(msg, value.error);
              continue;
            }

            let transferSession: RuntimeOperationTransferSession | undefined;
            const operationId = value.invocationId!;
            const apiId = `${ctx.permissions?.invoke.apiId ?? ""}@${
              ctx.permissions?.invoke.apiVersion ?? ""
            }`;
            const invocationDigest = (await digestJson({
              apiId,
              operation: String(operation),
              creatorPrincipalId: value.caller.principalId,
              creatorParticipantId: value.caller.participantId,
              input: value.input as JsonValue,
            })).digest;
            let reclaimed: RuntimeOperationRecord | undefined;
            const existing = await this.#resolveOperation(operationId);
            if (existing) {
              if (
                !this.#matchesOperationRoute(
                  existing,
                  String(operation),
                  ctx,
                  value.caller,
                )
              ) {
                this.respondWithError(
                  msg,
                  this.#operationNotFoundError(operationId),
                );
                continue;
              }
              if (
                existing.invocationDigest !== invocationDigest
              ) {
                this.respondWithError(
                  msg,
                  new ValidationError({
                    errors: [{
                      path: "/invocationId",
                      message:
                        "Invocation id was already accepted with different input",
                    }],
                  }),
                );
                continue;
              }
              if (value.cancellationRequested) {
                try {
                  await this.#requestOperationCancellation(existing, true);
                } catch (cause) {
                  this.respondWithError(msg, new UnexpectedError({ cause }));
                  continue;
                }
              }
              if (existing.reclaimed && !ctx.transfer) {
                reclaimed = existing;
              } else {
                msg.respond(JSON.stringify(
                  {
                    kind: "accepted",
                    ref: {
                      id: existing.id,
                      service: this.name,
                      operation: String(operation),
                    },
                    snapshot: existing.snapshot,
                    ...(existing.transferGrant
                      ? { transfer: existing.transferGrant }
                      : {}),
                  } satisfies RuntimeOperationAcceptedEnvelope,
                ));
                continue;
              }
            }
            if (ctx.transfer && !value.cancellationRequested) {
              if (!this.#transferSupport) {
                const error = new UnexpectedError({
                  cause: new Error(
                    `Operation '${
                      String(operation)
                    }' declared transfer support but no runtime transfer support is configured`,
                  ),
                });
                recordOperationServiceError(error, {
                  operation: String(operation),
                  phase: "start",
                });
                this.respondWithError(
                  msg,
                  error,
                );
                continue;
              }

              const contentType = asOptionalStringPointerValue(
                value.input,
                ctx.transfer.contentType,
              ).take();
              if (isErr(contentType)) {
                recordOperationServiceError(contentType.error, {
                  operation: String(operation),
                  phase: "start",
                });
                this.respondWithError(msg, contentType.error);
                continue;
              }

              const metadata = asOptionalStringRecordPointerValue(
                value.input,
                ctx.transfer.metadata,
              ).take();
              if (isErr(metadata)) {
                recordOperationServiceError(metadata.error, {
                  operation: String(operation),
                  phase: "start",
                });
                this.respondWithError(msg, metadata.error);
                continue;
              }

              const openedTransferValue = await this.#transferSupport
                .openOperationTransfer({
                  sessionKey: value.sessionKey,
                  connectionId: value.caller.connectionId,
                  contextDigest: value.caller.contextDigest,
                  permission: ctx.permissions?.invoke,
                  requiredCapabilities: ctx.callerCapabilities ?? [],
                  operationId,
                  expiresInMs: ctx.transfer.expiresInMs ?? 60_000,
                  ...(ctx.transfer.maxBytes !== undefined
                    ? { maxBytes: ctx.transfer.maxBytes }
                    : {}),
                  ...(contentType !== undefined ? { contentType } : {}),
                  ...(metadata !== undefined ? { metadata } : {}),
                  ...this.#operationUploadPersistence(
                    () => runtime,
                    () => transferFence,
                  ),
                }).take();
              if (isErr(openedTransferValue)) {
                recordOperationServiceError(openedTransferValue.error, {
                  operation: String(operation),
                  phase: "start",
                });
                this.respondWithError(msg, openedTransferValue.error);
                continue;
              }
              transferSession = openedTransferValue;
            }

            const createdAt = now();
            const traceparents = msg.headers?.values("traceparent") ?? [];
            const tracestates = msg.headers?.values("tracestate") ?? [];
            const traceparent = traceparents.length === 1
              ? traceparents[0]
              : undefined;
            const telemetry = validateTraceparent(traceparent)
              ? {
                traceparent,
                ...(tracestates.length === 1 &&
                    validateTracestate(tracestates[0])
                  ? { tracestate: tracestates[0] }
                  : {}),
              }
              : undefined;
            const runtime: RuntimeOperationRecord = reclaimed ?? {
              id: operationId,
              service: this.name,
              operation: String(operation),
              callerSessionKey: value.sessionKey,
              invocationDigest,
              caller: value.caller,
              creatorPrincipalId: value.caller.principalId,
              creatorParticipantId: value.caller.participantId,
              apiId,
              input: value.input,
              ...(value.cancellationRequested
                ? { cancelRequestedAt: createdAt }
                : {}),
              ...(telemetry ? { telemetry } : {}),
              revision: 1,
              ownerInstanceId: this.#operationOwnerId,
              ownerConnectionId: this.#operationConnectionId,
              ownerEpoch: 1,
              leaseExpiresAt: new Date(Date.now() + 30_000).toISOString(),
              ...(transferSession
                ? { transferGrant: transferSession.grant }
                : {}),
              snapshot: {
                id: operationId,
                service: this.name,
                operation: String(operation),
                revision: 1,
                state: "pending",
                createdAt,
                updatedAt: createdAt,
              },
              sequence: 0,
              signalSequence: 0,
              signals: [],
              terminal: false,
              watchers: new Map(),
              frameQueue: Promise.resolve(),
              signalWaiters: new Set(),
              cancellation: new AbortController(),
            };
            const transferFence = this.#operationFence(runtime);
            this.#activeOperationFences.set(runtime.id, transferFence);
            if (!reclaimed) {
              this.#operations.set(operationId, runtime);
              try {
                await this.saveOperationRecord(runtime, true);
                recordCatalogCounter("trellis.operation.ownership.events", 1, {
                  "trellis.action": "claim",
                  "trellis.outcome": "ok",
                });
              } catch (cause) {
                recordCatalogCounter("trellis.operation.ownership.events", 1, {
                  "trellis.action": "claim",
                  "trellis.outcome": isOperationRevisionConflict(cause)
                    ? "conflict"
                    : "error",
                });
                this.#operations.delete(operationId);
                const accepted = await this.#resolveOperation(operationId);
                if (!accepted) {
                  this.respondWithError(
                    msg,
                    cause instanceof Error
                      ? new ValidationError({
                        errors: [{ path: "/", message: cause.message }],
                      })
                      : new UnexpectedError({ cause }),
                  );
                  continue;
                }
                if (
                  !this.#matchesOperationRoute(
                    accepted,
                    String(operation),
                    ctx,
                    value.caller,
                  ) ||
                  accepted.invocationDigest !== invocationDigest
                ) {
                  this.respondWithError(
                    msg,
                    new ValidationError({
                      errors: [{
                        path: "/invocationId",
                        message:
                          "Invocation id was already accepted with different input",
                      }],
                    }),
                  );
                  continue;
                }
                if (value.cancellationRequested) {
                  try {
                    await this.#requestOperationCancellation(accepted, true);
                  } catch (cause) {
                    this.respondWithError(msg, new UnexpectedError({ cause }));
                    continue;
                  }
                }
                msg.respond(JSON.stringify(
                  {
                    kind: "accepted",
                    ref: {
                      id: accepted.id,
                      service: this.name,
                      operation: String(operation),
                    },
                    snapshot: accepted.snapshot,
                    ...(accepted.transferGrant
                      ? { transfer: accepted.transferGrant }
                      : {}),
                  } satisfies RuntimeOperationAcceptedEnvelope,
                ));
                continue;
              }
            }

            startLeaseHeartbeat(runtime, transferFence!);
            watchCancellation(runtime, transferFence!);

            const accepted: RuntimeOperationAcceptedEnvelope = {
              kind: "accepted",
              ref: {
                id: operationId,
                service: this.name,
                operation: String(operation),
              },
              snapshot: runtime.snapshot,
              ...(transferSession ? { transfer: transferSession.grant } : {}),
            };
            msg.respond(JSON.stringify(accepted));

            if (transferSession) {
              this.#operationTransferSessions.set(runtime.id, transferSession);
            }
            // Pin the receiving generation for the whole operation lifetime so
            // its execution support/control/observation keep working even after
            // newer generations take over new intake.
            // Ownership of the delivery's generation lease transfers to the
            // accepted execution; the loop releases only rejected deliveries.
            this.#pinOperationTransport(runtime.id, nc, delivery);
            void scheduleHandler(
              runtime,
              transferFence!,
              value.caller,
              transferSession,
              {
                requestId: msg.headers?.get("request-id"),
                traceId: traceIdFromTraceparent(
                  msg.headers?.get("traceparent"),
                ),
              },
            );
          }
        })();

        // Install the generic intake on every admitted generation (or directly
        // on the fixed connection when this runtime has no generation owner).
        if (this.adaptiveTransport) {
          await this.declareGenerationIntake(
            `operation:${String(operation)}:intake`,
            installIntake,
          );
        } else {
          const installed = await installIntake({
            id: 0,
            nc: this.#nats,
            contextDigest: this.#operationContextDigest(),
          });
          this.#operationIntakeDrains.add(installed.drain);
          this.#operationIntakeDrains.add(installed.dispose);
        }
        return Promise.resolve();
      },
    };
  }

  /**
   * Transfer an accepted delivery's already-held generation lease to the
   * operation execution so it is retained through terminal cleanup.
   *
   * The lease is acquired when the delivery is enqueued, never here: an
   * execution must not begin after a failed acquisition, and the receiving
   * generation is the one that accepted the start. When the operation is
   * already pinned (recovery/replay) this delivery's lease is left in place for
   * the consumer to release.
   */
  #pinOperationTransport(
    operationId: string,
    nc: NatsConnection,
    delivery: OperationIntakeDelivery,
  ): void {
    if (this.#operationTransport.has(operationId)) return;
    const lease = delivery.lease;
    delivery.lease = undefined;
    this.#operationTransport.set(operationId, { nc, lease });
    this.#log.debug({
      operationId,
      ...this.#activeOperationFences.get(operationId),
      physicalClientId: nc.info?.client_id,
      leased: lease !== undefined,
    }, "Accepted operation execution transport pin captured");
  }

  async stop(): Promise<void> {
    this.#stopPromise ??= (async () => {
      for (const observation of this.#executionObservations.values()) {
        observation.finish("interrupted");
      }
      // Logical cleanup is unconditional: a transport that is already closed
      // must not skip terminating intake, releasing queued generation pins, or
      // clearing logical recovery scans.
      this.#operationRecoveryAbort.abort();
      await this.retractFrameworkIntake("operation-recovery");
      if (this.#operationRecoveryScan) {
        clearInterval(this.#operationRecoveryScan);
      }
      this.#operationRecoverers.clear();
      this.#operationRecoveryIndexes.clear();
      for (const close of this.#operationIntakeClosers) close();
      for (const drain of this.#operationIntakeDrains) {
        void drain().catch(() => undefined);
      }
      this.#operationIntakeDrains.clear();
      for (const control of this.#mountedOperationControls.values()) {
        control.unsubscribe();
      }

      if (this.#transportClosed()) {
        return;
      }

      // Watch the logical lifecycle before reading any state or awaiting, so a
      // loss cannot slip between the usability check and this watch. A terminal
      // loss or close releases the waits below: NATS never rejects a pending
      // flush on close and reconnects are unbounded, so the transport status is
      // the only signal that can settle them.
      const statuses = (this.#transport?.status() ?? this.#nats.status())[
        Symbol.asyncIterator
      ]();
      const released = Promise.withResolvers<void>();
      void (async () => {
        try {
          for (;;) {
            const next = await statuses.next();
            if (next.done) break;
            const type = (next.value as { type?: string } | undefined)?.type;
            if (
              type === "disconnect" || type === "reconnecting" ||
              type === "forceReconnect" || type === "close"
            ) {
              break;
            }
          }
        } catch {
          // A status-stream failure is itself a terminal shutdown signal.
        } finally {
          released.resolve();
        }
      })();
      const untilReleased = (work: Promise<unknown>): Promise<boolean> =>
        Promise.race([
          work.then(() => false),
          released.promise.then(() => true),
        ]);

      try {
        // A stopped or not-yet-started verifier cannot settle admitted control
        // work or a graceful flush: close directly. (`health()` reports cache
        // lifecycle readiness, not current socket availability.)
        const cache = this.auth.authorizationProviderCache;
        if (cache !== undefined && !cache.health().healthy) {
          return;
        }

        if (
          await untilReleased(Promise.all([
            this.#operationRecoveryTask,
            this.#operationRecoveryInstall,
          ]))
        ) return;

        // Intake is already terminated above; settle admitted control work
        // before draining, so a stopping replica never answers a control
        // request from a draining connection. Each wait is released if the
        // transport is lost or closes.
        const controls = [...this.#mountedOperationControls.values()];
        for (const control of controls) {
          control.unsubscribe();
        }
        if (
          await untilReleased(
            Promise.allSettled(controls.map((control) => control.loop)),
          )
        ) {
          return;
        }
        while (this.#operationControlTasks.size > 0) {
          if (
            await untilReleased(
              Promise.allSettled([...this.#operationControlTasks]),
            )
          ) {
            return;
          }
        }

        // A loss or terminal close releases the drain instead of waiting on a
        // flush that never settles. An adaptive transport is closed through the
        // logical provider in `finally`, never drained on a raw generation.
        if (!this.adaptiveTransport) {
          await untilReleased(
            this.#nats.drain().catch((cause) => {
              if (
                !(cause instanceof Error) ||
                cause.name !== "DrainingConnectionError"
              ) {
                throw cause;
              }
            }),
          );
        }
      } finally {
        void statuses.return?.().catch(() => undefined);
        if (this.adaptiveTransport) {
          // The logical owner closes every generation; never a raw seed socket.
          await this.#transport?.close().catch(() => undefined);
        } else {
          await this.#nats.close();
        }
      }
    })();

    await this.#stopPromise;
  }
}
