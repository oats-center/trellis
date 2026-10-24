import { type Msg } from "@nats-io/nats-core";
import { BaseError, Result, err, ok } from "@oatscenter/result";
import { encodeSchema, parseUnknownSchema, type JsonValue } from "../codec.ts";
import {
  TransportError,
  UnexpectedError,
  ValidationError,
  type SchemaValidationError,
} from "../errors/index.ts";

import { RemoteError } from "../errors/RemoteError.ts";
import { type Codec } from "../generated.ts";
import { trellisCrypto } from "../auth/crypto.ts";

/**
 * How many live openings one route may verify concurrently before it sheds
 * excess unverified requests.
 */
export const MAX_PENDING_OPENINGS = 64;

/**
 * Whether one reply destination is provably inside the verified caller's inbox.
 *
 * Live openings must never reflect a denial, validation error, or offer onto a
 * destination outside this prefix.
 */
export function callerOwnsReply(
  reply: string | undefined,
  inboxPrefix: string,
): boolean {
  if (!reply || !inboxPrefix) return false;
  return reply === inboxPrefix || reply.startsWith(`${inboxPrefix}.`);
}

/**
 * Safely extract JSON from a NATS message.
 * The .json() method can throw if the message data is not valid JSON.
 */
export function safeJson(msg: Msg): Result<JsonValue, UnexpectedError> {
  return Result.try(() => msg.json() as JsonValue);
}

export function transportCauseContext(cause: unknown): Record<string, unknown> {
  if (cause instanceof Error) {
    return {
      causeName: cause.name,
      causeMessage: cause.message,
    };
  }

  return { cause: String(cause) };
}

export function createTransportError(args: {
  code: string;
  message: string;
  hint: string;
  context?: Record<string, unknown>;
  cause?: unknown;
}): TransportError {
  return new TransportError({
    code: args.code,
    message: args.message,
    hint: args.hint,
    cause: args.cause,
    context: {
      ...(args.context ?? {}),
      ...(args.cause === undefined ? {} : transportCauseContext(args.cause)),
    },
  });
}

export function requestFailedTransportError(args: {
  code: string;
  method?: string;
  subject: string;
  hint: string;
  message: string;
  cause?: unknown;
  context?: Record<string, unknown>;
}): TransportError {
  return createTransportError({
    code: args.code,
    message: args.message,
    hint: args.hint,
    cause: args.cause,
    context: {
      subject: args.subject,
      ...(args.method === undefined ? {} : { method: args.method }),
      ...(args.context ?? {}),
    },
  });
}

/** Bounded catalog outcome for one logical RPC failure. */
export function outcomeForFailure(
  error: BaseError,
):
  | "declared_error"
  | "denied"
  | "invalid"
  | "rate_limited"
  | "timeout"
  | "unavailable"
  | "cancelled"
  | "error" {
  if (error instanceof RemoteError) return "declared_error";
  if (error instanceof TransportError) {
    switch (Reflect.get(error, "code")) {
      case "trellis.request.denied":
        return "denied";
      case "trellis.request.unavailable":
        return "unavailable";
      case "trellis.request.timeout":
        return "timeout";
      case "trellis.request.cancelled":
        return "cancelled";
      case "trellis.request.invalid_response":
      case "trellis.request.invalid":
        return "invalid";
      case "trellis.request.rate_limited":
        return "rate_limited";
      default:
        return "error";
    }
  }
  return "error";
}

export function classifyRequestTransportFailure(args: {
  method?: string;
  subject: string;
  callerCapabilities?: readonly string[];
  cause: unknown;
}): TransportError {
  const message =
    args.cause instanceof Error ? args.cause.message : String(args.cause);
  const isNoResponders = message.includes("no responders");
  const isNatsPermission =
    args.cause instanceof Error &&
    [
      "PermissionViolationError",
      "AuthorizationError",
      "UserAuthenticationExpiredError",
    ].includes(args.cause.name);

  return requestFailedTransportError({
    code: isNoResponders
      ? "trellis.request.unavailable"
      : isNatsPermission
        ? "trellis.request.denied"
        : "trellis.request.failed",
    message: isNoResponders
      ? "Trellis could not reach the requested capability."
      : isNatsPermission
        ? "Trellis denied this request."
        : "Trellis could not complete the request.",
    hint: isNoResponders
      ? "Check that the target service is installed and reachable, then try again."
      : isNatsPermission
        ? "Sign in with a profile that has the required capability, then try again."
        : "Retry the request. If it keeps failing, check Trellis runtime health.",
    cause: args.cause,
    method: args.method,
    subject: args.subject,
    context: {
      ...(args.callerCapabilities === undefined
        ? {}
        : { requiredCapabilities: args.callerCapabilities }),
      noResponders: isNoResponders,
      lowLevelMessage: message,
    },
  });
}

