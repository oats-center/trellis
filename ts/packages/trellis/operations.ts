import { AsyncResult, err, isErr, ok, type Result } from "@qlever-llc/result";
import { ulid } from "ulid";

import { type JsonValue, parseUnknownSchema } from "./codec.ts";
import type { InferSchemaType } from "./participant.ts";
import {
  getBuiltinRpcError,
  OperationAlreadyTerminalError,
  OperationMismatchError,
  OperationNotFoundError,
  TransferError,
  TransportError,
  UnexpectedError,
} from "./errors/index.ts";
import type { FileInfo, SendTransferGrant, TransferBody } from "./transfer.ts";

type ActiveJobWaitTarget = {
  kind: "operation";
  id: string;
  operationId: string;
  service: string;
  type: string;
};

type ActiveJobWaitHook = <T>(
  target: ActiveJobWaitTarget,
  fn: () => Promise<T>,
) => Promise<T> | undefined;

let activeJobWaitHook: ActiveJobWaitHook | undefined;

/** @internal Registers the Jobs runtime hook used to track operation waits inside job handlers. */
export function setActiveJobWaitHook(hook: ActiveJobWaitHook): void {
  activeJobWaitHook = hook;
}

export type OperationState =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export type OperationRefData = {
  id: string;
  service: string;
  operation: string;
};

export type OperationSnapshot<TProgress = unknown, TOutput = unknown> = {
  id: string;
  service: string;
  operation: string;
  revision: number;
  state: OperationState;
  createdAt: string;
  updatedAt: string;
  completedAt?: string;
  progress?: TProgress;
  transfer?: OperationTransferProgress;
  output?: TOutput;
  error?: {
    type: string;
    message: string;
  };
};

export type OperationTransferProgress = {
  chunkIndex: number;
  chunkBytes: number;
  transferredBytes: number;
};

/** Errors returned when an operation control request violates lifecycle state. */
export type OperationLifecycleError =
  | OperationNotFoundError
  | OperationAlreadyTerminalError
  | OperationMismatchError;

/** Errors returned when an operation control request fails before producing a snapshot or ack. */
export type OperationControlError = TransportError | OperationLifecycleError;

export type TerminalOperation<TProgress = unknown, TOutput = unknown> =
  | (OperationSnapshot<TProgress, TOutput> & { state: "completed" })
  | (OperationSnapshot<TProgress, TOutput> & { state: "failed" })
  | (OperationSnapshot<TProgress, TOutput> & { state: "cancelled" });

export type OperationSignalAck<TProgress = unknown, TOutput = unknown> = {
  kind: "signal-accepted";
  operationId: string;
  signal: string;
  signalSequence: number;
  acceptedAt: string;
  snapshot: OperationSnapshot<TProgress, TOutput>;
};

export type CompletedTransfer<
  TDesc extends OperationShape,
  TProgress = OperationProgressOf<TDesc>,
  TOutput = OperationOutputOf<TDesc>,
> = {
  transferred: FileInfo;
  terminal: TerminalOperation<TProgress, TOutput>;
};

export type StartedTransfer<
  TDesc extends OperationShape,
  TProgress = OperationProgressOf<TDesc>,
  TOutput = OperationOutputOf<TDesc>,
  TUpdate = OperationUpdateOf<TDesc>,
> = {
  operation: OperationRef<
    TDesc,
    TProgress,
    TOutput,
    TUpdate
  >;
  wait(): AsyncResult<
    CompletedTransfer<TDesc, TProgress, TOutput>,
    OperationControlError | UnexpectedError | TransferError
  >;
};

export type OperationRef<
  TDesc extends OperationShape,
  TProgress = OperationProgressOf<TDesc>,
  TOutput = OperationOutputOf<TDesc>,
  TUpdate = OperationUpdateOf<TDesc>,
> = {
  id: string;
  service: string;
  operation: string;
  get(): AsyncResult<
    OperationSnapshot<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  >;
  wait(): AsyncResult<
    TerminalOperation<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  >;
  watch(options?: OperationWatchOptions): AsyncResult<
    AsyncIterable<OperationEvent<TProgress, TOutput, TUpdate>>,
    OperationControlError | UnexpectedError
  >;
  cancel(): AsyncResult<
    OperationSnapshot<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  >;
  signal(
    signal: string,
    input?: unknown,
  ): AsyncResult<
    OperationSignalAck<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  >;
};

/** Options controlling optional operation watch event classes. */
export type OperationWatchOptions = {
  /** Include transient typed update events. */
  updates?: boolean;
};

/** Options that identify an idempotent operation invocation. */
export type OperationStartOptions = {
  invocationId?: string;
};

export type AcceptedOperationEvent<TProgress = unknown, TOutput = unknown> = {
  type: "accepted";
  snapshot: OperationSnapshot<TProgress, TOutput>;
};

export type StartedOperationEvent<TProgress = unknown, TOutput = unknown> = {
  type: "started";
  snapshot: OperationSnapshot<TProgress, TOutput>;
};

export type TransferOperationSnapshot<TProgress = unknown, TOutput = unknown> =
  & OperationSnapshot<TProgress, TOutput>
  & { transfer: OperationTransferProgress };

export type TransferOperationEvent<TProgress = unknown, TOutput = unknown> = {
  type: "transfer";
  snapshot: TransferOperationSnapshot<TProgress, TOutput>;
  transfer: OperationTransferProgress;
};

export type ProgressOperationSnapshot<TProgress = unknown, TOutput = unknown> =
  & OperationSnapshot<TProgress, TOutput>
  & { progress: TProgress };

export type ProgressOperationEvent<TProgress = unknown, TOutput = unknown> = {
  type: "progress";
  snapshot: ProgressOperationSnapshot<TProgress, TOutput>;
  progress: TProgress;
};

/** A transient operation update paired with the unchanged current snapshot. */
export type UpdateOperationEvent<
  TProgress = unknown,
  TOutput = unknown,
  TUpdate = unknown,
> = {
  type: "update";
  update: TUpdate;
  snapshot: OperationSnapshot<TProgress, TOutput>;
};

export type CompletedOperationEvent<TProgress = unknown, TOutput = unknown> = {
  type: "completed";
  snapshot: TerminalOperation<TProgress, TOutput>;
};

export type FailedOperationEvent<TProgress = unknown, TOutput = unknown> = {
  type: "failed";
  snapshot: TerminalOperation<TProgress, TOutput>;
};

