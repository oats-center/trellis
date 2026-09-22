import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

export const KVErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("KVError"),
  message: Type.String(),
  operation: Type.Optional(Type.String()),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});
export type KVErrorData = Static<typeof KVErrorDataSchema>;

/**
 * Error for KV storage operations.
 * Used when key-value store operations fail.
 */
export class KVError extends TrellisError<KVErrorData> {
  override readonly name = "KVError" as const;
  readonly operation?: string;

  constructor(
    options: ErrorOptions & {
      operation?: string;
      context?: Record<string, unknown>;
      id?: string;
    },
  ) {
    const { operation, ...baseOptions } = options;
    const msg = `KV ${operation || ""} failed`;
    super(msg, baseOptions);
    this.operation = operation;
  }

  /**
   * Serializes error to a plain object.
   *
   * @returns Plain object representation of the error
   */
  override toSerializable(): KVErrorData {
    return {
      ...this.baseSerializable(),
      type: this.name,
      operation: this.operation,
    } as KVErrorData;
  }
}
