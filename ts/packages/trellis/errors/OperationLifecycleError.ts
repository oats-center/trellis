import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

export const OperationNotFoundErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("OperationNotFoundError"),
  message: Type.String(),
  operationId: Type.String(),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});

export type OperationNotFoundErrorData = Static<
  typeof OperationNotFoundErrorDataSchema
>;

/**
 * Error raised when a requested Trellis operation id cannot be found.
 */
export class OperationNotFoundError
  extends TrellisError<OperationNotFoundErrorData> {
  override readonly name = "OperationNotFoundError" as const;
  readonly operationId: string;

  constructor(
    options: ErrorOptions & {
      operationId: string;
      message?: string;
      context?: Record<string, unknown>;
      id?: string;
      traceId?: string;
    },
  ) {
    const { operationId, message, ...baseOptions } = options;
    super(message ?? `Operation not found: ${operationId}`, baseOptions);
    this.operationId = operationId;
  }

  /**
   * Serializes error to a plain object.
   *
   * @returns Plain object representation of the error.
   */
  override toSerializable(): OperationNotFoundErrorData {
    const base = this.baseSerializable();
    return {
      id: base.id,
      type: this.name,
      message: base.message,
      operationId: this.operationId,
      ...(base.context !== undefined ? { context: base.context } : {}),
      ...(base.traceId !== undefined ? { traceId: base.traceId } : {}),
    };
  }
}

export const OperationAlreadyTerminalErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("OperationAlreadyTerminalError"),
  message: Type.String(),
  operationId: Type.String(),
  state: Type.Optional(Type.String()),
  operation: Type.Optional(Type.String()),
  service: Type.Optional(Type.String()),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});

export type OperationAlreadyTerminalErrorData = Static<
  typeof OperationAlreadyTerminalErrorDataSchema
>;

/**
 * Error raised when a Trellis operation lifecycle mutation targets a terminal operation.
 */
export class OperationAlreadyTerminalError
  extends TrellisError<OperationAlreadyTerminalErrorData> {
  override readonly name = "OperationAlreadyTerminalError" as const;
  readonly operationId: string;
  readonly state?: string;
  readonly operation?: string;
  readonly service?: string;

  constructor(
    options: ErrorOptions & {
      operationId: string;
      state?: string;
      operation?: string;
      service?: string;
      message?: string;
      context?: Record<string, unknown>;
      id?: string;
      traceId?: string;
    },
  ) {
    const { operationId, state, operation, service, message, ...baseOptions } =
      options;
    super(message ?? `Operation already terminal: ${operationId}`, baseOptions);
    this.operationId = operationId;
    this.state = state;
    this.operation = operation;
    this.service = service;
  }

  /**
   * Serializes error to a plain object.
   *
   * @returns Plain object representation of the error.
   */
  override toSerializable(): OperationAlreadyTerminalErrorData {
    const base = this.baseSerializable();
    return {
      id: base.id,
      type: this.name,
      message: base.message,
      operationId: this.operationId,
      ...(this.state !== undefined ? { state: this.state } : {}),
      ...(this.operation !== undefined ? { operation: this.operation } : {}),
      ...(this.service !== undefined ? { service: this.service } : {}),
      ...(base.context !== undefined ? { context: base.context } : {}),
      ...(base.traceId !== undefined ? { traceId: base.traceId } : {}),
    };
  }
}

export const OperationMismatchErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("OperationMismatchError"),
  message: Type.String(),
  operationId: Type.String(),
  expectedService: Type.String(),
  expectedOperation: Type.String(),
  actualService: Type.Optional(Type.String()),
  actualOperation: Type.Optional(Type.String()),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});

export type OperationMismatchErrorData = Static<
  typeof OperationMismatchErrorDataSchema
>;

/**
 * Error raised when a Trellis operation id belongs to a different service or operation.
 */
export class OperationMismatchError
  extends TrellisError<OperationMismatchErrorData> {
  override readonly name = "OperationMismatchError" as const;
  readonly operationId: string;
  readonly expectedService: string;
  readonly expectedOperation: string;
  readonly actualService?: string;
  readonly actualOperation?: string;

  constructor(
    options: ErrorOptions & {
      operationId: string;
      expectedService: string;
      expectedOperation: string;
      actualService?: string;
      actualOperation?: string;
      message?: string;
      context?: Record<string, unknown>;
      id?: string;
      traceId?: string;
    },
  ) {
    const {
      operationId,
      expectedService,
      expectedOperation,
      actualService,
      actualOperation,
      message,
      ...baseOptions
    } = options;
    super(message ?? `Operation mismatch: ${operationId}`, baseOptions);
    this.operationId = operationId;
    this.expectedService = expectedService;
    this.expectedOperation = expectedOperation;
    this.actualService = actualService;
    this.actualOperation = actualOperation;
  }

  /**
   * Serializes error to a plain object.
   *
   * @returns Plain object representation of the error.
   */
  override toSerializable(): OperationMismatchErrorData {
    const base = this.baseSerializable();
    return {
      id: base.id,
      type: this.name,
      message: base.message,
      operationId: this.operationId,
      expectedService: this.expectedService,
      expectedOperation: this.expectedOperation,
      ...(this.actualService !== undefined
        ? { actualService: this.actualService }
        : {}),
      ...(this.actualOperation !== undefined
        ? { actualOperation: this.actualOperation }
        : {}),
      ...(base.context !== undefined ? { context: base.context } : {}),
      ...(base.traceId !== undefined ? { traceId: base.traceId } : {}),
    };
  }
}
