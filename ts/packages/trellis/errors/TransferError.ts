import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

export const TransferErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("TransferError"),
  message: Type.String(),
  operation: Type.Optional(Type.String()),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});

export type TransferErrorData = Static<typeof TransferErrorDataSchema>;

export class TransferError extends TrellisError<TransferErrorData> {
  override readonly name = "TransferError" as const;
  readonly operation?: string;

  constructor(
    options: ErrorOptions & {
      operation?: string;
      context?: Record<string, unknown>;
      id?: string;
    },
  ) {
    const { operation, ...baseOptions } = options;
    const message = `Transfer ${operation || "operation"} failed`;
    super(message, baseOptions);
    this.operation = operation;
  }

  override toSerializable(): TransferErrorData {
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
