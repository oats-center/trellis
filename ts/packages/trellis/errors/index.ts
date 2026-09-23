import {
  UnexpectedError,
  type UnexpectedErrorData,
  UnexpectedErrorDataSchema,
} from "@oatscenter/result";
import { AuthError } from "./AuthError.ts";
import type { AuthErrorData } from "./AuthError.ts";
import { AuthErrorDataSchema } from "./AuthError.ts";
import { ValidationError } from "./ValidationError.ts";
import type { ValidationErrorData } from "./ValidationError.ts";
import { ValidationErrorDataSchema } from "./ValidationError.ts";
import { SchemaValidationError } from "./SchemaValidationError.ts";
import type {
  SchemaValidationErrorData,
  SchemaValidationIssue,
} from "./SchemaValidationError.ts";
import {
  SchemaValidationErrorDataSchema,
  SchemaValidationIssueSchema,
} from "./SchemaValidationError.ts";
import { RemoteError } from "./RemoteError.ts";
import { KVError } from "./KVError.ts";
import type { KVErrorData } from "./KVError.ts";
import { KVErrorDataSchema } from "./KVError.ts";
import {
  OperationAlreadyTerminalError,
  OperationAlreadyTerminalErrorDataSchema,
  OperationMismatchError,
  OperationMismatchErrorDataSchema,
  OperationNotFoundError,
  OperationNotFoundErrorDataSchema,
} from "./OperationLifecycleError.ts";
import type {
  OperationAlreadyTerminalErrorData,
  OperationMismatchErrorData,
  OperationNotFoundErrorData,
} from "./OperationLifecycleError.ts";
import { StoreError } from "./StoreError.ts";
import type { StoreErrorData } from "./StoreError.ts";
import { StoreErrorDataSchema } from "./StoreError.ts";
import { TransportError } from "./TransportError.ts";
import type { TransportErrorData } from "./TransportError.ts";
import { TransportErrorDataSchema } from "./TransportError.ts";
import { TransferError } from "./TransferError.ts";
import type { TransferErrorData } from "./TransferError.ts";
import { TransferErrorDataSchema } from "./TransferError.ts";

type RuntimeRpcErrorDesc = {
  type: string;
  schema?: unknown;
  fromSerializable(data: unknown): Error;
};

export { UnexpectedError } from "@oatscenter/result";
export { TrellisError } from "./TrellisError.ts";
export { AuthError } from "./AuthError.ts";
export { ValidationError } from "./ValidationError.ts";
export { SchemaValidationError } from "./SchemaValidationError.ts";
export { RemoteError } from "./RemoteError.ts";
export { KVError } from "./KVError.ts";
export {
  OperationAlreadyTerminalError,
  OperationMismatchError,
  OperationNotFoundError,
} from "./OperationLifecycleError.ts";
export { StoreError } from "./StoreError.ts";
export { TransportError } from "./TransportError.ts";
export { TransferError } from "./TransferError.ts";

export { type AuthErrorData, AuthErrorDataSchema } from "./AuthError.ts";
export {
  type ValidationErrorData,
  ValidationErrorDataSchema,
  type ValidationIssue,
  ValidationIssueSchema,
} from "./ValidationError.ts";
export {
  type SchemaValidationErrorData,
  SchemaValidationErrorDataSchema,
  type SchemaValidationIssue,
  SchemaValidationIssueSchema,
} from "./SchemaValidationError.ts";
export { type RemoteErrorData, RemoteErrorDataSchema } from "./RemoteError.ts";
export { type KVErrorData, KVErrorDataSchema } from "./KVError.ts";
export {
  type OperationAlreadyTerminalErrorData,
  OperationAlreadyTerminalErrorDataSchema,
  type OperationMismatchErrorData,
  OperationMismatchErrorDataSchema,
  type OperationNotFoundErrorData,
  OperationNotFoundErrorDataSchema,
} from "./OperationLifecycleError.ts";
export { type StoreErrorData, StoreErrorDataSchema } from "./StoreError.ts";
export {
  type TransportErrorData,
  TransportErrorDataSchema,
} from "./TransportError.ts";
export {
  type TransferErrorData,
  TransferErrorDataSchema,
} from "./TransferError.ts";

/**
 * Single source of truth for all Trellis errors.
 * This object is used for compile-time type inference.
 */
const TRANSPORTABLE_TRELLIS_ERRORS = {
  UnexpectedError,
  TransportError,
  AuthError,
  ValidationError,
  SchemaValidationError,
  KVError,
  OperationNotFoundError,
  OperationAlreadyTerminalError,
  OperationMismatchError,
  StoreError,
  TransferError,
} as const;

const TRELLIS_ERRORS = {
  ...TRANSPORTABLE_TRELLIS_ERRORS,
  RemoteError,
} as const;

/**
 * Compile-time mapping from error names to their instance types.
 * Derived from TRELLIS_ERRORS to ensure types stay in sync with runtime.
 */
export type TrellisErrorMap = {
  [K in keyof typeof TRELLIS_ERRORS]: InstanceType<(typeof TRELLIS_ERRORS)[K]>;
};

export type TransportableTrellisErrorMap = {
  [K in keyof typeof TRANSPORTABLE_TRELLIS_ERRORS]: InstanceType<
    (typeof TRANSPORTABLE_TRELLIS_ERRORS)[K]
  >;
};

