import {
  BaseError,
  type BaseErrorSchema,
  Result,
  describeUnknown,
  isErr,
} from "@oatscenter/result";
import { RemoteError, TrellisErrorDataSchema } from "../errors/RemoteError.ts";
import {
  UnexpectedError,
  ValidationError,
  getBuiltinRpcError,
} from "../errors/index.ts";
import {
  recordTrellisError,
  type TrellisErrorMetricAttributes,
} from "../telemetry/mod.ts";
import { type RuntimeRpcErrorDesc } from "./local_authorization.ts";
import { type StaticDecode } from "typebox";
import { type JsonValue } from "../codec.ts";
import { isDeclaredRpcError } from "./helpers.ts";
import { parseRuntimeSchema } from "./wire.ts";
import { type LoggerLike } from "../globals.ts";

export function isResultLike(
  value: unknown,
): value is Result<unknown, BaseError> {
  return value instanceof Result;
}

export type SerializableRuntimeError = {
  id?: string;
  type: string;
  message: string;
  context?: Record<string, unknown>;
  traceId?: string;
} & Record<string, unknown>;

export type HandlerErrorAnnotationContext = {
  method?: string;
  event?: string;
  live?: string;
  operation?: string;
  jobType?: string;
  requestId?: string;
  service?: string;
  contractId?: string;
  contractDigest?: string;
  traceId?: string;
};

export function compactHandlerErrorContext(
  context: HandlerErrorAnnotationContext,
): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(context).filter(
      ([key, value]) => key !== "traceId" && value !== undefined,
    ),
  );
}

export function sanitizeHandlerErrorContext(error: BaseError): void {
  delete error.getContext().subject;
}

/**
 * The form of an error sent to a caller: serialized, without the failure text
 * or stack of its cause. The ID, trace ID and request ID stay, so the caller can
 * quote them to find the full error in the service's log.
 */
export function callerErrorData(error: BaseError): BaseErrorSchema {
  const data: BaseErrorSchema = error.toSerializable();
  if (data.context === undefined) return data;
  const { causeMessage: _message, causeStack: _stack, ...kept } = data.context;
  return { ...data, context: kept };
}

export function annotateHandlerBoundaryError(
  cause: unknown,
  context: HandlerErrorAnnotationContext,
): BaseError {
  const error =
    cause instanceof BaseError && !(cause instanceof RemoteError)
      ? cause
      : new UnexpectedError({ cause });
  sanitizeHandlerErrorContext(error);
  error.withContext(compactHandlerErrorContext(context));
  error.withTraceId(context.traceId);
  return error;
}

export function recordRuntimeError(
  error: unknown,
  attributes: TrellisErrorMetricAttributes,
): void {
  recordTrellisError(error, {
    messagingSystem: "nats",
    ...attributes,
  });
}

export function reconstructDeclaredRpcError(
  errorNames: readonly string[] | undefined,
  runtimeErrors: readonly RuntimeRpcErrorDesc[] | undefined,
  data: StaticDecode<typeof TrellisErrorDataSchema>,
  json: JsonValue,
): BaseError | ValidationError | UnexpectedError | null {
  if (!isDeclaredRpcError(errorNames, data.type)) {
    return null;
  }

  const runtimeError =
    getBuiltinRpcError(data.type) ??
    runtimeErrors?.find((candidate) => candidate.type === data.type);
  if (!runtimeError) {
    return null;
  }

  const parsed = runtimeError.schema
    ? parseRuntimeSchema(runtimeError.schema, json).take()
    : data;
  if (isErr(parsed)) {
    return parsed.error instanceof ValidationError ||
      parsed.error instanceof UnexpectedError
      ? parsed.error
      : new UnexpectedError({ cause: parsed.error });
  }

  try {
    const reconstructed = runtimeError.fromSerializable(parsed);
    if (reconstructed instanceof BaseError) {
      return reconstructed;
    }
    return new UnexpectedError({
      cause: new Error(
        `RPC error '${data.type}' reconstructed to a non-Trellis error instance`,
      ),
    });
  } catch (cause) {
    return new UnexpectedError({ cause });
  }
}

