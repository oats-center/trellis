import {
  type Context,
  context,
  propagation,
  type Span,
  trace,
} from "@opentelemetry/api";

export interface HeaderCarrier {
  get(key: string): string | undefined;
  set(key: string, value: string): void;
}

export function injectTraceContext(carrier: HeaderCarrier, span?: Span): void {
  const ctx = span ? trace.setSpan(context.active(), span) : context.active();

  propagation.inject(ctx, carrier, {
    set: (c, k, v) => c.set(k, v),
  });
}

export function extractTraceContext(carrier: HeaderCarrier): Context {
  return propagation.extract(context.active(), carrier, {
    get: (c, k) => c.get(k),
    keys: () => [],
  });
}

export function createMapCarrier(): HeaderCarrier {
  const map = new Map<string, string>();
  return {
    get: (key: string) => map.get(key),
    set: (key: string, value: string) => map.set(key, value),
  };
}
