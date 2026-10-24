import { type Span } from "../telemetry/mod.ts";
import { type RuntimeApi } from "../participant_runtime/api.ts";
import { TransportError, machineErrorCode } from "../errors/index.ts";
import { type RuntimeRpcErrorDesc } from "./local_authorization.ts";
import { createTransportError } from "./wire.ts";

export function activeTraceId(span: Span): string | undefined {
  const traceId = span.spanContext().traceId;
  return traceId === "00000000000000000000000000000000" ? undefined : traceId;
}

export function traceIdFromTraceparent(
  traceparent: string | undefined,
): string | undefined {
  const [version, traceId, parentId, flags, extra] =
    traceparent?.split("-") ?? [];
  if (
    extra !== undefined ||
    !/^[0-9a-f]{2}$/u.test(version ?? "") ||
    version === "ff" ||
    !/^[0-9a-f]{32}$/u.test(traceId ?? "") ||
    traceId === "00000000000000000000000000000000" ||
    !/^[0-9a-f]{16}$/u.test(parentId ?? "") ||
    parentId === "0000000000000000" ||
    !/^[0-9a-f]{2}$/u.test(flags ?? "")
  ) {
    return undefined;
  }
  return traceId;
}

export const EMPTY_TRELLIS_API: RuntimeApi = {
  rpc: {},
  operations: {},
  events: {},
  lives: {},
  subjects: {},
};

export function isBrowserAuthRequiredError(error: unknown): boolean {
  return machineErrorCode(error) === "session_not_found";
}

export function isDeclaredRpcError(
  errorNames: readonly string[] | undefined,
  type: string,
): boolean {
  return !!errorNames?.includes(type);
}

export function isRuntimeRpcErrorDesc(
  value: unknown,
): value is RuntimeRpcErrorDesc {
  return (
    !!value &&
    (typeof value === "object" || typeof value === "function") &&
    typeof Reflect.get(value, "type") === "string" &&
    typeof Reflect.get(value, "fromSerializable") === "function"
  );
}

export const payloadSizeEncoder = new TextEncoder();

export function payloadByteLength(payload: string | Uint8Array): number {
  return typeof payload === "string"
    ? payloadSizeEncoder.encode(payload).byteLength
    : payload.byteLength;
}

export function causeMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

export function causeLogData(cause: unknown): unknown {
  return cause instanceof Error
    ? { message: cause.message, stack: cause.stack, name: cause.name }
    : cause;
}

export async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Error for an owner whose generation was retired before it could install, so
 * it must not add resources to the disposed scope.
 */
export function ingressRetiredError(id: number): TransportError {
  return createTransportError({
    code: "trellis.transport.ingress_retired",
    message:
      "Trellis stopped installing provider intake for a retired generation.",
    hint: "Retry against the current generation. This is expected during a fast supersede.",
    context: { generationId: id },
  });
}
