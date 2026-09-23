import {
  type Context,
  context,
  type Span,
  SpanKind,
  SpanStatusCode,
  trace,
  type Tracer,
} from "@opentelemetry/api";

export function getTracer(scope = "@oatscenter/trellis/telemetry"): Tracer {
  return trace.getTracer(scope);
}

export function getActiveSpan(): Span | undefined {
  return trace.getActiveSpan();
}

export function withSpan<T>(span: Span, fn: () => T): T {
  return context.with(trace.setSpan(context.active(), span), fn);
}

/**
 * Starts one async unit under a span's captured context.
 *
 * Browser context managers only scope synchronous execution. After an await,
 * Trellis-owned continuations must use the explicitly captured span or context;
 * this helper does not preserve ambient context across awaits.
 */
export function withSpanAsync<T>(
  span: Span,
  fn: () => Promise<T>,
): Promise<T> {
  const parent = trace.setSpan(context.active(), span);
  return context.with(parent, fn);
}

export { context, SpanKind, SpanStatusCode, trace };
export type { Context, Span };
