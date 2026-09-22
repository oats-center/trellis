import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

export const TransportErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("TransportError"),
  message: Type.String(),
  code: Type.String(),
  hint: Type.String(),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});

export type TransportErrorData = Static<typeof TransportErrorDataSchema>;

/**
 * Error for Trellis runtime transport failures.
 */
export class TransportError extends TrellisError<TransportErrorData> {
  override readonly name = "TransportError" as const;
  readonly code: string;
  readonly hint: string;

  constructor(
    options: ErrorOptions & {
      code: string;
      message: string;
      hint: string;
      context?: Record<string, unknown>;
      id?: string;
      traceId?: string;
    },
  ) {
    const { code, message, hint, ...baseOptions } = options;
    super(message, baseOptions);
    this.code = code;
    this.hint = hint;
  }

  override toSerializable(): TransportErrorData {
    return {
      ...this.baseSerializable(),
      type: this.name,
      code: this.code,
      hint: this.hint,
    };
  }
}
