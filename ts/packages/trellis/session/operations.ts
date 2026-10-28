import { AsyncResult, BaseError, Result } from "@oatscenter/result";
import { OperationInvoker, type OperationRefData } from "../operations.ts";
import {
  type FileInfo,
  type SendTransferGrant,
  type TransferOperationError,
} from "../transfer.ts";
import { TransferError } from "../errors/index.ts";
import {
  type SessionCaller,
  type VerifiedCaller,
} from "./local_authorization.ts";
import { type RuntimeApi } from "../participant_runtime/api.ts";
import {
  type OperationHandlerErrorOf,
  type OperationInputOf,
  type OperationOutputOf,
  type OperationProgressOf,
  type OperationUpdateOf,
  type OperationsOf,
} from "./contract_types.ts";
import { type TrellisMode } from "./types.ts";
import { type JsonValue } from "../codec.ts";
import { type SerializableRuntimeError } from "./handler_errors.ts";
import { type AuthenticatedCaller } from "../auth/protocol_wasm.ts";
import { type PersistedOperationUpload } from "../service/runtime/operation_record.ts";

export type OperationRuntimeHandle<
  TProgress,
  TOutput,
  TError extends BaseError,
  TUpdate = unknown,
> = {
  id: string;
  /**
   * The complete reference a caller resumes this operation with, for example
   * in a return URL the handler builds. Serialize it with `JSON.stringify`.
   */
  ref: OperationRefData;
  started(): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  progress(value: TProgress): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  /** Emits a transient update without changing the persisted snapshot. */
  emitUpdate(value: TUpdate): AsyncResult<void, BaseError>;
  complete(value: TOutput): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  fail(error: TError): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  /** Durably requests cancellation; terminal state follows handler cleanup. */
  cancel(): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  attach(job: {
    wait(): AsyncResult<unknown, BaseError>;
  }): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  signals(): AsyncIterable<RuntimeOperationSignal>;
  nextSignal(name?: string): AsyncResult<RuntimeOperationSignal, BaseError>;
  /** Durably acknowledges a signal after its side effects have been accepted. */
  acknowledgeSignal(sequence: number): AsyncResult<void, BaseError>;
  defer(): OperationDeferred;
};

export type OperationDeferred = {
  kind: "deferred";
};

/**
 * Returns true when a handler result explicitly leaves operation completion to
 * an external control path.
 */
export function isOperationDeferred(
  value: unknown,
): value is OperationDeferred {
  return (
    !!value &&
    typeof value === "object" &&
    "kind" in value &&
    value.kind === "deferred"
  );
}

export type AcceptedOperation<
  TProgress,
  TOutput,
  TError extends BaseError,
  TUpdate = unknown,
> = OperationRuntimeHandle<TProgress, TOutput, TError, TUpdate> & {
  ref: OperationRefData;
  snapshot: RuntimeOperationSnapshot & {
    progress?: TProgress;
    output?: TOutput;
  };
};

/**
 * A provider's view of an Operation's caller-sent bytes.
 *
 * For an `upload` Operation the bytes are durably staged before the handler
 * reads them, and can be read again. For a `stream` Operation the handler
 * reads the caller's bytes as they arrive, exactly once: the stream ends only
 * after the caller's final size and SHA-256 digest were verified and the
 * delivery was durably recorded, and errors if the transfer fails. A stream
 * cannot be replayed, so an interrupted `stream` Operation fails and the
 * caller must start a new one.
 */
export type OperationTransferHandle = {
  /** Consumed-byte progress while the transfer runs. */
  updates(): AsyncIterable<RuntimeOperationTransferProgress>;
  /** Resolves with the verified object metadata once the bytes are committed. */
  completed(): AsyncResult<FileInfo, TransferOperationError>;
  /**
   * `upload`: waits for staging, then opens the staged object.
   * `stream`: returns the live stream once; a second call fails.
   */
  stream(): AsyncResult<ReadableStream<Uint8Array>, TransferOperationError>;
  /**
   * `upload`: waits for staging, then reads the staged bytes.
   * `stream`: reads the live stream to its end, retaining every byte.
   */
  bytes(): AsyncResult<Uint8Array, TransferError>;
};

export type OperationHandlerContext<
  TInput,
  TProgress,
  TOutput,
  TTransfer,
  TError extends BaseError,
  TUpdate = unknown,