export type CancelledOperationEvent<TProgress = unknown, TOutput = unknown> = {
  type: "cancelled";
  snapshot: TerminalOperation<TProgress, TOutput>;
};

export type OperationEvent<
  TProgress = unknown,
  TOutput = unknown,
  TUpdate = unknown,
> =
  | AcceptedOperationEvent<TProgress, TOutput>
  | StartedOperationEvent<TProgress, TOutput>
  | TransferOperationEvent<TProgress, TOutput>
  | ProgressOperationEvent<TProgress, TOutput>
  | UpdateOperationEvent<TProgress, TOutput, TUpdate>
  | CompletedOperationEvent<TProgress, TOutput>
  | FailedOperationEvent<TProgress, TOutput>
  | CancelledOperationEvent<TProgress, TOutput>;

export type OperationObserverCallbacks<
  TProgress = unknown,
  TOutput = unknown,
  TUpdate = unknown,
> = {
  onAccepted?: (
    event: AcceptedOperationEvent<TProgress, TOutput>,
  ) => void | Promise<void>;
  onStarted?: (
    event: StartedOperationEvent<TProgress, TOutput>,
  ) => void | Promise<void>;
  onTransfer?: (
    event: TransferOperationEvent<TProgress, TOutput>,
  ) => void | Promise<void>;
  onProgress?: (
    event: ProgressOperationEvent<TProgress, TOutput>,
  ) => void | Promise<void>;
  onUpdate?: (
    event: UpdateOperationEvent<TProgress, TOutput, TUpdate>,
  ) => void | Promise<void>;
  onCompleted?: (
    event: CompletedOperationEvent<TProgress, TOutput>,
  ) => void | Promise<void>;
  onFailed?: (
    event: FailedOperationEvent<TProgress, TOutput>,
  ) => void | Promise<void>;
  onCancelled?: (
    event: CancelledOperationEvent<TProgress, TOutput>,
  ) => void | Promise<void>;
  onEvent?: (
    event: OperationEvent<TProgress, TOutput, TUpdate>,
  ) => void | Promise<void>;
};

interface OperationObserverBuilderBase<
  TBuilder,
  TProgress = unknown,
  TOutput = unknown,
  TUpdate = unknown,
> {
  onAccepted(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput>["onAccepted"]
    >,
  ): TBuilder;
  onStarted(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput>["onStarted"]
    >,
  ): TBuilder;
  onProgress(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput>["onProgress"]
    >,
  ): TBuilder;
  onUpdate(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput, TUpdate>["onUpdate"]
    >,
  ): TBuilder;
  onCompleted(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput>["onCompleted"]
    >,
  ): TBuilder;
  onFailed(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput>["onFailed"]
    >,
  ): TBuilder;
  onCancelled(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput>["onCancelled"]
    >,
  ): TBuilder;
  onEvent(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput, TUpdate>["onEvent"]
    >,
  ): TBuilder;
}

interface OperationInputBuilderBase<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
  TBuilder,
> extends OperationObserverBuilderBase<TBuilder, TProgress, TOutput, TUpdate> {
  start(
    callbacks?: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
    options?: OperationStartOptions,
  ): AsyncResult<
    OperationRef<TDesc, TProgress, TOutput, TUpdate>,
    OperationControlError | UnexpectedError
  >;
}

export interface TransferOperationBuilder<
  TDesc extends OperationShape,
  TProgress = OperationProgressOf<TDesc>,
  TOutput = OperationOutputOf<TDesc>,
  TUpdate = OperationUpdateOf<TDesc>,
> extends
  OperationObserverBuilderBase<
    TransferOperationBuilder<TDesc, TProgress, TOutput, TUpdate>,
    TProgress,
    TOutput,
    TUpdate
  > {
  onTransfer(
    handler: NonNullable<
      OperationObserverCallbacks<TProgress, TOutput>["onTransfer"]
    >,
  ): TransferOperationBuilder<TDesc, TProgress, TOutput, TUpdate>;
  start(
    callbacks?: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
    options?: OperationStartOptions,
  ): AsyncResult<
    StartedTransfer<TDesc, TProgress, TOutput, TUpdate>,
    OperationControlError | UnexpectedError | TransferError
  >;
}

export interface OperationInputBuilder<
  TDesc extends OperationShape,
  TProgress = OperationProgressOf<TDesc>,
  TOutput = OperationOutputOf<TDesc>,
  TUpdate = OperationUpdateOf<TDesc>,
> extends
  OperationInputBuilderBase<
    TDesc,
    TProgress,
    TOutput,
    TUpdate,
    OperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>
  > {
}

export interface TransferCapableOperationInputBuilder<
  TDesc extends OperationShape,
  TProgress = OperationProgressOf<TDesc>,
  TOutput = OperationOutputOf<TDesc>,
  TUpdate = OperationUpdateOf<TDesc>,
> extends
  OperationInputBuilderBase<
    TDesc,
    TProgress,
    TOutput,
    TUpdate,
    TransferCapableOperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>
  > {
  transfer(
    body: TransferBody,
  ): TransferOperationBuilder<TDesc, TProgress, TOutput>;
}

type OperationAcceptedEnvelope<TProgress = unknown, TOutput = unknown> = {
  kind: "accepted";
  ref: OperationRefData;
  snapshot: OperationSnapshot<TProgress, TOutput>;
  transfer?: SendTransferGrant;
};

type OperationSnapshotFrame<TProgress = unknown, TOutput = unknown> = {
  kind: "snapshot";
  snapshot: OperationSnapshot<TProgress, TOutput>;
};

type OperationSignalAckFrame<TProgress = unknown, TOutput = unknown> =
  OperationSignalAck<TProgress, TOutput>;

type OperationControlErrorFrame = {
  kind: "error";
  error: {
    type: string;
    message: string;
  };
};

type OperationShape = {
  subject: string;
  input: unknown;
  progress?: unknown;
  update?: unknown;
  output?: unknown;
  transfer?: {
    store?: string;
    key?: `/${string}`;
    contentType?: `/${string}`;
    metadata?: `/${string}`;
    expiresInMs?: number;
    maxBytes?: number;
  };
  cancel?: boolean;
};

type OperationInputOf<TDesc extends OperationShape> = InferSchemaType<
  TDesc["input"]
>;
type OperationProgressOf<TDesc extends OperationShape> =
  TDesc["progress"] extends undefined ? unknown
    : InferSchemaType<NonNullable<TDesc["progress"]>>;
type OperationUpdateOf<TDesc extends OperationShape> = TDesc["update"] extends
  undefined ? unknown
  : InferSchemaType<NonNullable<TDesc["update"]>>;