/**
 * What one failure log line says about where the failure happened. A field the
 * runtime does not know is left out of the line.
 */
export type FailureLogFields = {
  /** RPC method, Operation, event, job type or webhook the failure belongs to. */
  route?: string;
  /** The caller's `request-id`, or the job's inherited request ID. */
  requestId?: string;
  /** W3C trace ID of the request or of the handling span. */
  traceId?: string;
  /** Service name of the runtime that failed. */
  service?: string;
  /** Authenticated caller, never its credentials. */
  caller?: { principal?: string; session?: string };
  /** Extra identifiers for this surface, such as an Operation or job ID. */
  extra?: Record<string, unknown>;
};

/**
 * The cause chain of an error, outermost first, and the root's stack. It
 * never throws: a cause that cannot be read, such as one with a throwing
 * getter, ends the chain with `"[unprintable cause]"`.
 */
function failureCauseChain(error: unknown): {
  causes: { type: string; message: string }[];
  stack?: string;
} {
  const causes: { type: string; message: string }[] = [];
  let stack: string | undefined;
  try {
    let current: unknown = error instanceof Error ? error.cause : undefined;
    for (let depth = 0; current !== undefined && depth < 8; depth++) {
      if (current instanceof Error) {
        causes.push({
          type: describeUnknown(current.name),
          message: describeUnknown(current.message),
        });
        stack = typeof current.stack === "string" ? current.stack : undefined;
        current = current.cause;
      } else {
        causes.push({
          type: typeof current,
          message: describeUnknown(current),
        });
        current = undefined;
      }
    }
  } catch {
    causes.push({ type: "unknown", message: "[unprintable cause]" });
  }
  return stack === undefined ? { causes } : { causes, stack };
}

/**
 * Logs one failure as one structured line, under the error ID the caller
 * receives. The field names match the Rust SDK's (`error_id`, `request_id`,
 * `trace_id`, ...), so one query finds a reference in either language's log.
 * An unexpected failure logs at `error`, an authorization refusal at `warn`,
 * and a declared outcome or invalid input at `info`.
 *
 * @param log - Logger that receives the line.
 * @param error - The failure as sent to the caller.
 * @param fields - Where the failure happened and who asked.
 */
export function logFailure(
  log: LoggerLike,
  error: BaseError,
  fields: FailureLogFields,
): void {
  const context = error.getContext();
  const requestId =
    fields.requestId ??
    (typeof context.requestId === "string" ? context.requestId : undefined);
  const traceId = fields.traceId ?? error.toSerializable().traceId;
  const { causes, stack } = failureCauseChain(error);
  const line: Record<string, unknown> = {
    error_id: error.id,
    ...(requestId === undefined ? {} : { request_id: requestId }),
    ...(traceId === undefined ? {} : { trace_id: traceId }),
    ...(fields.route === undefined ? {} : { route: fields.route }),
    ...(fields.service === undefined ? {} : { service: fields.service }),
    ...(fields.caller?.principal === undefined
      ? {}
      : { caller_principal: fields.caller.principal }),
    ...(fields.caller?.session === undefined
      ? {}
      : { caller_session: fields.caller.session }),
    error_type: error.name,
    ...(typeof context.code === "string" ? { code: context.code } : {}),
    error: error.message,
    ...(causes.length === 0 ? {} : { cause: causes }),
    ...(stack === undefined ? {} : { cause_stack: stack }),
    ...fields.extra,
  };
  if (
    error instanceof UnexpectedError ||
    error.name === "TransportError" ||
    error.name.endsWith("::UnexpectedError")
  ) {
    log.error(line, "request failed");
  } else if (error.name === "AuthError") {
    log.warn(line, "request refused");
  } else {
    log.info(line, "request refused");
  }
}
