import { BaseError, Result, isErr } from "@oatscenter/result";
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
