import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

export const StoreErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("StoreError"),
  message: Type.String(),
  operation: Type.Optional(Type.String()),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});

export type StoreErrorData = Static<typeof StoreErrorDataSchema>;

export class StoreError extends TrellisError<StoreErrorData> {
  override readonly name = "StoreError" as const;
  readonly operation?: string;

  constructor(
    options: ErrorOptions & {
      operation?: string;
      context?: Record<string, unknown>;
      id?: string;
    },
  ) {
    const { operation, ...baseOptions } = options;
    const message = `Store ${operation || ""} failed`;
    super(message, baseOptions);
    this.operation = operation;
  }

  override toSerializable(): StoreErrorData {
    const base = this.baseSerializable();
    return {
      id: base.id,
      type: this.name,
      message: base.message,
      ...(this.operation !== undefined ? { operation: this.operation } : {}),
      ...(base.context !== undefined ? { context: base.context } : {}),
      ...(base.traceId !== undefined ? { traceId: base.traceId } : {}),
    };
  }
}
