/**
 * Lossless human display serialization for generated console values.
 *
 * Generated records carry `bigint`, `Uint8Array`, and known JSON-byte fields
 * that `JSON.stringify` renders incorrectly or throws on. These helpers exist
 * for display, search, and copy output only. They never construct protocol
 * requests; generated codecs remain responsible for the wire.
 */

/** Marker rendered where an object graph revisits a value on the current path. */
export const CIRCULAR_MARKER = "[Circular]";

/** Marker for a value that cannot be represented in display JSON. */
export const UNSERIALIZABLE_MARKER = "[Unserializable]";

function base64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(
    /=+$/u,
    "",
  );
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function displayValue(
  value: unknown,
  path: Set<object>,
): unknown {
  if (value === undefined) return null;
  if (typeof value === "bigint") return value.toString();
  if (value instanceof Uint8Array) {
    return { "$bytes": base64Url(value), length: value.length };
  }
  if (typeof value === "number" && !Number.isFinite(value)) {
    return value === Infinity
      ? "[Infinity]"
      : value === -Infinity
      ? "[-Infinity]"
      : "[NaN]";
  }
  if (!isPlainRecord(value)) return value;
  if (path.has(value)) return CIRCULAR_MARKER;
  path.add(value);
  try {
    if (Array.isArray(value)) {
      return value.map((item) => displayValue(item, path));
    }
    const result: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) {
      result[key] = displayValue(item, path);
    }
    return result;
  } finally {
    path.delete(value);
  }
}

/** Encodes a value for display. Bigint stays exact; bytes get a tagged form. */
export function displayValueJson(value: unknown): unknown {
  return displayValue(value, new Set<object>());
}

/**
 * Human-readable JSON with exact bigint decimals and tagged byte values.
 * Shared noncyclic references are not marked circular; true cycles use a stable
 * marker instead of throwing.
 */
export function displayJson(value: unknown, indent = 2): string {
  try {
    return JSON.stringify(displayValue(value, new Set<object>()), null, indent);
  } catch {
    return UNSERIALIZABLE_MARKER;
  }
}

/** Outcome of decoding a field whose semantic payload is known to be JSON. */
export type KnownJsonBytes =
  | { ok: true; value: unknown }
  | { ok: false; error: string };

/**
 * Decodes a field whose payload is known to be UTF-8 JSON (provider identity or
 * deployment review mode). Malformed input returns a displayable error rather
 * than throwing, and unknown enum values inside valid JSON stay intact.
 */
export function decodeKnownJsonBytes(bytes: unknown): KnownJsonBytes {
  if (!(bytes instanceof Uint8Array)) {
    return { ok: false, error: "expected byte payload" };
  }
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return { ok: false, error: "payload is not valid UTF-8" };
  }
  try {
    return { ok: true, value: JSON.parse(text) as unknown };
  } catch {
    return { ok: false, error: "payload is not valid JSON" };
  }
}

/** Decoded timestamp classification. `absent` is distinct from `invalid`. */
export type TimestampDisplay =
  | { kind: "absent" }
  | { kind: "valid"; date: Date; milliseconds: bigint }
  | { kind: "invalid"; reason: string };

const DECIMAL_INTEGER = /^(?:0|-?[1-9][0-9]*)$/;
const MAX_DATE_MS = 8_640_000_000_000_000n;

/**
 * Normalizes a console timestamp. Null/undefined/empty is absent and zero is
 * valid. Decimal integer strings are Unix milliseconds and are checked before
 * ISO date strings. Bigint converts to Number only when exact and in range.
 */
export function normalizeTimestamp(
  value: bigint | number | string | null | undefined,
): TimestampDisplay {
  if (value === null || value === undefined) return { kind: "absent" };
  if (typeof value === "string" && value.trim() === "") {
    return { kind: "absent" };
  }

  let milliseconds: bigint;
  if (typeof value === "bigint") {
    milliseconds = value;
  } else if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      return { kind: "invalid", reason: "timestamp is not finite" };
    }
    if (!Number.isInteger(value)) {
      return { kind: "invalid", reason: "timestamp is not an integer" };
    }
    milliseconds = BigInt(value);
  } else if (DECIMAL_INTEGER.test(value)) {
    milliseconds = BigInt(value);
  } else {
    const parsed = Date.parse(value);
    if (Number.isNaN(parsed)) {
      return {
        kind: "invalid",
        reason: "timestamp is not a decimal or ISO date",
      };
    }
    milliseconds = BigInt(parsed);
  }

  if (milliseconds > MAX_DATE_MS || milliseconds < -MAX_DATE_MS) {
    return {
      kind: "invalid",
      reason: "timestamp is outside the valid date range",
    };
  }
  const exact = Number(milliseconds);
  if (!Number.isSafeInteger(exact) && BigInt(exact) !== milliseconds) {
    return { kind: "invalid", reason: "timestamp loses precision as a number" };
  }
  const date = new Date(exact);
  if (Number.isNaN(date.getTime())) {
    return {
      kind: "invalid",
      reason: "timestamp is outside the valid date range",
    };
  }
  return { kind: "valid", date, milliseconds };
}

