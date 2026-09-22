import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

export const AuthErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("AuthError"),
  message: Type.String(),
  reason: Type.String(),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});
export type AuthErrorData = Static<typeof AuthErrorDataSchema>;

/**
 * Error for authentication and authorization failures.
 */
export class AuthError extends TrellisError<AuthErrorData> {
  override readonly name = "AuthError" as const;
  readonly reason: AuthErrorData["reason"];

  constructor(
    options: ErrorOptions & {
      reason: AuthErrorData["reason"];
      message?: string;
      context?: Record<string, unknown>;
      id?: string;
    },
  ) {
    const { reason, message, ...baseOptions } = options;
    super(message ?? `Auth failed: ${reason}`, baseOptions);
    this.reason = reason;
  }

  /**
   * Serializes error to a plain object.
   *
   * @returns Plain object representation of the error
   */
  override toSerializable(): AuthErrorData {
    return {
      ...this.baseSerializable(),
      type: this.name,
      reason: this.reason,
    } as AuthErrorData;
  }
}
