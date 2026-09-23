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
  Result,
} from "@oatscenter/result";
import { ulid } from "ulid";

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
  type TrellisErrorInstance,
  UnexpectedError,
  ValidationError,
} from "../../errors/index.ts";
import type { LoggerLike } from "../../globals.ts";
import { serviceRuntimeLogger } from "./logger.ts";
import {
  recordTrellisError,
  type TrellisErrorMetricAttributes,
} from "../../telemetry/mod.ts";
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
  Trellis,
  type TrellisAuth,
  type TrellisMode,
  type TrellisOpts,
  type VerifiedCaller,
  verifyLocalAuthorization,
} from "../../session.ts";
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
    feedOwnerId?: string;
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
    permission: PermissionAtom | undefined;
    requiredCapabilities: readonly string[];
    store: string;
    key: string;
    expiresInMs: number;
    maxBytes?: number;
    contentType?: string;
    metadata?: Record<string, string>;
    onComplete?: (info: FileInfo) => Promise<void>;
  }): AsyncResult<RuntimeOperationTransferSession, TransferError>;
};

type RuntimeOperationFence = Readonly<{
  ownerInstanceId: string;
  ownerConnectionId: string;
  ownerEpoch: number;
}>;

const CANONICAL_POSITIVE_INTEGER = /^[1-9][0-9]*$/u;