/**
 * Map a generation acquisition failure to the ordinary request transport error.
 *
 * Automatic adoption never invents `transport_upgrade_required`; a caller that
 * cannot obtain a suitable generation within its budget observes a normal
 * unavailable/timeout/cancelled request failure.
 */
export function mapTransportAcquireFailure(args: {
  method?: string;
  subject: string;
  callerCapabilities?: readonly string[];
  cause: unknown;
}): TransportError {
  const cause = args.cause;
  if (cause instanceof TransportError) {
    const code =
      cause.code === "trellis.transport.timeout"
        ? "trellis.request.timeout"
        : cause.code === "trellis.transport.aborted"
          ? "trellis.request.cancelled"
          : cause.code === "trellis.transport.closed"
            ? "trellis.request.closed"
            : "trellis.request.unavailable";
    return requestFailedTransportError({
      code,
      message: cause.message,
      hint: cause.hint,
      method: args.method,
      subject: args.subject,
      cause,
      context: {
        ...(args.callerCapabilities === undefined
          ? {}
          : { requiredCapabilities: args.callerCapabilities }),
        generationCode: cause.code,
      },
    });
  }
  return classifyRequestTransportFailure(args);
}

/** Creates the existing typed transport error for an unavailable optional action. */
export function createActionUnavailableError(
  action: string,
  capabilities: readonly string[],
): TransportError {
  return requestFailedTransportError({
    code: "trellis.request.unavailable",
    message: "Trellis could not reach the requested capability.",
    hint: "Wait for the optional capability to become available, then try again.",
    subject: action,
    context: { action, capabilities },
  });
}

export function encodeRuntimeSchema(
  schema: unknown,
  data: unknown,
): Result<string, SchemaValidationError | ValidationError | UnexpectedError> {
  if (
    schema &&
    typeof schema === "object" &&
    typeof Reflect.get(schema, "encode") === "function"
  ) {
    try {
      const encoded = (schema as Codec<unknown>).encode(data);
      const json = JSON.stringify(encoded);
      if (json === undefined) {
        throw new TypeError("Codec encoded no JSON value");
      }
      return ok(json);
    } catch (cause) {
      return err(new UnexpectedError({ cause }));
    }
  }
  return encodeSchema(schema as never, data);
}

export function parseRuntimeSchema(
  schema: unknown,
  data: JsonValue,
): Result<unknown, SchemaValidationError | ValidationError | UnexpectedError> {
  if (
    schema &&
    typeof schema === "object" &&
    typeof Reflect.get(schema, "decode") === "function"
  ) {
    try {
      return ok((schema as Codec<unknown>).decode(data));
    } catch (cause) {
      return err(new UnexpectedError({ cause }));
    }
  }
  return parseUnknownSchema(
    schema as Parameters<typeof parseUnknownSchema>[0],
    data,
  );
}

export function base64urlEncode(data: Uint8Array): string {
  const b64 = btoa(String.fromCharCode(...data));
  return b64.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

export function base64urlDecode(s: string): Uint8Array {
  const normalized = s.replace(/-/g, "+").replace(/_/g, "/");
  const padLen = (4 - (normalized.length % 4)) % 4;
  const padded = normalized + "=".repeat(padLen);
  const bin = atob(padded);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  const buf = data.buffer;
  if (buf instanceof ArrayBuffer) {
    return buf.slice(data.byteOffset, data.byteOffset + data.byteLength);
  }
  const copy = new Uint8Array(data.byteLength);
  copy.set(data);
  return copy.buffer;
}

export async function sha256(data: Uint8Array): Promise<Uint8Array> {
  return await (await trellisCrypto()).sha256(data);
}
