import type { Context, Span } from "@opentelemetry/api";
import { SpanKind } from "@opentelemetry/api";
import { getTracer } from "./core.ts";
import { routeToken, type TrellisRouteFamily } from "./metrics.ts";

const TRELLIS_INSTRUMENTATION_SCOPE = "@oatscenter/trellis";

/** Returns the shared Trellis tracer. */
export function getTrellisTracer() {
  return getTracer(TRELLIS_INSTRUMENTATION_SCOPE);
}

/** Resolves one bounded registered route token for span and metric labels. */
export function trellisRoute(
  family: TrellisRouteFamily,
  descriptor: string,
): string {
  return routeToken(family, descriptor);
}

/** Starts one logical RPC client span for a bounded route token. */
export function startClientSpan(route: string): Span {
  return getTrellisTracer().startSpan("trellis.rpc.client", {
    kind: SpanKind.CLIENT,
    attributes: {
      "trellis.route": route,
    },
  });
}

/** Starts one server RPC span for a bounded route token. */
export function startServerSpan(
  route: string,
  parentContext?: Context,
): Span {
  return getTrellisTracer().startSpan(
    "trellis.rpc.server",
    {
      kind: SpanKind.SERVER,
      attributes: {
        "trellis.route": route,
      },
    },
    parentContext,
  );
}
