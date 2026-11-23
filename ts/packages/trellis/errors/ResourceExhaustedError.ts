import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

/** The kinds of resource whose writes JetStream can refuse for lack of room. */
export type ExhaustedResourceKind =
  "kv" | "store" | "state" | "jobs" | "events";

export const ResourceExhaustedErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("ResourceExhaustedError"),
  message: Type.String(),
  kind: Type.Union([
    Type.Literal("kv"),
    Type.Literal("store"),
    Type.Literal("state"),
    Type.Literal("jobs"),
    Type.Literal("events"),
  ]),
  resource: Type.String(),
  reason: Type.String(),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});
export type ResourceExhaustedErrorData = Static<
  typeof ResourceExhaustedErrorDataSchema
>;

/**
 * A write JetStream refused because a resource has no room left.
 *
 * KV, store, State, job and event writes return it when JetStream refuses for
 * lack of storage, memory, streams or consumers, or because a stream holds as
 * much as its limits allow. Nothing was written. Writing again succeeds only
 * once room is made, such as by deleting data or raising the broker's limits.
 */
export class ResourceExhaustedError extends TrellisError<ResourceExhaustedErrorData> {
  override readonly name = "ResourceExhaustedError" as const;
  /** What kind of resource refused the write. */
  readonly kind: ExhaustedResourceKind;
  /**
   * Which one: the KV, store or State resource's name, the job queue's type,
   * or the event's subject.
   */
  readonly resource: string;
  /** What JetStream ran out of, in a few words. */
  readonly reason: string;

  /** Records which resource refused the write, and why. */
  constructor(
    options: ErrorOptions & {
      kind: ExhaustedResourceKind;
      resource: string;
      reason: string;
      context?: Record<string, unknown>;
      id?: string;
    },
  ) {
    const { kind, resource, reason, ...baseOptions } = options;
    super(`${kind} '${resource}' has no room: ${reason}`, baseOptions);
    this.kind = kind;
    this.resource = resource;
    this.reason = reason;
  }

  /**
   * Serializes error to a plain object.
   *
   * @returns Plain object representation of the error
   */
  override toSerializable(): ResourceExhaustedErrorData {
    return {
      ...this.baseSerializable(),
      type: this.name,
      kind: this.kind,
      resource: this.resource,
      reason: this.reason,
    } as ResourceExhaustedErrorData;
  }
}

/**
 * Why JetStream refused a write, when `cause` is a refusal for lack of room.
 *
 * Reads the JetStream error code (`err_code`) when the error or one of its
 * causes carries one, and otherwise the description JetStream gives.
 */
export function jetStreamExhaustion(cause: unknown): string | undefined {
  for (let error = cause, depth = 0; error && depth < 8; depth++) {
    const reason = exhaustionOf(error);
    if (reason !== undefined) return reason;
    error = error instanceof Error ? error.cause : undefined;
  }
  return undefined;
}

function exhaustionOf(cause: unknown): string | undefined {
  const code =
    cause && typeof cause === "object" && "code" in cause
      ? (cause as { code: unknown }).code
      : undefined;
  const text = (
    cause instanceof Error ? cause.message : String(cause)
  ).toLowerCase();
  if (code === 10047 || code === 10002 || code === 10023)
    return "JetStream storage is exhausted";
  if (code === 10028) return "JetStream memory is exhausted";
  if (code === 10027) return "JetStream's stream limit is reached";
  if (code === 10026) return "JetStream's consumer limit is reached";
  if (code === 10077 && text.includes("maximum")) return "the stream is full";
  if (
    text.includes("insufficient storage resources") ||
    text.includes("insufficient resources")
  )
    return "JetStream storage is exhausted";
  if (text.includes("insufficient memory resources"))
    return "JetStream memory is exhausted";
  if (/maximum (bytes|messages)[a-z ]* exceeded/.test(text))
    return "the stream is full";
  return undefined;
}

/**
 * The typed refusal for the `kind` resource `resource`, when `cause` is
 * JetStream refusing a write for lack of room.
 */
export function resourceExhausted(
  kind: ExhaustedResourceKind,
  resource: string,
  cause: unknown,
): ResourceExhaustedError | undefined {
  const reason = jetStreamExhaustion(cause);
  return reason === undefined
    ? undefined
    : new ResourceExhaustedError({ kind, resource, reason, cause });
}