function asStringPointerValue(
  operation: string,
  input: unknown,
  pointer: `/${string}`,
  field: string,
): Result<string, TransferError> {
  const value = Pointer.Get(input as Record<string, unknown>, pointer);
  if (typeof value !== "string" || value.length === 0) {
    return err(
      new TransferError({
        operation: "transfer",
        context: { reason: "invalid_input", operation, field, pointer },
      }),
    );
  }
  return ok(value);
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

export class TrellisServiceRuntime extends Trellis<RuntimeApi, TrellisMode> {
  #nats: NatsConnection;
  #version?: string;
  #log: LoggerLike;
  #operations = new Map<string, RuntimeOperationRecord>();
  #activeOperationFences = new Map<string, RuntimeOperationFence>();
  #operationUpdateSequences = new Map<string, number>();
  #mountedOperationControls = new Set<string>();
  #stopPromise?: Promise<void>;
  #transferSupport?: RuntimeOperationTransferSupport;
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
    if (opts?.feedOwnerId) this.setFeedOwnerId(opts.feedOwnerId);
    this.#nats = nats;
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
        this.#applyOwnedOperationUpdate(operationId, "cancelled", {
          event: { type: "cancelled" },
        }),
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
    } catch {
      if (this.#operations.get(runtime.id) === runtime) {
        this.#operations.delete(runtime.id);
      }
      return null;
    }
    runtime.reclaimed = true;
    this.#activeOperationFences.set(runtime.id, this.#operationFence(runtime));
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
    durable: DurableOperationRecord,
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
      this.#activeOperationFences.delete(operationId);
    }
  }

  #applyOperationUpdate(
    runtime: RuntimeOperationRecord,
    fence: RuntimeOperationFence,
    state: RuntimeOperationState,
    opts: {
      patch?: Partial<RuntimeOperationSnapshot>;
      event: Record<string, unknown> & { type: string };
    },
  ): AsyncResult<RuntimeOperationSnapshot, BaseError> {
    return AsyncResult.from((async () => {
      return await this.#queueOperationFrame(runtime, async () => {
        for (let retry = 0;; retry++) {
          const durable = await this.loadOperationRecord(runtime.id);
          if (!durable || !this.#ownsOperation(durable, fence)) {
            runtime.cancellation.abort("operation ownership lost");
            return err(this.#operationAlreadyTerminalError(runtime));
          }
          if (
            durable.snapshot.state === "completed" ||
            durable.snapshot.state === "failed" ||
            durable.snapshot.state === "cancelled" ||
            durable.cancelRequestedAt ||
            runtime.cancellation.signal.aborted
          ) return err(this.#operationAlreadyTerminalError(runtime));
          runtime.revision = durable.revision;
          runtime.snapshot = durable.snapshot;
          runtime.sequence = durable.sequence + 1;
          runtime.signalSequence = durable.signalSequence;
          runtime.signals = durable.signals;
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
            break;
          } catch (cause) {
            if (retry >= 3 || !isOperationRevisionConflict(cause)) {
              runtime.cancellation.abort("operation ownership lost");
              throw cause;
            }
          }
        }

        if (runtime.terminal) {
          this.#releaseOperationFence(runtime.id, fence);
          this.#rejectSignalWaiters(runtime);
        }

        return ok(runtime.snapshot);
      });
    })());
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
      await this.#nats.publish(
        subject,
        payload,
        { headers: metadata, reply },
      );
      await this.#nats.flush();
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
      if (
        runtime.ownerInstanceId !== fence.ownerInstanceId ||
        runtime.ownerEpoch !== fence.ownerEpoch
      ) {
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
      cancel: () =>
        this.#applyControlledOperationUpdate(runtime, fence, ctx, "cancelled", {
          event: { type: "cancelled" },
        }),
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
      (!caller ||
        (runtime.creatorPrincipalId === caller.principalId &&
          runtime.creatorParticipantId === caller.participantId));
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
      throw cause;
    }
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
            return ok(undefined);
          } catch (cause) {
            if (retry >= 3 || !isOperationRevisionConflict(cause)) {
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
      caller: VerifiedCaller;
      sessionKey: string;
    }, UnexpectedError | AuthError | ValidationError>
  > {
    const jsonData = safeJson(msg).take();
    if (isErr(jsonData)) return jsonData;

    let parsedInput: unknown;
    let invocationId: string | undefined;
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

    return ok({
      input: parsedInput,
      ...(invocationId ? { invocationId } : {}),
      caller: authValue,
      sessionKey: authValue.sessionKey,
    });
  }

  #ensureOperationControlLoop(
    operation: string,
    ctx: RegisteredRuntimeOperationDesc,
  ): void {
    const controlSubject = `${ctx.subject}.control`;
    if (this.#mountedOperationControls.has(controlSubject)) {
      return;
    }
    this.#mountedOperationControls.add(controlSubject);

    const publishFrame = async (reply: string, frame: unknown) => {
      await this.#nats.publish(reply, JSON.stringify(frame));
    };

    const publishSnapshot = async (
      reply: string,
      snapshot: RuntimeOperationSnapshot,
    ) => {
      await publishFrame(reply, { kind: "snapshot", snapshot });
    };

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

    const controlSub = this.#nats.subscribe(controlSubject, {
      queue: routeQueueGroup(controlSubject),
    });
    void (async () => {
      for await (const msg of controlSub) {
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
        const snapshot = runtime.snapshot;
        if (control.action === "watch") {
          if (msg.reply) {
            const reply = msg.reply;
            const updateSub = control.includeUpdates
              ? this.#nats.subscribe(
                `${ctx.subject}.updates.${control.operationId}`,
              )
              : undefined;
            void (async () => {
              let ownerEpoch = 0;
              let updateSequence = 0;
              let revision = runtime.revision;
              const durableWatch = (await (await this.operationStoreHandle())
                .watch(
                  control.operationId,
                ).orThrow())[Symbol.asyncIterator]();
              if (updateSub) await this.#nats.flush();
              await publishSnapshot(reply, runtime.snapshot);
              const updates = updateSub
                ? (async () => {
                  for await (const updateMsg of updateSub) {
                    try {
                      const cache = this.auth.authorizationProviderCache;
                      if (!cache) throw new Error("authorization unavailable");
                      await cache.resolveContext(value.caller.contextDigest);
                    } catch {
                      updateSub.unsubscribe();
                      await durableWatch.return?.();
                      break;
                    }
                    const providerAuthorization =
                      await verifyLocalAuthorization({
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
                    const current = await this.loadOperationRecord(
                      control.operationId,
                    );
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
                    if (
                      isErr(parsedUpdate) ||
                      typeof executor !== "string" ||
                      typeof connectionId !== "string" ||
                      typeof envelope.occurredAt !== "string" ||
                      envelope.operationId !== control.operationId ||
                      envelope.apiId !== current?.apiId ||
                      envelope.operation !== current?.operation ||
                      envelope.deploymentId !== this.#operationDeploymentId ||
                      !current ||
                      !this.#matchesOperationRoute(
                        current,
                        operation,
                        ctx,
                        value.caller,
                      ) || current.snapshot.state === "completed" ||
                      current.snapshot.state === "failed" ||
                      current.snapshot.state === "cancelled" ||
                      current.cancelRequestedAt ||
                      Date.parse(current.leaseExpiresAt) <= Date.now() ||
                      current.ownerEpoch !== epoch ||
                      current.ownerInstanceId !== executor ||
                      current.ownerConnectionId !== connectionId ||
                      provider.participantId !== this.contractId ||
                      provider.deploymentId !== this.#operationDeploymentId ||
                      provider.connectionId !== current.ownerConnectionId ||
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
                    if (isErr(wireUpdate)) continue;
                    await publishFrame(reply, {
                      kind: "event",
                      sequence: current.sequence,
                      event: {
                        type: "update",
                        update: wireUpdate,
                        snapshot: current.snapshot,
                      },
                    });
                  }
                })()
                : Promise.resolve();
              try {
                for (;;) {
                  const next = await durableWatch.next();
                  if (next.done) break;
                  const changed = next.value;
                  const durable = changed.take();
                  if (isErr(durable)) throw durable.error;
                  if (!durable.value) break;
                  if (durable.value.revision <= revision) continue;
                  revision = durable.value.revision;
                  if (
                    !this.#matchesOperationRoute(
                      durable.value,
                      operation,
                      ctx,
                      value.caller,
                    )
                  ) break;
                  try {
                    const cache = this.auth.authorizationProviderCache;
                    if (!cache) throw new Error("authorization unavailable");
                    await cache.resolveContext(value.caller.contextDigest);
                  } catch {
                    break;
                  }
                  await publishSnapshot(reply, durable.value.snapshot);
                  if (
                    durable.value.snapshot.state === "completed" ||
                    durable.value.snapshot.state === "failed" ||
                    durable.value.snapshot.state === "cancelled"
                  ) break;
                }
              } finally {
                updateSub?.unsubscribe();
                await updates;
              }
            })().catch((error) => {
              if (!this.#nats.isClosed()) {
                this.#log.warn(
                  { error, operation: String(operation) },
                  "Operation watch stopped",
                );
              }
            });
          }
          continue;
        }

        if (control.action === "get") {
          msg.respond(JSON.stringify({ kind: "snapshot", snapshot }));
          continue;
        }

        if (control.action === "cancel") {
          if (runtime.terminal) {
            respondControlError(
              msg,
              this.#operationAlreadyTerminalError(runtime),
            );
            continue;
          }
          try {
            let current = runtime;
            for (let retry = 0;; retry++) {
              current.cancelRequestedAt = new Date().toISOString();
              current.sequence += 1;
              current.snapshot = buildRuntimeOperationSnapshot(
                current,
                "cancelled",
                { completedAt: current.cancelRequestedAt },
              );
              current.terminal = true;
              try {
                await this.saveOperationRecord(current);
                break;
              } catch (cause) {
                this.#operations.delete(current.id);
                if (retry >= 3 || !isOperationRevisionConflict(cause)) {
                  throw cause;
                }
                const reloaded = await this.#resolveOperation(current.id);
                if (!reloaded || reloaded.terminal) throw cause;
                current = reloaded;
              }
            }
            current.cancellation.abort("operation cancelled");
            msg.respond(
              JSON.stringify({ kind: "snapshot", snapshot: current.snapshot }),
            );
          } catch (cause) {
            respondControlError(msg, new UnexpectedError({ cause }));
          }
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

    return {
      control: (operationId) => {
        this.#ensureOperationControlLoop(String(operation), ctx);
        return this.#controlOperation(String(operation), ctx, operationId);
      },
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

        const publishFrame = async (reply: string, frame: unknown) => {
          await this.#nats.publish(reply, JSON.stringify(frame));
        };

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
              this.#applyControlledOperationUpdate(
                runtime,
                fence,
                ctx,
                "cancelled",
                {
                  patch: { completedAt: now() },
                  event: { type: "cancelled" },
                },
              ),
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
          const admitted = await this.loadOperationRecord(runtime.id);
          if (
            !admitted || !this.#ownsOperation(admitted, fence) ||
            admitted.snapshot.state === "completed" ||
            admitted.snapshot.state === "failed" ||
            admitted.snapshot.state === "cancelled" ||
            admitted.cancelRequestedAt ||
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
          runtime.revision = admitted.revision;
          runtime.leaseExpiresAt = admitted.leaseExpiresAt;
          runtime.snapshot = admitted.snapshot;
          const op = makeOperation(runtime, fence, operationContext);
          try {
            const handlerResult: unknown = await handler(
              transferSession
                ? {
                  input: runtime.input,
                  op,
                  caller,
                  signal: runtime.cancellation.signal,
                  resuming,
                  ...(runtime.snapshot.progress !== undefined
                    ? { progress: runtime.snapshot.progress }
                    : {}),
                  transfer: transferSession.transfer,
                }
                : {
                  input: runtime.input,
                  op,
                  caller,
                  signal: runtime.cancellation.signal,
                  resuming,
                  ...(runtime.snapshot.progress !== undefined
                    ? { progress: runtime.snapshot.progress }
                    : {}),
                },
            );
            const handlerOutcome = isResultLike(handlerResult)
              ? handlerResult.take()
              : handlerResult;
            if (isErr(handlerOutcome)) {
              const error = annotateHandlerBoundaryError(handlerOutcome.error, {
                operation: String(operation),
                requestId: operationContext.requestId,
                service: this.name,
                contractId: this.contractId,
                contractDigest: this.contractDigest,
                traceId: operationContext.traceId,
              });
              recordOperationServiceError(error, {
                operation: String(operation),
                phase: "handler_result",
              });
              await op.fail(error);
              return;
            }
            if (isOperationDeferred(handlerOutcome)) return;
            if (isTerminalRuntimeOperationSnapshot(handlerOutcome)) {
              return;
            }
            if (!runtime.terminal) await op.complete(handlerOutcome);
          } catch (cause) {
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
          }
        };

        const startLeaseHeartbeat = (
          runtime: RuntimeOperationRecord,
          fence: RuntimeOperationFence,
        ) => {
          const leaseHeartbeat = setInterval(() => {
            if (runtime.terminal || runtime.cancellation.signal.aborted) {
              clearInterval(leaseHeartbeat);
              return;
            }
            void this.#queueOperationFrame(runtime, async () => {
              for (let retry = 0;; retry++) {
                const durable = await this.loadOperationRecord(runtime.id);
                if (
                  !durable || durable.cancelRequestedAt ||
                  durable.snapshot.state === "cancelled" ||
                  !this.#ownsOperation(durable, fence)
                ) throw new Error("operation ownership lost");
                runtime.revision = durable.revision;
                runtime.snapshot = durable.snapshot;
                runtime.sequence = durable.sequence;
                runtime.signalSequence = durable.signalSequence;
                runtime.signals = durable.signals;
                runtime.leaseExpiresAt = new Date(Date.now() + 30_000)
                  .toISOString();
                try {
                  await this.saveOperationRecord(runtime);
                  return;
                } catch (cause) {
                  if (retry >= 3 || !isOperationRevisionConflict(cause)) {
                    throw cause;
                  }
                }
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
          const cancellationWatch = setInterval(() => {
            if (
              reading || runtime.terminal || runtime.cancellation.signal.aborted
            ) {
              if (runtime.terminal || runtime.cancellation.signal.aborted) {
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

        this.#ensureOperationControlLoop(String(operation), ctx);
        const recover = async (durable: DurableOperationRecord) => {
          if (!this.#matchesOperationRoute(durable, String(operation), ctx)) {
            return;
          }
          const runtime = await this.#acquireOperation(durable);
          if (!runtime?.reclaimed) return;
          runtime.reclaimed = false;
          if (runtime.transferGrant) {
            const fence = this.#operationFence(runtime);
            const committed = Reflect.get(runtime.transferGrant, "committed");
            if (Value.Check(FileInfoSchema, committed)) {
              startLeaseHeartbeat(runtime, fence);
              watchCancellation(runtime, fence);
              void executeHandler(
                runtime,
                fence,
                runtime.caller,
                {
                  grant: runtime.transferGrant,
                  transfer: {
                    updates: async function* () {},
                    completed: () =>
                      AsyncResult.from(Promise.resolve(ok(committed))),
                  },
                },
                {},
                true,
              );
              return;
            }
            if (!ctx.transfer || !this.#transferSupport) return;
            const key = asStringPointerValue(
              String(operation),
              runtime.input,
              ctx.transfer.key,
              "key",
            ).take();
            const contentType = asOptionalStringPointerValue(
              runtime.input,
              ctx.transfer.contentType,
            ).take();
            const metadata = asOptionalStringRecordPointerValue(
              runtime.input,
              ctx.transfer.metadata,
            ).take();
            if (isErr(key) || isErr(contentType) || isErr(metadata)) return;
            const reopened = await this.#transferSupport
              .openOperationTransfer({
                sessionKey: runtime.callerSessionKey,
                permission: ctx.permissions?.invoke,
                requiredCapabilities: ctx.callerCapabilities ?? [],
                store: ctx.transfer.store,
                key,
                expiresInMs: ctx.transfer.expiresInMs ?? 60_000,
                ...(ctx.transfer.maxBytes !== undefined
                  ? { maxBytes: ctx.transfer.maxBytes }
                  : {}),
                ...(contentType !== undefined ? { contentType } : {}),
                ...(metadata !== undefined ? { metadata } : {}),
                onComplete: async (info) => {
                  await this.#queueOperationFrame(runtime, async () => {
                    if (!runtime.transferGrant) return;
                    const durable = await this.loadOperationRecord(runtime.id);
                    if (
                      !durable || !this.#ownsOperation(durable, fence) ||
                      durable.cancelRequestedAt ||
                      durable.snapshot.state === "completed" ||
                      durable.snapshot.state === "failed" ||
                      durable.snapshot.state === "cancelled"
                    ) {
                      runtime.cancellation.abort("operation ownership lost");
                      return;
                    }
                    runtime.revision = durable.revision;
                    Reflect.set(runtime.transferGrant, "committed", info);
                    await this.saveOperationRecord(runtime);
                  });
                },
              }).take();
            if (isErr(reopened)) return;
            runtime.transferGrant = reopened.grant;
            const current = await this.loadOperationRecord(runtime.id);
            if (!current || !this.#ownsOperation(current, fence)) {
              runtime.cancellation.abort("operation ownership lost");
              return;
            }
            runtime.revision = current.revision;
            await this.saveOperationRecord(runtime);
            startLeaseHeartbeat(runtime, fence);
            void (async () => {
              for await (const progress of reopened.transfer.updates()) {
                await this.#applyOperationUpdate(
                  runtime,
                  fence,
                  "running",
                  {
                    patch: { transfer: progress },
                    event: { type: "transfer", transfer: progress },
                  },
                );
              }
            })();
            watchCancellation(runtime, fence);
            void executeHandler(
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
          void executeHandler(
            runtime,
            fence,
            runtime.caller,
            undefined,
            {},
            true,
          );
        };
        const recoverExpired = async () => {
          for (const durable of await this.listNonterminalOperationRecords()) {
            if (Date.parse(durable.leaseExpiresAt) <= Date.now()) {
              await recover(durable);
            }
          }
        };
        await recoverExpired();
        const recoveryScan = setInterval(() => {
          if (this.#nats.isClosed()) {
            clearInterval(recoveryScan);
            return;
          }
          void recoverExpired().catch((error) => {
            if (!this.#nats.isClosed()) {
              this.#log.warn(
                { error, operation: String(operation) },
                "Operation recovery scan failed",
              );
            }
          });
        }, 1_000);
        const startSub = this.#nats.subscribe(startSubject, {
          queue: routeQueueGroup(startSubject),
        });

        void (async () => {
          for await (const msg of startSub) {
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
            let transferFence: RuntimeOperationFence | undefined;
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
            if (ctx.transfer) {
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

              const key = ctx.transfer.key
                ? asStringPointerValue(
                  String(operation),
                  value.input,
                  ctx.transfer.key,
                  "key",
                ).take()
                : operationId;
              if (isErr(key)) {
                recordOperationServiceError(key.error, {
                  operation: String(operation),
                  phase: "start",
                });
                this.respondWithError(msg, key.error);
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
                  permission: ctx.permissions?.invoke,
                  requiredCapabilities: ctx.callerCapabilities ?? [],
                  store: ctx.transfer.store,
                  key,
                  expiresInMs: ctx.transfer.expiresInMs ?? 60_000,
                  ...(ctx.transfer.maxBytes !== undefined
                    ? { maxBytes: ctx.transfer.maxBytes }
                    : {}),
                  ...(contentType !== undefined ? { contentType } : {}),
                  ...(metadata !== undefined ? { metadata } : {}),
                  onComplete: async (info) => {
                    await this.#queueOperationFrame(runtime, async () => {
                      if (!runtime.transferGrant || !transferFence) return;
                      const durable = await this.loadOperationRecord(
                        runtime.id,
                      );
                      if (
                        !durable ||
                        !this.#ownsOperation(durable, transferFence) ||
                        durable.cancelRequestedAt ||
                        durable.snapshot.state === "completed" ||
                        durable.snapshot.state === "failed" ||
                        durable.snapshot.state === "cancelled"
                      ) {
                        runtime.cancellation.abort("operation ownership lost");
                        return;
                      }
                      runtime.revision = durable.revision;
                      Reflect.set(runtime.transferGrant, "committed", info);
                      await this.saveOperationRecord(runtime);
                    });
                  },
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
            transferFence = this.#operationFence(runtime);
            this.#activeOperationFences.set(runtime.id, transferFence);
            if (!reclaimed) {
              this.#operations.set(operationId, runtime);
              try {
                await this.saveOperationRecord(runtime);
              } catch (cause) {
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

            if (transferSession) {
              void (async () => {
                for await (
                  const progress of transferSession.transfer.updates()
                ) {
                  await this.#applyOperationUpdate(
                    runtime,
                    transferFence!,
                    "running",
                    {
                      patch: { transfer: progress },
                      event: { type: "transfer", transfer: progress },
                    },
                  );
                }
              })();
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

            void executeHandler(
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

        return Promise.resolve();
      },
    };
  }

  async stop(): Promise<void> {
    this.#stopPromise ??= (async () => {
      if (this.#nats.isClosed()) {
        return;
      }

      try {
        await this.#nats.drain();
      } catch (cause) {
        if (
          !(cause instanceof Error) ||
          cause.name !== "DrainingConnectionError"
        ) {
          throw cause;
        }

        await this.#nats.closed().catch(() => undefined);
      }
    })();

    await this.#stopPromise;
  }
}