export type TrellisErrorName = keyof TransportableTrellisErrorMap;
export type TrellisErrorInstance = TrellisErrorMap[TrellisErrorName];
export type MapErrorNamesToTypes<T extends readonly TrellisErrorName[]> = {
  [K in keyof T]: T[K] extends TrellisErrorName
    ? TransportableTrellisErrorMap[T[K]]
    : never;
}[number];

export const BUILTIN_RPC_ERRORS = {
  UnexpectedError: {
    type: "UnexpectedError",
    schema: UnexpectedErrorDataSchema,
    fromSerializable(data: UnexpectedErrorData) {
      return new UnexpectedError({
        id: data.id,
        context: data.context,
        traceId: data.traceId,
      });
    },
  },
  AuthError: {
    type: "AuthError",
    schema: AuthErrorDataSchema,
    fromSerializable(data: AuthErrorData) {
      return new AuthError({
        reason: data.reason,
        message: data.message,
        id: data.id,
        context: data.context,
      });
    },
  },
  TransportError: {
    type: "TransportError",
    schema: TransportErrorDataSchema,
    fromSerializable(data: TransportErrorData) {
      return new TransportError({
        code: data.code,
        message: data.message,
        hint: data.hint,
        id: data.id,
        context: data.context,
        traceId: data.traceId,
      });
    },
  },
  ValidationError: {
    type: "ValidationError",
    schema: ValidationErrorDataSchema,
    fromSerializable(data: ValidationErrorData) {
      return new ValidationError({
        errors: data.issues.map((issue) => ({
          path: issue.path,
          message: issue.message,
        })),
        id: data.id,
        context: data.context,
      });
    },
  },
  SchemaValidationError: {
    type: "SchemaValidationError",
    schema: SchemaValidationErrorDataSchema,
    fromSerializable(data: SchemaValidationErrorData) {
      return new SchemaValidationError({
        issues: data.issues,
        id: data.id,
        context: data.context,
      });
    },
  },
  KVError: {
    type: "KVError",
    schema: KVErrorDataSchema,
    fromSerializable(data: KVErrorData) {
      return new KVError({
        operation: data.operation,
        id: data.id,
        context: data.context,
      });
    },
  },
  OperationNotFoundError: {
    type: "OperationNotFoundError",
    schema: OperationNotFoundErrorDataSchema,
    fromSerializable(data: OperationNotFoundErrorData) {
      return new OperationNotFoundError({
        operationId: data.operationId,
        message: data.message,
        id: data.id,
        context: data.context,
        traceId: data.traceId,
      });
    },
  },
  OperationAlreadyTerminalError: {
    type: "OperationAlreadyTerminalError",
    schema: OperationAlreadyTerminalErrorDataSchema,
    fromSerializable(data: OperationAlreadyTerminalErrorData) {
      return new OperationAlreadyTerminalError({
        operationId: data.operationId,
        state: data.state,
        operation: data.operation,
        service: data.service,
        message: data.message,
        id: data.id,
        context: data.context,
        traceId: data.traceId,
      });
    },
  },
  OperationMismatchError: {
    type: "OperationMismatchError",
    schema: OperationMismatchErrorDataSchema,
    fromSerializable(data: OperationMismatchErrorData) {
      return new OperationMismatchError({
        operationId: data.operationId,
        expectedService: data.expectedService,
        expectedOperation: data.expectedOperation,
        actualService: data.actualService,
        actualOperation: data.actualOperation,
        message: data.message,
        id: data.id,
        context: data.context,
        traceId: data.traceId,
      });
    },
  },
  StoreError: {
    type: "StoreError",
    schema: StoreErrorDataSchema,
    fromSerializable(data: StoreErrorData) {
      return new StoreError({
        operation: data.operation,
        id: data.id,
        context: data.context,
      });
    },
  },
  TransferError: {
    type: "TransferError",
    schema: TransferErrorDataSchema,
    fromSerializable(data: TransferErrorData) {
      return new TransferError({
        operation: data.operation,
        id: data.id,
        context: data.context,
      });
    },
  },
} as const satisfies Record<string, RuntimeRpcErrorDesc>;

export function getBuiltinRpcError(
  type: string,
): RuntimeRpcErrorDesc | undefined {
  return BUILTIN_RPC_ERRORS[type as keyof typeof BUILTIN_RPC_ERRORS];
}

/**
 * Reads the machine-readable failure code from any Trellis error shape.
 *
 * Declared RPC errors reconstruct into generated error classes that carry the
 * code under `data`, built-in errors carry `reason`, and undecoded remote
 * failures carry it under `remoteError`. Callers that must branch on a specific
 * server code use this instead of matching one historical shape.
 */
export function machineErrorCode(error: unknown): string | undefined {
  if (!error || typeof error !== "object") return undefined;
  const record = error as Record<string, unknown>;
  if (typeof record.code === "string") return record.code;
  if (typeof record.reason === "string") return record.reason;
  const data = record.data;
  if (data && typeof data === "object") {
    const dataRecord = data as Record<string, unknown>;
    if (typeof dataRecord.code === "string") return dataRecord.code;
    if (typeof dataRecord.reason === "string") return dataRecord.reason;
  }
  const remote = record.remoteError;
  if (remote && typeof remote === "object") {
    const remoteRecord = remote as Record<string, unknown>;
    if (typeof remoteRecord.code === "string") return remoteRecord.code;
    if (typeof remoteRecord.reason === "string") return remoteRecord.reason;
  }
  return undefined;
}
