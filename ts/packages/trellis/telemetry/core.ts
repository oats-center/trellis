import {
  type Context,
  context,
  type Span,
  SpanKind,
  SpanStatusCode,
  trace,
  type Tracer,
} from "@opentelemetry/api";

export function getTracer(scope = "@qlever-llc/trellis/telemetry"): Tracer {
  return trace.getTracer(scope);
}

export function getActiveSpan(): Span | undefined {
  return trace.getActiveSpan();
}

export function withSpan<T>(span: Span, fn: () => T): T {
  return context.with(trace.setSpan(context.active(), span), fn);
}

export async function withSpanAsync<T>(
  span: Span,
  fn: () => Promise<T>,
): Promise<T> {
  return context.with(trace.setSpan(context.active(), span), fn);
}

export { context, SpanKind, SpanStatusCode, trace };
export type { Context, Span };