> = {
  input: TInput;
  op: OperationRuntimeHandle<TProgress, TOutput, TError, TUpdate>;
  caller: SessionCaller;
  /**
   * Aborted when cancellation is requested or this executor loses ownership.
   * Cancellation-first admission and cancellation recovery enter the handler
   * with this signal already aborted. Perform cleanup before business work or
   * accessing transfer, then return successfully to acknowledge cleanup.
   * Throwing, returning an error, or deferring leaves cancellation nonterminal.
   */
  signal: AbortSignal;
  /** Whether this handler resumes an earlier invocation or reconciles durable state. */
  resuming: boolean;
  /** Last durably persisted progress available to a resumed handler. */
  progress?: TProgress;
} & (TTransfer extends undefined
  ? {}
  : {
      /**
       * Absent during cancellation-first admission or cancellation-only recovery
       * when staging was not opened. Handle aborted-signal cleanup before use.
       */
      transfer?: TTransfer;
    });

export type OperationRegistration<
  TInput,
  TProgress,
  TOutput,
  TTransfer,
  TError extends BaseError,
  TUpdate = unknown,
> = {
  /**
   * Loads an existing operation by id and returns a service-side control handle.
   * The operation must belong to this service and registration name.
   */
  control(
    operationId: string,
  ): AsyncResult<
    OperationRuntimeHandle<TProgress, TOutput, TError, TUpdate>,
    BaseError
  >;
  /** Rerun the registered handler on its current owner with durable resume state. */
  reconcile(
    operationId: string,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  handle(
    handler: (
      context: OperationHandlerContext<
        TInput,
        TProgress,
        TOutput,
        TTransfer,
        TError,
        TUpdate
      >,
    ) => unknown | Promise<unknown>,
  ): Promise<void>;
};

export type OperationTransferContextOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { transfer: infer TTransfer }
  ? TTransfer extends undefined
    ? undefined
    : OperationTransferHandle
  : undefined;

export type OperationSurface<
  TA extends RuntimeApi,
  TMode extends TrellisMode,
  O extends OperationsOf<TA>,
> = TMode extends "service"
  ? OperationRegistration<
      OperationInputOf<TA, O>,
      OperationProgressOf<TA, O>,
      OperationOutputOf<TA, O>,
      OperationTransferContextOf<TA, O>,
      OperationHandlerErrorOf<TA, O>,
      OperationUpdateOf<TA, O>
    >
  : OperationInvoker<TA["operations"][O] & RuntimeOperationDesc>;

export type RuntimeOperationDesc = {
  subject: string;
  permissions: RuntimeApi["operations"][string]["permissions"];
  input: unknown;
  revision: number;
  progress?: unknown;
  update?: unknown;
  output?: unknown;
  signals?: Record<string, { input: unknown }>;
  cancelCapabilities?: readonly string[];
  controlCapabilities?: readonly string[];
  /** Caller-sent bytes; `stream` delivers them to the running handler. */
  transfer?: { direction: "send"; stream?: boolean };
  cancel?: boolean;
};

export type RuntimeOperationSignal = {
  operationId: string;
  sequence: number;
  requestId: string;
  signal: string;
  input?: JsonValue;
  acceptedAt: string;
  acknowledged: boolean;
};

export type RuntimeOperationSignalWaiter = (
  result: Result<RuntimeOperationSignal, BaseError>,
) => void;

export type RuntimeOperationTransferProgress = {
  chunkIndex: number;
  chunkBytes: number;
  transferredBytes: number;
};

export type RuntimeOperationState =
  "pending" | "running" | "completed" | "failed" | "cancelled";

export type RuntimeOperationSnapshot = {
  id: string;
  service: string;
  operation: string;
  revision: number;
  state: RuntimeOperationState;
  createdAt: string;
  updatedAt: string;
  completedAt?: string;
  progress?: unknown;
  transfer?: RuntimeOperationTransferProgress;
  output?: unknown;
  error?: SerializableRuntimeError;
};

export type RuntimeOperationRecord = {
  id: string;
  deploymentId: string;
  service: string;
  operation: string;
  callerSessionKey: string;
  invocationDigest: string;
  caller: VerifiedCaller;
  creatorPrincipalId: string;
  creatorBinding: AuthenticatedCaller["binding"];
  apiId: string;
  input: unknown;
  telemetry?: { traceparent: string; tracestate?: string };
  revision: number;
  ownerInstanceId: string;
  ownerConnectionId: string;
  ownerEpoch: number;
  leaseExpiresAt: string;
  cancelRequestedAt?: string;
  transferGrant?: SendTransferGrant;
  /** Retained upload attempt, including its private staging locator and state. */
  uploadState?: PersistedOperationUpload;
  snapshot: RuntimeOperationSnapshot;
  sequence: number;
  signalSequence: number;
  signals: RuntimeOperationSignal[];
  terminal: boolean;
  watchers: Map<string, { includeUpdates: boolean }>;
  frameQueue: Promise<void>;
  signalWaiters: Set<RuntimeOperationSignalWaiter>;
  cancellation: AbortController;
  reclaimed?: boolean;
};

export type DurableOperationRecord = {
  invocationId: string;
  deploymentId: string;
  callerSessionKey: string;
  invocationDigest: string;
  caller: VerifiedCaller;
  creatorPrincipalId: string;
  creatorBinding: AuthenticatedCaller["binding"];
  apiId: string;
  operation: string;
  input: unknown;
  telemetry?: { traceparent: string; tracestate?: string };
  revision: number;
  ownerInstanceId: string;
  ownerConnectionId: string;
  ownerEpoch: number;
  leaseExpiresAt: string;
  cancelRequestedAt?: string;
  transferGrant?: SendTransferGrant;
  /** Persisted upload attempt, separate from the public logical FileInfo key. */
  uploadState?: PersistedOperationUpload;
  sequence: number;
  signalSequence: number;
  signals: RuntimeOperationSignal[];
  snapshot: RuntimeOperationSnapshot;
};

export type RuntimeOperationAcceptedEnvelope = {
  kind: "accepted";
  ref: OperationRefData;
  snapshot: RuntimeOperationSnapshot;
  transfer?: SendTransferGrant;
};

export type RuntimeOperationControlRequest =
  | {
      action: "get" | "cancel";
      operationId: string;
    }
  | {
      action: "watch";
      operationId: string;
      includeUpdates?: boolean;
      observation?: {
        format: string;
        type: string;
        openId: string;
        receiveMaxPayloadBytes: number;
      };
    }
  | {
      action: "signal";
      operationId: string;
      signal: string;
      input?: JsonValue;
    };

export type RuntimeOperationController = {
  get(operationId: string): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  started(
    operationId: string,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  progress(
    operationId: string,
    progress: unknown,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  complete(
    operationId: string,
    output: unknown,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  fail(
    operationId: string,
    error: BaseError,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  cancel(operationId: string): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  signals(operationId: string): AsyncIterable<RuntimeOperationSignal>;
  nextSignal(
    operationId: string,
    name?: string,
  ): AsyncResult<RuntimeOperationSignal, BaseError>;
};

export function buildRuntimeOperationSnapshot(
  runtime: Pick<
    RuntimeOperationRecord,
    "id" | "service" | "operation" | "snapshot"
  >,
  state: RuntimeOperationState,
  patch?: Partial<RuntimeOperationSnapshot>,
): RuntimeOperationSnapshot {
  const updatedAt = new Date().toISOString();
  const completedAt =
    state === "completed" || state === "failed" || state === "cancelled"
      ? (patch?.completedAt ?? updatedAt)
      : patch?.completedAt;
  return {
    id: runtime.id,
    service: runtime.service,
    operation: runtime.operation,
    revision: patch?.revision ?? runtime.snapshot.revision + 1,
    state,
    createdAt: patch?.createdAt ?? runtime.snapshot.createdAt,
    updatedAt,
    ...(completedAt ? { completedAt } : {}),
    ...(patch?.progress !== undefined
      ? { progress: patch.progress }
      : runtime.snapshot.progress !== undefined
        ? { progress: runtime.snapshot.progress }
        : {}),
    ...(patch?.transfer !== undefined
      ? { transfer: patch.transfer }
      : runtime.snapshot.transfer !== undefined
        ? { transfer: runtime.snapshot.transfer }
        : {}),
    ...(patch?.output !== undefined
      ? { output: patch.output }
      : runtime.snapshot.output !== undefined
        ? { output: runtime.snapshot.output }
        : {}),
    ...(patch?.error
      ? { error: patch.error }
      : runtime.snapshot.error
        ? { error: runtime.snapshot.error }
        : {}),
  };
}

export function isRuntimeOperationSnapshot(
  value: unknown,
): value is RuntimeOperationSnapshot {
  return (
    !!value &&
    typeof value === "object" &&
    typeof (value as RuntimeOperationSnapshot).id === "string" &&
    typeof (value as RuntimeOperationSnapshot).service === "string" &&
    typeof (value as RuntimeOperationSnapshot).operation === "string" &&
    typeof (value as RuntimeOperationSnapshot).revision === "number" &&
    typeof (value as RuntimeOperationSnapshot).state === "string" &&
    typeof (value as RuntimeOperationSnapshot).createdAt === "string" &&
    typeof (value as RuntimeOperationSnapshot).updatedAt === "string"
  );
}

export function isTerminalRuntimeOperationSnapshot(
  value: unknown,
): value is RuntimeOperationSnapshot {
  return (
    isRuntimeOperationSnapshot(value) &&
    (value.state === "completed" ||
      value.state === "failed" ||
      value.state === "cancelled")
  );
}
