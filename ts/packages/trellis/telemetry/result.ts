import { trace } from "@opentelemetry/api";
import { BaseError } from "@qlever-llc/result";

let traceIdGetterConfigured = false;

export function configureErrorTraceId(): void {
  if (traceIdGetterConfigured) return;
  traceIdGetterConfigured = true;

  BaseError.traceIdGetter = () => {
    const span = trace.getActiveSpan();
    if (!span) return undefined;

    const spanContext = span.spanContext();
    if (spanContext.traceId === "00000000000000000000000000000000") {
      return undefined;
    }
    return spanContext.traceId;
  };
}
