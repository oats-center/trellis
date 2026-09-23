import {
  type Context,
  context,
  propagation,
  ROOT_CONTEXT,
  type Span,
  trace,
} from "@opentelemetry/api";

/** Maximum accepted `traceparent` length in bytes. */
export const TRACEPARENT_MAX_BYTES = 256;
/** Maximum accepted `tracestate` length in bytes. */
export const TRACESTATE_MAX_BYTES = 512;
/** Maximum accepted `tracestate` members. */
export const TRACESTATE_MAX_MEMBERS = 32;

export interface HeaderCarrier {
  get(key: string): string | undefined;
  set(key: string, value: string): void;
}

/** Whether one header name is a whitelisted W3C trace field. */
function isTraceField(key: string): boolean {
  const lower = key.toLowerCase();
  return lower === "traceparent" || lower === "tracestate";
}

/**
 * Injects only the W3C trace fields from one context.
 *
 * A host propagator may normally add baggage or other fields; this carrier
 * whitelists `traceparent` and `tracestate` so untrusted diagnostic metadata
 * never grows beyond the trace-context contract.
 */
export function injectTraceContext(carrier: HeaderCarrier, span?: Span): void {
  const ctx = span ? trace.setSpan(context.active(), span) : context.active();
  const fields = new Map<string, string>();
  propagation.inject(ctx, fields, {
    set: (target: Map<string, string>, key: string, value: string) => {
      if (isTraceField(key)) target.set(key.toLowerCase(), value);
    },
  });
  for (const [key, value] of fields) carrier.set(key, value);
}

/**
 * Extracts a trace context starting from the root context.
 *
 * Extraction never begins from the active context: an absent, malformed,
 * duplicated, or oversized carrier must produce a new local root rather than
 * inheriting an unrelated active span. Invalid carriers never reject a request.
 */
export function extractTraceContext(carrier: HeaderCarrier): Context {
  const traceparent = carrier.get("traceparent") ??
    carrier.get("Traceparent") ??
    carrier.get("TRACEPARENT");
  const tracestate = carrier.get("tracestate") ?? carrier.get("Tracestate") ??
    carrier.get("TRACESTATE");
  if (!validateTraceparent(traceparent)) {
    return ROOT_CONTEXT;
  }
  const pairs = new Map<string, string>([
    ["traceparent", traceparent],
  ]);
  if (tracestate !== undefined && validateTracestate(tracestate)) {
    pairs.set("tracestate", tracestate);
  }
  return propagation.extract(ROOT_CONTEXT, pairs, {
    get: (source: Map<string, string>, key: string) =>
      source.get(key.toLowerCase()),
    keys: (source: Map<string, string>) => [...source.keys()],
  });
}

/**
 * Validates one `traceparent` value against the pinned W3C propagator.
 *
 * Structural widths are checked first, then the value is round-tripped through
 * the propagator so version, flags, and hex syntax are validated exactly as
 * extraction will parse them.
 */
export function validateTraceparent(
  value: string | undefined,
): value is string {
  if (value === undefined) return false;
  const encoder = new TextEncoder();
  if (
    value.length === 0 ||
    encoder.encode(value).length > TRACEPARENT_MAX_BYTES ||
    !/^[\x20-\x7e]*$/.test(value)
  ) {
    return false;
  }
  const parts = value.split("-");
  if (
    parts.length !== 4 ||
    parts[0].length !== 2 ||
    parts[1].length !== 32 ||
    parts[2].length !== 16 ||
    parts[3].length !== 2
  ) {
    return false;
  }
  const probe = new Map<string, string>([["traceparent", value]]);
  const extracted = propagation.extract(ROOT_CONTEXT, probe, {
    get: (source: Map<string, string>, key: string) =>
      source.get(key.toLowerCase()),
    keys: (source: Map<string, string>) => [...source.keys()],
  });
  const spanContext = trace.getSpanContext(extracted);
  return spanContext !== undefined &&
    spanContext.traceId !== "00000000000000000000000000000000";
}

/** Validates one `tracestate` value: bounded size, members, and printable ASCII. */
export function validateTracestate(value: string | undefined): value is string {
  if (value === undefined) return false;
  const encoder = new TextEncoder();
  return value.length > 0 &&
    encoder.encode(value).length <= TRACESTATE_MAX_BYTES &&
    /^[\x20-\x7e]*$/.test(value) &&
    value.split(",").length <= TRACESTATE_MAX_MEMBERS;
}

export function createMapCarrier(): HeaderCarrier {
  const map = new Map<string, string>();
  return {
    get: (key: string) => map.get(key),
    set: (key: string, value: string) => map.set(key, value),
  };
}