type OperationOutputOf<TDesc extends OperationShape> = TDesc["output"] extends
  undefined ? unknown
  : InferSchemaType<NonNullable<TDesc["output"]>>;

export interface OperationTransport {
  requestJson(
    subject: string,
    body: JsonValue,
  ): AsyncResult<JsonValue, TransportError | UnexpectedError>;
  watchJson(
    subject: string,
    body: JsonValue,
  ): AsyncResult<
    AsyncIterable<Result<JsonValue, TransportError | UnexpectedError>>,
    TransportError | UnexpectedError
  >;
  putTransfer(
    grant: SendTransferGrant,
    body: TransferBody,
  ): AsyncResult<FileInfo, TransferError>;
}

function operationRequestBody(input: unknown, invocationId: string): JsonValue {
  return { invocationId, input: input as JsonValue };
}

function encodeOperationInput(schema: unknown, value: unknown): JsonValue {
  if (
    value === undefined || value === null || schema === undefined ||
    schema === null
  ) {
    return value as JsonValue;
  }
  if (typeof Reflect.get(schema, "encode") === "function") {
    return (schema as { encode(value: unknown): unknown }).encode(
      value,
    ) as JsonValue;
  }
  return value as JsonValue;
}

export function controlSubject(subject: string): string {
  return `${subject}.control`;
}

function isTerminalState(state: OperationState): boolean {
  return state === "completed" || state === "failed" || state === "cancelled";
}

function createTransportError(args: {
  code: string;
  message: string;
  hint: string;
  context?: Record<string, unknown>;
  cause?: unknown;
}): TransportError {
  return new TransportError({
    code: args.code,
    message: args.message,
    hint: args.hint,
    cause: args.cause,
    context: {
      ...(args.context ?? {}),
      ...(args.cause instanceof Error
        ? { causeName: args.cause.name, causeMessage: args.cause.message }
        : args.cause === undefined
        ? {}
        : { cause: String(args.cause) }),
    },
  });
}

function snapshotToEvent<TProgress, TOutput, TUpdate = unknown>(
  snapshot: OperationSnapshot<TProgress, TOutput>,
): OperationEvent<TProgress, TOutput, TUpdate> {
  switch (snapshot.state) {
    case "pending":
      return { type: "accepted", snapshot };
    case "running":
      return { type: "started", snapshot };
    case "completed":
      return {
        type: "completed",
        snapshot: snapshot as TerminalOperation<TProgress, TOutput>,
      };
    case "failed":
      return {
        type: "failed",
        snapshot: snapshot as TerminalOperation<TProgress, TOutput>,
      };
    case "cancelled":
      return {
        type: "cancelled",
        snapshot: snapshot as TerminalOperation<TProgress, TOutput>,
      };
  }
}

function isTerminalEvent<TProgress, TOutput, TUpdate>(
  event: OperationEvent<TProgress, TOutput, TUpdate>,
): event is Extract<
  OperationEvent<TProgress, TOutput, TUpdate>,
  { type: "completed" | "failed" | "cancelled" }
> {
  return event.type === "completed" || event.type === "failed" ||
    event.type === "cancelled";
}

function normalizeOperationEvent<TProgress, TOutput, TUpdate>(
  event: OperationEvent<TProgress, TOutput, TUpdate>,
): Result<OperationEvent<TProgress, TOutput, TUpdate>, TransportError> {
  try {
    switch (event.type) {
      case "transfer": {
        const transfer = event.transfer ?? event.snapshot.transfer;
        if (!transfer) {
          throw new Error("transfer event is missing transfer progress");
        }
        return ok({
          type: "transfer",
          transfer,
          snapshot: {
            ...event.snapshot,
            transfer,
          },
        });
      }

      case "progress": {
        const progress = event.progress ?? event.snapshot.progress;
        if (progress === undefined) {
          throw new Error("progress event is missing progress payload");
        }
        return ok({
          type: "progress",
          progress,
          snapshot: {
            ...event.snapshot,
            progress,
          },
        });
      }

      default:
        return ok(event);
    }
  } catch (cause) {
    return err(createTransportError({
      code: "trellis.operation.invalid_event",
      message: "Trellis returned an invalid operation event.",
      hint:
        "Retry the operation watch. If it keeps failing, reconnect to Trellis and try again.",
      cause,
    }));
  }
}

async function dispatchObservedOperationEvent<TProgress, TOutput, TUpdate>(
  options: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
  event: OperationEvent<TProgress, TOutput, TUpdate>,
): Promise<void> {
  switch (event.type) {
    case "accepted":
      await options.onAccepted?.(event);
      break;
    case "started":
      await options.onStarted?.(event);
      break;
    case "transfer":
      await options.onTransfer?.(event);
      break;
    case "progress":
      await options.onProgress?.(event);
      break;
    case "update":
      await options.onUpdate?.(event);
      break;
    case "completed":
      await options.onCompleted?.(event);
      break;
    case "failed":
      await options.onFailed?.(event);
      break;
    case "cancelled":
      await options.onCancelled?.(event);
      break;
  }

  await options.onEvent?.(event);
}

function hasObserverCallbacks<TProgress, TOutput, TUpdate>(
  options: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
): boolean {
  return Boolean(
    options.onAccepted ||
      options.onStarted ||
      options.onTransfer ||
      options.onProgress ||
      options.onUpdate ||
      options.onCompleted ||
      options.onFailed ||
      options.onCancelled ||
      options.onEvent,
  );
}

function decodeAcceptedEnvelope<TProgress, TOutput>(
  value: JsonValue,
): Result<OperationAcceptedEnvelope<TProgress, TOutput>, TransportError> {
  try {
    const envelope = value as OperationAcceptedEnvelope<TProgress, TOutput>;
    if (envelope?.kind !== "accepted" || !envelope.ref || !envelope.snapshot) {
      throw new Error(
        `Expected accepted operation envelope, got ${JSON.stringify(value)}`,
      );
    }
    return ok(envelope);
  } catch (cause) {
    return err(createTransportError({
      code: "trellis.operation.invalid_accept",
      message: "Trellis returned an invalid operation start response.",
      hint:
        "Retry starting the operation. If it keeps failing, reconnect to Trellis and try again.",
      cause,
    }));
  }
}

