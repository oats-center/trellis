/**
 * Base class for all Trellis-specific errors.
 * Extends BaseError and relies on the traceId getter being configured via initTelemetry.
 */
import { BaseError, type BaseErrorSchema } from "@qlever-llc/result";

/**
 * Abstract base class for Trellis errors.
 * Trellis errors automatically include traceId when initTelemetry() has been called
 * and a span is active in the current context.
 *
 * The traceId integration is configured by the telemetry module's initTelemetry() function,
 * which sets up BaseError.traceIdGetter to retrieve the traceId from the active span.
 */
export abstract class TrellisError<
  TData extends BaseErrorSchema = BaseErrorSchema,
> extends BaseError<TData> {
  // TrellisError inherits getTraceId() from BaseError which uses the static traceIdGetter.
  // The traceIdGetter is configured by initTelemetry() in the telemetry module.
}
