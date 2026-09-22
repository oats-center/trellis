import type { Context, Span } from "@opentelemetry/api";
import { SpanKind } from "@opentelemetry/api";
import { getTracer } from "./core.ts";

const TRELLIS_INSTRUMENTATION_SCOPE = "@qlever-llc/trellis";

export function getTrellisTracer() {
  return getTracer(TRELLIS_INSTRUMENTATION_SCOPE);
}

export function startClientSpan(method: string, subject: string): Span {
  return getTrellisTracer().startSpan(`rpc.client.${method}`, {
    kind: SpanKind.CLIENT,
    attributes: {
      "rpc.system": "nats",
      "rpc.method": method,
      "messaging.destination": subject,
    },
  });
}

export function startServerSpan(
  method: string,
  subject: string,
  parentContext?: Context,
): Span {
  return getTrellisTracer().startSpan(
    `rpc.server.${method}`,
    {
      kind: SpanKind.SERVER,
      attributes: {
        "rpc.system": "nats",
        "rpc.method": method,
        "messaging.destination": subject,
      },
    },
    parentContext,
  );
}