function decodeSnapshotFrame<TProgress, TOutput>(
  value: JsonValue,
): Result<OperationSnapshotFrame<TProgress, TOutput>, OperationControlError> {
  try {
    if (isOperationControlErrorFrame(value)) {
      return err(controlFrameToError(value));
    }

    const frame = value as OperationSnapshotFrame<TProgress, TOutput>;
    if (frame?.kind !== "snapshot" || !frame.snapshot) {
      throw new Error("Expected snapshot operation frame");
    }
    return ok(frame);
  } catch (cause) {
    return err(createTransportError({
      code: "trellis.operation.invalid_snapshot",
      message: "Trellis returned an invalid operation snapshot.",
      hint:
        "Retry the operation request. If it keeps failing, reconnect to Trellis and try again.",
      cause,
    }));
  }
}

function decodeSignalAckFrame<TProgress, TOutput>(
  value: JsonValue,
): Result<OperationSignalAckFrame<TProgress, TOutput>, OperationControlError> {
  try {
    if (isOperationControlErrorFrame(value)) {
      return err(controlFrameToError(value));
    }

    const frame = value as OperationSignalAckFrame<TProgress, TOutput>;
    if (
      frame?.kind !== "signal-accepted" ||
      typeof frame.operationId !== "string" ||
      typeof frame.signal !== "string" ||
      typeof frame.signalSequence !== "number" ||
      typeof frame.acceptedAt !== "string" ||
      !frame.snapshot
    ) {
      throw new Error(
        `Expected signal-accepted operation frame, received ${
          JSON.stringify(value)
        }`,
      );
    }
    return ok(frame);
  } catch (cause) {
    return err(createTransportError({
      code: "trellis.operation.invalid_signal_ack",
      message: "Trellis returned an invalid operation signal response.",
      hint:
        "Retry the operation signal. If it keeps failing, reconnect to Trellis and try again.",
      cause,
    }));
  }
}

class RuntimeOperationRef<
  TDesc extends OperationShape,
  TProgress = OperationProgressOf<TDesc>,
  TOutput = OperationOutputOf<TDesc>,
  TUpdate = OperationUpdateOf<TDesc>,
