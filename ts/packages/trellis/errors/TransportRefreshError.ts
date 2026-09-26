import { TrellisError } from "./TrellisError.ts";

/** Stable failure codes for an explicit transport refresh. */
export type TransportRefreshErrorCode =
  | "connection_closed"
  | "authorization_unavailable"
  | "timeout"
  | "transport_error";

export type TransportRefreshErrorData = {
  id: string;
  type: "TransportRefreshError";
  message: string;
  code: TransportRefreshErrorCode;
  context?: Record<string, unknown>;
};

/**
 * Failure of an explicit `refreshTransport()` request.
 *
 * Codes are stable and semantic: the caller learns whether the connection was
 * already closed, fresh authority could not be prepared, the operation exceeded
 * its single deadline, or the physical reconnect failed. A refresh never leaves
 * the caller guessing whether a healthy socket was replaced.
 */
export class TransportRefreshError
  extends TrellisError<TransportRefreshErrorData> {
  override readonly name = "TransportRefreshError" as const;
  readonly code: TransportRefreshErrorCode;

  constructor(
    options: ErrorOptions & {
      code: TransportRefreshErrorCode;
      message: string;
      context?: Record<string, unknown>;
      id?: string;
    },
  ) {
    const { code, message, ...base } = options;
    super(message, base);
    this.code = code;
  }

  /** The logical connection was already closed or closing. */
  static connectionClosed(): TransportRefreshError {
    return new TransportRefreshError({
      code: "connection_closed",
      message: "The Trellis connection is already closed.",
    });
  }

  /** Fresh next-connect or application authorization could not be prepared. */
  static authorizationUnavailable(cause: unknown): TransportRefreshError {
    return new TransportRefreshError({
      code: "authorization_unavailable",
      message: "Fresh Trellis authorization could not be prepared.",
      cause,
    });
  }

  /** The single refresh deadline elapsed before the new attachment was ready. */
  static timedOut(): TransportRefreshError {
    return new TransportRefreshError({
      code: "timeout",
      message: "Trellis transport refresh timed out.",
    });
  }

  /** The physical reconnect reported an ordinary transport failure. */
  static fromTransport(cause: unknown): TransportRefreshError {
    return new TransportRefreshError({
      code: "transport_error",
      message: "Trellis could not replace the physical NATS attachment.",
      cause,
    });
  }

  override toSerializable(): TransportRefreshErrorData {
    return {
      ...this.baseSerializable(),
      type: this.name,
      code: this.code,
    };
  }
}