/** Renders a normalized timestamp for display, with explicit absent/invalid copy. */
export function formatTimestamp(
  value: bigint | number | string | null | undefined,
  formatter?: (date: Date) => string,
  absentLabel = "-",
): string {
  const normalized = normalizeTimestamp(value);
  switch (normalized.kind) {
    case "absent":
      return absentLabel;
    case "invalid":
      return `Invalid timestamp (${normalized.reason})`;
    case "valid":
      return formatter?.(normalized.date) ??
        new Intl.DateTimeFormat(undefined, {
          dateStyle: "medium",
          timeStyle: "short",
        }).format(normalized.date);
  }
}

/** Safe projection of any console error for display and branching. */
export type ConsoleErrorProjection = {
  message: string;
  code?: string;
  field?: string | null;
  retryable?: boolean;
  id?: string;
  consentRequest?: unknown;
};

function projectionFromSource(
  source: Record<string, unknown>,
): ConsoleErrorProjection | undefined {
  const message =
    typeof source.message === "string" && source.message.length > 0
      ? source.message
      : undefined;
  const data = isPlainRecord(source.data) ? source.data : undefined;
  const context = isPlainRecord(source.context) ? source.context : undefined;
  const code = typeof source.code === "string"
    ? source.code
    : typeof data?.code === "string"
    ? data.code
    : typeof source.reason === "string"
    ? source.reason
    : typeof context?.reason === "string"
    ? context.reason
    : undefined;
  if (message === undefined && code === undefined) return undefined;
  const consentRequest = "consentRequest" in source
    ? source.consentRequest
    : data !== undefined && "consentRequest" in data
    ? data.consentRequest
    : undefined;
  return {
    message: message ?? `Request failed (${code})`,
    ...(code === undefined ? {} : { code }),
    ...(data !== undefined && "field" in data
      ? { field: data.field as string | null }
      : {}),
    ...(typeof data?.retryable === "boolean"
      ? { retryable: data.retryable }
      : {}),
    ...(typeof source.id === "string"
      ? { id: source.id }
      : typeof data?.id === "string"
      ? { id: data.id }
      : {}),
    ...(consentRequest === undefined ? {} : { consentRequest }),
  };
}

/**
 * Projects a generated or ordinary thrown error into safe display fields.
 * Branches on structured codes only, never on English message text. Accepts a
 * thrown error, a serialized error, or a `Result`-shaped `{ error }` value.
 */
export function projectConsoleError(error: unknown): ConsoleErrorProjection {
  if (typeof error === "string" && error.length > 0) {
    return { message: error };
  }
  if (isPlainRecord(error)) {
    // `Result` err values carry the real error under `error`.
    const inner = error.error;
    if (isPlainRecord(inner) || inner instanceof Error) {
      const projectedInner = projectConsoleError(inner);
      if (projectedInner.message !== "Unexpected error") return projectedInner;
    }
    const remote = isPlainRecord(error.remoteError)
      ? error.remoteError
      : undefined;
    if (remote !== undefined) {
      const projected = projectionFromSource(remote);
      if (projected !== undefined) return projected;
    }
    if (typeof error.toSerializable === "function") {
      const serialized = (error.toSerializable as () => unknown).call(error);
      if (isPlainRecord(serialized)) {
        const projected = projectionFromSource(serialized);
        if (projected !== undefined) return projected;
      }
    }
    if (typeof error.getContext === "function") {
      const context = (error.getContext as () => unknown).call(error);
      if (isPlainRecord(context)) {
        const projected = projectionFromSource(context);
        if (projected !== undefined) return projected;
      }
    }
    const direct = projectionFromSource(error);
    if (direct !== undefined) return direct;
    if (error instanceof Error && error.message.length > 0) {
      return { message: error.message };
    }
  }
  return { message: "Unexpected error" };
}

/** Formats a projection as operator-facing copy without exposing internals. */
export function formatConsoleError(error: unknown): string {
  return projectConsoleError(error).message;
}
