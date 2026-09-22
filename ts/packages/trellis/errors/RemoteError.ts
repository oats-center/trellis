import {
  Result,
  type UnexpectedError,
  type UnexpectedErrorData,
} from "@qlever-llc/result";
import Type, { type Static } from "typebox";
import { ParseError, Value } from "typebox/value";
import type { AuthErrorData } from "./AuthError.ts";
import type { KVErrorData } from "./KVError.ts";
import type {
  OperationAlreadyTerminalErrorData,
  OperationMismatchErrorData,
  OperationNotFoundErrorData,
} from "./OperationLifecycleError.ts";
import type { StoreErrorData } from "./StoreError.ts";
import type { TransportErrorData } from "./TransportError.ts";
import type { TransferErrorData } from "./TransferError.ts";
import { TrellisError } from "./TrellisError.ts";
import { ValidationError } from "./ValidationError.ts";
import type { ValidationErrorData } from "./ValidationError.ts";

export const RemoteErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("RemoteError"),
  message: Type.String(),
  remoteError: Type.Any(),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});
export type RemoteErrorData = Static<typeof RemoteErrorDataSchema>;

export const TrellisErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.String(),
  message: Type.String(),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
}, { additionalProperties: true });
export type TrellisErrorData = Static<typeof TrellisErrorDataSchema>;

export type TransportableTrellisErrorData =
  | UnexpectedErrorData
  | AuthErrorData
  | ValidationErrorData
  | KVErrorData
  | OperationNotFoundErrorData
  | OperationAlreadyTerminalErrorData
  | OperationMismatchErrorData
  | StoreErrorData
  | TransportErrorData
  | TransferErrorData
  | TrellisErrorData;

/**
 * Error for wrapping errors received from remote Trellis services.
 * This is the only error type with parseJSON/parseObject methods for deserializing remote errors.
 */
export class RemoteError extends TrellisError<RemoteErrorData> {
  override readonly name = "RemoteError" as const;
  readonly remoteError: TransportableTrellisErrorData;

  constructor(
    options: ErrorOptions & {
      error: TransportableTrellisErrorData;
      context?: Record<string, unknown>;
      id?: string;
    },
  ) {
    const { error, ...baseOptions } = options;
    super(`Remote error: ${error.message}`, baseOptions);
    this.remoteError = error;
  }

  /**
   * Serializes error to a plain object.
   *
   * @returns Plain object representation of the error
   */
  override toSerializable(): RemoteErrorData {
    return {
      ...this.baseSerializable(),
      type: this.name,
      remoteError: this.remoteError,
    } as RemoteErrorData;
  }

  /**
   * Parses and validates a plain object as TrellisErrorData.
   * Use this to deserialize errors received from remote services.
   *
   * @param obj - Plain object to validate
   * @returns Result containing validated TrellisError or a ValidationError or UnexpectedError
   */
  static parse(
    data: unknown,
  ): Result<TransportableTrellisErrorData, ValidationError | UnexpectedError> {
    return Result.try(() => typeof data === "string" ? JSON.parse(data) : data)
      .andThen(
        (
          obj: unknown,
        ): Result<
          TransportableTrellisErrorData,
          ValidationError | UnexpectedError
        > => {
          const parseResult = Result.try(() =>
            Value.Parse(TrellisErrorDataSchema, obj)
          );
          if (parseResult.isErr()) {
            const cause = parseResult.error.cause;
            if (cause instanceof ParseError) {
              const errors = Value.Errors(TrellisErrorDataSchema, obj);
              return Result.err(new ValidationError({ errors, cause }));
            }
            return Result.err(parseResult.error);
          }
          return Result.ok(parseResult.take() as TransportableTrellisErrorData);
        },
      );
  }

  /**
   * Alias for parse() - parses JSON string or object as TrellisErrorData.
   * @see parse
   */
  static parseJSON = RemoteError.parse;
}
