/** A decoded Trellis HTTP error envelope. */
export const INVALID_HTTP_ERROR_ENVELOPE = "invalid_http_error_envelope";

/** A decoded Trellis HTTP error envelope. */
export class TrellisHttpError extends Error {
  /** HTTP status returned by Trellis. */
  readonly status: number;
  /** Exact machine-readable Trellis error code. */
  readonly code: string;

  constructor(status: number, code: string) {
    super(`Trellis HTTP ${status}: ${code}`);
    this.name = "TrellisHttpError";
    this.status = status;
    this.code = code;
  }
}

/** Decode one authoritative Trellis HTTP error envelope. */
export async function decodeTrellisHttpError(
  response: Response,
): Promise<TrellisHttpError> {
  let code = INVALID_HTTP_ERROR_ENVELOPE;
  try {
    const body: unknown = await response.json();
    if (
      typeof body === "object" && body !== null && "error" in body &&
      typeof body.error === "object" && body.error !== null &&
      "code" in body.error && typeof body.error.code === "string" &&
      body.error.code.length > 0
    ) {
      code = body.error.code;
    }
  } catch {
    // The local code keeps malformed responses distinct from server error codes.
  }
  return new TrellisHttpError(response.status, code);
}

/**
 * Authorization codes that report in-flight materialization rather than denial.
 *
 * The server returns these while approved authority, dependencies, or resources
 * are still being materialized for the participant. They are transient and
 * identical in meaning to every client: retry after a bounded backoff, never
 * treat as terminal. Growth of a participant's own authority must never fail a
 * caller, so every bootstrap, bind, connect, device, and refresh retry loop
 * shares this predicate instead of matching one code each.
 */
export const RETRIABLE_AUTHORIZATION_CODES: readonly string[] = [
  "resource_pending",
  "dependency_pending",
  "authorization_pending",
];

/** Return whether one decoded HTTP error code is a retriable in-flight state. */
export function isRetriableAuthorizationCode(code: string): boolean {
  return RETRIABLE_AUTHORIZATION_CODES.includes(code);
}