> {
  readonly id: string;
  readonly service: string;
  readonly operation: string;

  readonly #transport: OperationTransport;
  readonly #descriptor: TDesc;
  readonly #acceptedTransfer?: SendTransferGrant;

  constructor(
    transport: OperationTransport,
    descriptor: TDesc,
    ref: OperationRefData,
    acceptedTransfer?: SendTransferGrant,
  ) {
    this.#transport = transport;
    this.#descriptor = descriptor;
    this.id = ref.id;
    this.service = ref.service;
    this.operation = ref.operation;
    this.#acceptedTransfer = acceptedTransfer;
  }

  #decodePayload(schema: unknown, value: unknown): unknown {
    return decodeOperationPayload(schema, value);
  }

  #decodeSnapshot(
    snapshot: OperationSnapshot<TProgress, TOutput> | undefined,
  ): OperationSnapshot<TProgress, TOutput> | undefined {
    return decodeOperationSnapshot(this.#descriptor, snapshot);
  }

  #decodeEvent(
    event: OperationEvent<TProgress, TOutput, TUpdate>,
  ): OperationEvent<TProgress, TOutput, TUpdate> {
    const descriptor = this.#descriptor as {
      update?: unknown;
      progress?: unknown;
    };
    const decoded = (event as { snapshot?: unknown }).snapshot === undefined
      ? { ...event }
      : { ...event, snapshot: this.#decodeSnapshot(event.snapshot) };
    if (event.type === "progress") {
      return {
        ...decoded,
        progress: this.#decodePayload(
          descriptor.progress,
          event.progress,
        ) as TProgress,
      } as OperationEvent<TProgress, TOutput, TUpdate>;
    }
    if (event.type === "update") {
      return {
        ...decoded,
        update: this.#decodePayload(
          descriptor.update,
          event.update,
        ) as TUpdate,
      } as OperationEvent<TProgress, TOutput, TUpdate>;
    }
    return decoded as OperationEvent<TProgress, TOutput, TUpdate>;
  }

  get(): AsyncResult<
    OperationSnapshot<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  > {
    return this.#controlSnapshot("get");
  }

  wait(): AsyncResult<
    TerminalOperation<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  > {
    return AsyncResult.from((async () => {
      const initialTerminal = await this.#terminalSnapshotFromGet().take();
      if (isErr(initialTerminal)) {
        return initialTerminal;
      }
      if (initialTerminal !== null) {
        return ok(initialTerminal);
      }

      const eventsValue = await this.watch().take();
      if (isErr(eventsValue)) {
        const terminal = await this.#terminalSnapshotFromGet().take();
        if (!isErr(terminal) && terminal !== null) {
          return ok(terminal);
        }
        return eventsValue;
      }

      try {
        for await (const event of eventsValue) {
          if (isTerminalEvent(event)) {
            return ok(event.snapshot);
          }
        }
      } catch (cause) {
        const terminal = await this.#terminalSnapshotFromGet().take();
        if (!isErr(terminal) && terminal !== null) {
          return ok(terminal);
        }
        return err(
          cause instanceof TransportError || cause instanceof UnexpectedError ||
            isOperationLifecycleError(cause)
            ? cause
            : new UnexpectedError({ cause }),
        );
      }

      const terminal = await this.#terminalSnapshotFromGet().take();
      if (!isErr(terminal) && terminal !== null) {
        return ok(terminal);
      }

      return err(createTransportError({
        code: "trellis.operation.watch_closed",
        message: "Trellis operation watch ended before a terminal snapshot.",
        hint:
          "Retry the operation wait. If it keeps happening, reconnect to Trellis and try again.",
        context: { operationId: this.id, operation: this.operation },
      }));
    })());
  }

  #terminalSnapshotFromGet(): AsyncResult<
    TerminalOperation<TProgress, TOutput> | null,
    OperationControlError | UnexpectedError
  > {
    return AsyncResult.from((async () => {
      const snapshotValue = await this.#controlSnapshot("get").take();
      if (isErr(snapshotValue)) {
        return snapshotValue;
      }
      if (isTerminalState(snapshotValue.state)) {
        return ok(snapshotValue as TerminalOperation<TProgress, TOutput>);
      }
      return ok(null);
    })());
  }

  cancel(): AsyncResult<
    OperationSnapshot<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  > {
    return this.#controlSnapshot("cancel");
  }

  signal(
    signal: string,
    input?: unknown,
  ): AsyncResult<
    OperationSignalAck<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  > {
    return AsyncResult.from((async (): Promise<
      Result<
        OperationSignalAck<TProgress, TOutput>,
        OperationControlError | UnexpectedError
      >
    > => {
      const body: {
        action: "signal";
        operationId: string;
        signal: string;
        input?: JsonValue;
      } = {
        action: "signal",
        operationId: this.id,
        signal,
      };
      if (input !== undefined) {
        const signals =
          (this.#descriptor as { signals?: Record<string, unknown> })
            .signals;
        body.input = encodeOperationInput(signals?.[signal], input);
      }

      const responseValue = await this.#transport.requestJson(
        controlSubject(this.#descriptor.subject),
        body,
      ).take();
      if (isErr(responseValue)) {
        return err(responseValue.error);
      }

      const ack = decodeSignalAckFrame<TProgress, TOutput>(responseValue)
        .take();
      if (isErr(ack)) {
        return ack;
      }
      return ok({ ...ack, snapshot: this.#decodeSnapshot(ack.snapshot)! });
    })());
  }

  startTransfer(body: TransferBody): AsyncResult<FileInfo, TransferError> {
    const grant = this.#acceptedTransfer;
    if (!grant) {
      return AsyncResult.err(
        new TransferError({
          operation: "transfer",
          context: { reason: "missing_transfer" },
        }),
      );
    }
    return this.#transport.putTransfer(grant, body);
  }

  watch(options: OperationWatchOptions = {}): AsyncResult<
    AsyncIterable<OperationEvent<TProgress, TOutput, TUpdate>>,
    OperationControlError | UnexpectedError
  > {
    return AsyncResult.from((async () => {
      const rawIterable = await this.#transport.watchJson(
        controlSubject(this.#descriptor.subject),
        {
          action: "watch",
          operationId: this.id,
          ...(options.updates ? { includeUpdates: true } : {}),
        },
      ).take();
      if (isErr(rawIterable)) {
        return err(rawIterable.error);
      }
      const iterable = rawIterable as AsyncIterable<
        Result<JsonValue, TransportError | UnexpectedError>
      >;
      const decodeEvent = this.#decodeEvent.bind(this);

      async function* events() {
        for await (const frame of iterable) {
          const frameValue = frame.take();
          if (isErr(frameValue)) {
            throw frameValue.error;
          }
          const decoded = decodeWatchFrame<TProgress, TOutput, TUpdate>(
            frameValue,
          );
          const decodedValue = decoded.take();
          if (isErr(decodedValue)) {
            throw decodedValue.error;
          }
          if (decodedValue === null) {
            continue;
          }
          const normalized = normalizeOperationEvent(decodedValue).take();
          if (isErr(normalized)) {
            throw normalized.error;
          }
          const event = decodeEvent(normalized);
          yield event;
          if (isTerminalEvent(event)) {
            break;
          }
        }
      }

      return ok(events());
    })());
  }

  #controlSnapshot(
    action: "get" | "cancel",
  ): AsyncResult<
    OperationSnapshot<TProgress, TOutput>,
    OperationControlError | UnexpectedError
  > {
    return AsyncResult.from((async () => {
      const responseValue = await this.#transport.requestJson(
        controlSubject(this.#descriptor.subject),
        {
          action,
          operationId: this.id,
        },
      ).take();
      if (isErr(responseValue)) {
        return err(responseValue.error);
      }

      const frame = decodeSnapshotFrame<TProgress, TOutput>(
        responseValue,
      ).take();
      if (isErr(frame)) {
        return frame;
      }
      return ok(this.#decodeSnapshot(frame.snapshot)!);
    })());
  }
}

function decodeWatchFrame<TProgress, TOutput, TUpdate>(
  value: JsonValue,
): Result<
  OperationEvent<TProgress, TOutput, TUpdate> | null,
  OperationControlError
> {
  try {
    if (
      value && typeof value === "object" &&
      (value as { kind?: string }).kind === "keepalive"
    ) {
      return ok(null);
    }

    if (isOperationControlErrorFrame(value)) {
      return err(controlFrameToError(value));
    }

    const frame = value as
      | { kind: "snapshot"; snapshot: OperationSnapshot<TProgress, TOutput> }
      | {
        kind: "event";
        event: OperationEvent<TProgress, TOutput, TUpdate>;
      };

    if (
      (frame as { kind?: string }).kind === "snapshot" && "snapshot" in frame
    ) {
      return ok(snapshotToEvent<TProgress, TOutput, TUpdate>(frame.snapshot));
    }
    if ((frame as { kind?: string }).kind === "event" && "event" in frame) {
      return ok(frame.event);
    }

    throw new Error("Expected snapshot, event, or keepalive frame");
  } catch (cause) {
    return err(createTransportError({
      code: "trellis.operation.invalid_frame",
      message: "Trellis returned an invalid operation watch frame.",
      hint:
        "Retry the operation watch. If it keeps failing, reconnect to Trellis and try again.",
      cause,
    }));
  }
}

type OperationWatchObservation<TProgress, TOutput> = {
  task?: Promise<
    Result<
      TerminalOperation<TProgress, TOutput>,
      OperationControlError | UnexpectedError
    >
  >;
  close?: () => Promise<void>;
};

type ObservedWatchOptions<TProgress, TOutput, TUpdate> = {
  ready?: Promise<void>;
  skipEvent?: (event: OperationEvent<TProgress, TOutput, TUpdate>) => boolean;
};

type InvokedOperation<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
> = {
  accepted: AcceptedOperationEvent<TProgress, TOutput>;
  operation: RuntimeOperationRef<TDesc, TProgress, TOutput, TUpdate>;
};

function invokeOperation<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
>(
  transport: OperationTransport,
  descriptor: TDesc,
  input: unknown,
  invocationId: string,
): AsyncResult<
  InvokedOperation<TDesc, TProgress, TOutput, TUpdate>,
  TransportError | UnexpectedError
> {
  return AsyncResult.from((async () => {
    const responseValue = await transport.requestJson(
      descriptor.subject,
      operationRequestBody(
        encodeOperationInput(descriptor.input, input),
        invocationId,
      ),
    ).take();
    if (isErr(responseValue)) {
      return responseValue;
    }

    const envelope = decodeAcceptedEnvelope<TProgress, TOutput>(responseValue)
      .take();
    if (isErr(envelope)) {
      return envelope;
    }

    return ok({
      accepted: {
        type: "accepted",
        snapshot: decodeOperationSnapshot(descriptor, envelope.snapshot)!,
      },
      operation: new RuntimeOperationRef<TDesc, TProgress, TOutput, TUpdate>(
        transport,
        descriptor,
        envelope.ref,
        envelope.transfer,
      ),
    });
  })());
}

function beginObservedWatch<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
>(
  operation: RuntimeOperationRef<TDesc, TProgress, TOutput, TUpdate>,
  callbacks: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
  options: ObservedWatchOptions<TProgress, TOutput, TUpdate> = {},
): AsyncResult<
  OperationWatchObservation<TProgress, TOutput>,
  OperationControlError | UnexpectedError
> {
  if (!hasObserverCallbacks(callbacks)) {
    return AsyncResult.ok({});
  }

  return AsyncResult.from((async () => {
    const watchValue = await operation.watch({
      updates: callbacks.onUpdate !== undefined,
    }).take();
    if (isErr(watchValue)) {
      return watchValue;
    }

    const iterator = watchValue[Symbol.asyncIterator]();
    const close = async () => {
      await iterator.return?.();
    };

    const task = (async (): Promise<
      Result<
        TerminalOperation<TProgress, TOutput>,
        OperationControlError | UnexpectedError
      >
    > => {
      try {
        await options.ready;

        while (true) {
          const next = await iterator.next();
          if (next.done) {
            break;
          }

          const event = next.value;
          if (options.skipEvent?.(event)) {
            continue;
          }
          try {
            await dispatchObservedOperationEvent(callbacks, event);
          } catch (cause) {
            return err(toObservedCallbackError(cause));
          }
          if (isTerminalEvent(event)) {
            await close();
            return ok(
              event.snapshot as TerminalOperation<TProgress, TOutput>,
            );
          }
        }

        return err(createTransportError({
          code: "trellis.operation.watch_incomplete",
          message: "Trellis ended the operation watch before completion.",
          hint:
            "Retry watching the operation. If it keeps happening, reconnect to Trellis and try again.",
          cause: new Error("operation watch ended before terminal event"),
        }));
      } catch (cause) {
        return err(
          cause instanceof TransportError || cause instanceof UnexpectedError ||
            isOperationLifecycleError(cause)
            ? cause
            : new UnexpectedError({ cause }),
        );
      }
    })();

    return ok({ task, close });
  })());
}

function startObservedOperation<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
>(
  transport: OperationTransport,
  descriptor: TDesc,
  input: unknown,
  callbacks: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
  options?: OperationStartOptions,
): AsyncResult<
  OperationRef<TDesc, TProgress, TOutput, TUpdate>,
  OperationControlError | UnexpectedError
> {
  return AsyncResult.from((async () => {
    const startedValue = await invokeOperation<
      TDesc,
      TProgress,
      TOutput,
      TUpdate
    >(
      transport,
      descriptor,
      input,
      options?.invocationId ?? ulid(),
    ).take();
    if (isErr(startedValue)) {
      return startedValue;
    }

    const ready = deferred<void>();

    const observedValue = await beginObservedWatch(
      startedValue.operation,
      callbacks,
      {
        ready: ready.promise,
        skipEvent: createAcceptedReplayFilter(startedValue.accepted),
      },
    ).take();
    const observation = isErr(observedValue) ? {} : observedValue;

    const accepted = await dispatchOperationEventResult(
      callbacks,
      startedValue.accepted,
    );
    if (accepted.isErr()) {
      if (!isErr(observedValue)) {
        ready.resolve();
        await observation.close?.();
      }
      return ok(createPublicOperationRef(
        startedValue.operation,
        failedObservation(accepted.error),
      ));
    }

    if (!isErr(observedValue)) {
      ready.resolve();
    }
    return ok(createPublicOperationRef(startedValue.operation, observation));
  })());
}

function startObservedTransfer<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
>(
  transport: OperationTransport,
  descriptor: TDesc,
  input: unknown,
  body: TransferBody,
  callbacks: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
  options?: OperationStartOptions,
): AsyncResult<
  StartedTransfer<TDesc, TProgress, TOutput, TUpdate>,
  OperationControlError | UnexpectedError | TransferError
> {
  return AsyncResult.from((async () => {
    const startedValue = await invokeOperation<
      TDesc,
      TProgress,
      TOutput,
      TUpdate
    >(
      transport,
      descriptor,
      input,
      options?.invocationId ?? ulid(),
    ).take();
    if (isErr(startedValue)) {
      return startedValue;
    }

    const operation = startedValue.operation;
    const ready = deferred<void>();
    const observedValue = await beginObservedWatch(operation, callbacks, {
      ready: ready.promise,
      skipEvent: createAcceptedReplayFilter(startedValue.accepted),
    }).take();
    const observation = isErr(observedValue) ? {} : observedValue;

    const accepted = await dispatchOperationEventResult(
      callbacks,
      startedValue.accepted,
    );
    if (accepted.isErr()) {
      if (!isErr(observedValue)) {
        ready.resolve();
        await observation.close?.();
      }
    }

    if (!isErr(observedValue)) {
      ready.resolve();
    }

    const transferredValue = await operation.startTransfer(body).take();
    if (isErr(transferredValue)) {
      await observation.close?.();
      return transferredValue;
    }

    const publicOperation = createPublicOperationRef(
      operation,
      accepted.isErr() ? failedObservation(accepted.error) : observation,
    );

    return ok({
      operation: publicOperation,
      wait: () =>
        AsyncResult.from((async () => {
          const terminalValue = await publicOperation.wait().take();
          if (isErr(terminalValue)) {
            return terminalValue;
          }

          return ok({
            transferred: transferredValue,
            terminal: terminalValue,
          });
        })()),
    });
  })());
}

function createObservedOperationRef<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
>(
  operation: RuntimeOperationRef<TDesc, TProgress, TOutput, TUpdate>,
  observation: OperationWatchObservation<TProgress, TOutput>,
): OperationRef<TDesc, TProgress, TOutput, TUpdate> {
  return createPublicOperationRef(operation, observation);
}

function createPublicOperationRef<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
>(
  operation: RuntimeOperationRef<TDesc, TProgress, TOutput, TUpdate>,
  observation: OperationWatchObservation<TProgress, TOutput>,
): OperationRef<TDesc, TProgress, TOutput, TUpdate> {
  const base = {
    id: operation.id,
    service: operation.service,
    operation: operation.operation,
    get: () => operation.get(),
    wait: () =>
      AsyncResult.from((async () => {
        const waited = activeJobWaitHook?.({
          kind: "operation",
          id: operation.id,
          operationId: operation.id,
          service: operation.service,
          type: operation.operation,
        }, async () => {
          if (observation.task) {
            const terminal = await observation.task;
            const terminalValue = terminal.take();
            if (!isErr(terminalValue)) {
              return ok(terminalValue);
            }
            if (isObservedCallbackError(terminalValue.error)) {
              return terminalValue;
            }
          }

          return await operation.wait();
        });
        if (waited) return await waited;

        if (observation.task) {
          const terminal = await observation.task;
          const terminalValue = terminal.take();
          if (!isErr(terminalValue)) {
            return ok(terminalValue);
          }
          if (isObservedCallbackError(terminalValue.error)) {
            return terminalValue;
          }
        }

        return await operation.wait();
      })()),
    watch: (options?: OperationWatchOptions) => operation.watch(options),
    cancel: () => operation.cancel(),
    signal: (signal: string, input?: unknown) =>
      operation.signal(signal, input),
  };

  return base as OperationRef<TDesc, TProgress, TOutput, TUpdate>;
}

function createOperationInputBuilder<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
>(
  transport: OperationTransport,
  descriptor: TDesc,
  input: unknown,
  callbacks: OperationObserverCallbacks<TProgress, TOutput, TUpdate> = {},
): TDesc["transfer"] extends undefined
  ? OperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>
  : TransferCapableOperationInputBuilder<TDesc, TProgress, TOutput, TUpdate> {
  const rebuild = (
    nextCallbacks: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
  ) =>
    createOperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>(
      transport,
      descriptor,
      input,
      nextCallbacks,
    );

  const baseBuilder = {
    onAccepted(
      handler: NonNullable<
        OperationObserverCallbacks<TProgress, TOutput>["onAccepted"]
      >,
    ) {
      return rebuild({ ...callbacks, onAccepted: handler });
    },
    onStarted(
      handler: NonNullable<
        OperationObserverCallbacks<TProgress, TOutput>["onStarted"]
      >,
    ) {
      return rebuild({ ...callbacks, onStarted: handler });
    },
    onProgress(
      handler: NonNullable<
        OperationObserverCallbacks<TProgress, TOutput>["onProgress"]
      >,
    ) {
      return rebuild({ ...callbacks, onProgress: handler });
    },
    onUpdate(
      handler: NonNullable<
        OperationObserverCallbacks<TProgress, TOutput, TUpdate>["onUpdate"]
      >,
    ) {
      return rebuild({ ...callbacks, onUpdate: handler });
    },
    onCompleted(
      handler: NonNullable<
        OperationObserverCallbacks<TProgress, TOutput>["onCompleted"]
      >,
    ) {
      return rebuild({ ...callbacks, onCompleted: handler });
    },
    onFailed(
      handler: NonNullable<
        OperationObserverCallbacks<TProgress, TOutput>["onFailed"]
      >,
    ) {
      return rebuild({ ...callbacks, onFailed: handler });
    },
    onCancelled(
      handler: NonNullable<
        OperationObserverCallbacks<TProgress, TOutput>["onCancelled"]
      >,
    ) {
      return rebuild({ ...callbacks, onCancelled: handler });
    },
    onEvent(
      handler: NonNullable<
        OperationObserverCallbacks<TProgress, TOutput, TUpdate>["onEvent"]
      >,
    ) {
      return rebuild({ ...callbacks, onEvent: handler });
    },
    start(
      startCallbacks?: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
      options?: OperationStartOptions,
    ) {
      return startObservedOperation<TDesc, TProgress, TOutput, TUpdate>(
        transport,
        descriptor,
        input,
        { ...callbacks, ...startCallbacks },
        options,
      );
    },
  } satisfies OperationInputBuilderBase<
    TDesc,
    TProgress,
    TOutput,
    TUpdate,
    OperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>
  >;

  if (descriptor.transfer) {
    return {
      ...baseBuilder,
      transfer(body: TransferBody) {
        return createTransferOperationBuilder<
          TDesc,
          TProgress,
          TOutput,
          TUpdate
        >(
          transport,
          descriptor,
          input,
          body,
          callbacks,
        );
      },
    } as TDesc["transfer"] extends undefined
      ? OperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>
      : TransferCapableOperationInputBuilder<
        TDesc,
        TProgress,
        TOutput,
        TUpdate
      >;
  }

  return baseBuilder as TDesc["transfer"] extends undefined
    ? OperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>
    : TransferCapableOperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>;
}

function createTransferOperationBuilder<
  TDesc extends OperationShape,
  TProgress,
  TOutput,
  TUpdate,
>(
  transport: OperationTransport,
  descriptor: TDesc,
  input: unknown,
  body: TransferBody,
  callbacks: OperationObserverCallbacks<TProgress, TOutput, TUpdate> = {},
): TransferOperationBuilder<TDesc, TProgress, TOutput, TUpdate> {
  const rebuild = (
    nextCallbacks: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
  ) =>
    createTransferOperationBuilder<TDesc, TProgress, TOutput, TUpdate>(
      transport,
      descriptor,
      input,
      body,
      nextCallbacks,
    );

  return {
    onAccepted(handler) {
      return rebuild({ ...callbacks, onAccepted: handler });
    },
    onStarted(handler) {
      return rebuild({ ...callbacks, onStarted: handler });
    },
    onTransfer(handler) {
      return rebuild({ ...callbacks, onTransfer: handler });
    },
    onProgress(handler) {
      return rebuild({ ...callbacks, onProgress: handler });
    },
    onUpdate(handler) {
      return rebuild({ ...callbacks, onUpdate: handler });
    },
    onCompleted(handler) {
      return rebuild({ ...callbacks, onCompleted: handler });
    },
    onFailed(handler) {
      return rebuild({ ...callbacks, onFailed: handler });
    },
    onCancelled(handler) {
      return rebuild({ ...callbacks, onCancelled: handler });
    },
    onEvent(handler) {
      return rebuild({ ...callbacks, onEvent: handler });
    },
    start(
      startCallbacks?: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
      options?: OperationStartOptions,
    ) {
      return startObservedTransfer<TDesc, TProgress, TOutput, TUpdate>(
        transport,
        descriptor,
        input,
        body,
        { ...callbacks, ...startCallbacks },
        options,
      );
    },
  };
}

export class OperationInvoker<
  TDesc extends OperationShape,
  TInput = OperationInputOf<TDesc>,
  TProgress = OperationProgressOf<TDesc>,
  TOutput = OperationOutputOf<TDesc>,
  TUpdate = OperationUpdateOf<TDesc>,
> {
  readonly #transport: OperationTransport;
  readonly #descriptor: TDesc;

  constructor(transport: OperationTransport, descriptor: TDesc) {
    this.#transport = transport;
    this.#descriptor = descriptor;
  }

  resume(
    ref: OperationRefData,
  ): OperationRef<TDesc, TProgress, TOutput, TUpdate> {
    return createPublicOperationRef(
      new RuntimeOperationRef<TDesc, TProgress, TOutput, TUpdate>(
        this.#transport,
        this.#descriptor,
        ref,
      ),
      {},
    );
  }

  start(
    input: TInput,
    callbacks?: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
  ): AsyncResult<
    OperationRef<TDesc, TProgress, TOutput, TUpdate>,
    OperationControlError | UnexpectedError
  > {
    return createOperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>(
      this.#transport,
      this.#descriptor,
      input as OperationInputOf<TDesc>,
    ).start(callbacks);
  }

  input(
    input: TInput,
  ): TDesc["transfer"] extends undefined
    ? OperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>
    : TransferCapableOperationInputBuilder<TDesc, TProgress, TOutput, TUpdate> {
    return createOperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>(
      this.#transport,
      this.#descriptor,
      input as OperationInputOf<TDesc>,
    ) as TDesc["transfer"] extends undefined
      ? OperationInputBuilder<TDesc, TProgress, TOutput, TUpdate>
      : TransferCapableOperationInputBuilder<
        TDesc,
        TProgress,
        TOutput,
        TUpdate
      >;
  }
}

function decodeOperationPayload(schema: unknown, value: unknown): unknown {
  if (
    value === undefined || value === null || schema === undefined ||
    schema === null
  ) {
    return value;
  }
  if (typeof Reflect.get(schema, "decode") === "function") {
    return (schema as { decode(value: unknown): unknown }).decode(value);
  }
  return value;
}

function decodeOperationSnapshot<TProgress, TOutput>(
  descriptor: { progress?: unknown; output?: unknown },
  snapshot: OperationSnapshot<TProgress, TOutput> | undefined,
): OperationSnapshot<TProgress, TOutput> | undefined {
  if (!snapshot || typeof snapshot !== "object") {
    return snapshot;
  }
  return {
    ...snapshot,
    ...(snapshot.progress === undefined ? {} : {
      progress: decodeOperationPayload(
        descriptor.progress,
        snapshot.progress,
      ) as TProgress,
    }),
    ...(snapshot.output === undefined ? {} : {
      output: decodeOperationPayload(
        descriptor.output,
        snapshot.output,
      ) as TOutput,
    }),
  };
}

function isOperationControlErrorFrame(
  value: JsonValue,
): value is OperationControlErrorFrame {
  return !!value && typeof value === "object" &&
    (value as { kind?: string }).kind === "error" &&
    typeof Reflect.get(value, "error") === "object";
}

function controlFrameToError(
  frame: OperationControlErrorFrame,
): OperationControlError {
  const lifecycleError = controlFrameToLifecycleError(frame);
  if (lifecycleError) {
    return lifecycleError;
  }

  return createTransportError({
    code: "trellis.operation.control_error",
    message: "Trellis rejected the operation control request.",
    hint:
      "Check the operation state, then retry the action if it still applies.",
    context: {
      controlErrorType: frame.error.type,
      controlErrorMessage: frame.error.message,
    },
  });
}

function controlFrameToLifecycleError(
  frame: OperationControlErrorFrame,
): OperationLifecycleError | undefined {
  if (!isFullOperationLifecycleErrorData(frame.error)) {
    return undefined;
  }

  const descriptor = getBuiltinRpcError(frame.error.type);
  if (!descriptor?.schema) {
    return undefined;
  }

  const parsed = parseUnknownSchema(
    descriptor.schema as Parameters<typeof parseUnknownSchema>[0],
    frame.error as JsonValue,
  ).take();
  if (isErr(parsed)) {
    return undefined;
  }

  const error = descriptor.fromSerializable(parsed);
  return isOperationLifecycleError(error) ? error : undefined;
}

function isOperationLifecycleError(
  error: unknown,
): error is OperationLifecycleError {
  return error instanceof OperationNotFoundError ||
    error instanceof OperationAlreadyTerminalError ||
    error instanceof OperationMismatchError;
}

function isFullOperationLifecycleErrorData(
  error: { type: string; message: string },
): boolean {
  if (typeof Reflect.get(error, "id") !== "string") {
    return false;
  }

  switch (error.type) {
    case "OperationNotFoundError":
    case "OperationAlreadyTerminalError":
      return typeof Reflect.get(error, "operationId") === "string";
    case "OperationMismatchError":
      return typeof Reflect.get(error, "operationId") === "string" &&
        typeof Reflect.get(error, "expectedService") === "string" &&
        typeof Reflect.get(error, "expectedOperation") === "string";
    default:
      return false;
  }
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
} {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function createAcceptedReplayFilter<TProgress, TOutput>(
  accepted: AcceptedOperationEvent<TProgress, TOutput>,
): (event: OperationEvent<TProgress, TOutput>) => boolean {
  return (event) => {
    return event.type === "accepted" &&
      event.snapshot.id === accepted.snapshot.id &&
      event.snapshot.service === accepted.snapshot.service &&
      event.snapshot.operation === accepted.snapshot.operation &&
      event.snapshot.revision === accepted.snapshot.revision &&
      event.snapshot.state === accepted.snapshot.state;
  };
}

function failedObservation<TProgress, TOutput>(
  error: OperationControlError | UnexpectedError,
): OperationWatchObservation<TProgress, TOutput> {
  return {
    task: Promise.resolve(err(error)),
  };
}

async function dispatchOperationEventResult<TProgress, TOutput, TUpdate>(
  callbacks: OperationObserverCallbacks<TProgress, TOutput, TUpdate>,
  event: OperationEvent<TProgress, TOutput, TUpdate>,
): Promise<Result<void, UnexpectedError>> {
  try {
    await dispatchObservedOperationEvent(callbacks, event);
    return ok(undefined);
  } catch (cause) {
    return err(toObservedCallbackError(cause));
  }
}

function toObservedCallbackError(cause: unknown): UnexpectedError {
  return (cause instanceof UnexpectedError
    ? cause
    : new UnexpectedError({ cause }))
    .withContext({ operationObserverCallback: true });
}

function isObservedCallbackError(
  error: OperationControlError | UnexpectedError,
): boolean {
  return error instanceof UnexpectedError &&
    error.getContext().operationObserverCallback === true;
}
