/**
 * Base error types for use with Result<T, E>
 * @module error
 */

import { ulid } from "ulid";
import Type, { type Static } from "typebox";

type StackTraceErrorConstructor = ErrorConstructor & {
  captureStackTrace?: (targetObject: object, constructorOpt?: object) => void;
};

/**
 * Base error serialization schema.
 * All errors serialize to this structure with optional additional fields.
 */
export const BaseErrorSchema = Type.Object({
  id: Type.String(),
  type: Type.String(),
  message: Type.String(),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});
export type BaseErrorSchema = Static<typeof BaseErrorSchema>;

export type BaseErrorOptions = ErrorOptions & {
  context?: Record<string, unknown>;
  id?: string;
  /*
   * Can be set by external modules (like Trellis) to provide trace IDs from their tracing context.
   * Returns undefined by default.
   */
  traceId?: string;
};

/**
 * Base class for all errors used in Result<T, E>.
 * @template TData - The serialized data type for this error
 */
export abstract class BaseError<
  TData extends BaseErrorSchema = BaseErrorSchema,
> extends Error {
  /**
   * Optional callback to provide a trace ID from ambient context (e.g. OpenTelemetry).
   *
   * Trellis (or other consumers) can set this once at startup so all BaseError
   * instances capture a traceId automatically unless explicitly provided.
   */
  static traceIdGetter: (() => string | undefined) | undefined;

  /** Unique identifier for this error instance */
  readonly id: string;

  /** Error type name (used for discrimination) */
  abstract override readonly name: string;

  /** Runtime contextual information */
  readonly #context: Record<string, unknown>;

  /** Trace ID captured at error creation time */
  #traceId: string | undefined;

  constructor(message: string, options?: BaseErrorOptions) {
    const { context, id, traceId, ...errorOptions } = options ?? {};
    super(message, errorOptions);

    this.id = id ?? ulid();
    this.#context = context ?? {};
    this.#traceId = traceId ?? BaseError.traceIdGetter?.();

    const constructor = this.constructor;
    if ("prototype" in constructor) {
      Object.setPrototypeOf(this, constructor.prototype);
    }
    const captureStackTrace = (Error as StackTraceErrorConstructor)
      .captureStackTrace;
    if (captureStackTrace) {
      captureStackTrace(this, this.constructor);
    }
  }

  /**
   * Add contextual information to this error.
   * Useful for adding runtime context like request IDs, user IDs, etc.
   *
   * @param context - Key-value pairs to add to the error context
   * @returns This error instance for chaining
   *
   * @example
   * ```typescript
   * const error = new UnauthorizedError({ reason: "Invalid token" })
   *   .withContext({ userId: "123", requestId: "req-456" });
   * ```
   */
  withContext(context?: Record<string, unknown>): this {
    if (context) {
      Object.assign(this.#context, context);
    }
    return this;
  }

  /**
   * Get the current context object.
   * @returns The context object
   */
  getContext(): Record<string, unknown> {
    return this.#context;
  }

  /**
   * Get the trace ID for this error.
   * Returns the trace ID captured at error creation time.
   *
   * @returns The trace ID string or undefined
   */
  protected getTraceId(): string | undefined {
    return this.#traceId;
  }

  /**
   * Attach a trace ID if one was not captured when the error was created.
   *
   * @param traceId - Trace ID to attach to serialized error data.
   * @returns This error instance for chaining.
   */
  withTraceId(traceId: string | undefined): this {
    if (this.#traceId === undefined && traceId !== undefined) {
      this.#traceId = traceId;
    }
    return this;
  }

  /**
   * Helper method to get base serializable fields.
   * Subclasses should use this to build their complete serializable object.
   *
   * @returns Base error fields (id, type, message, context, traceId)
   */
  protected baseSerializable(): BaseErrorSchema {
    const traceId = this.getTraceId();
    return {
      id: this.id,
      type: this.name,
      message: this.message,
      context: this.#context,
      ...(traceId !== undefined && { traceId }),
    };
  }

  /**
   * Serializes error to a plain object.
   * Subclasses must implement this to return their specific data type.
   *
   * @returns Plain object representation of the error
   */
  abstract toSerializable(): TData;

  /**
   * Serializes error to JSON string.
   *
   * @returns JSON string representation of the error
   */
  toJSON(): string {
    return JSON.stringify(this.toSerializable());
  }
}

/** Longest text {@link describeUnknown} returns, in UTF-16 code units. */
const DESCRIBE_MAX_LENGTH = 4096;
/** Deepest nesting {@link describeUnknown} prints; deeper objects print as `"[Object]"`. */
const DESCRIBE_MAX_DEPTH = 6;
/** Most objects and arrays {@link describeUnknown} prints before eliding the rest. */
const DESCRIBE_MAX_NODES = 256;

/**
 * Describe any thrown value or error cause as bounded text, for a log line.
 * Objects print as JSON, with repeated references as `"[Circular]"`, BigInts
 * as `"<number>n"`, nesting below six levels as `"[Object]"`, and the text cut
 * at 4096 characters. It never throws: when a value cannot be printed, for
 * example because a getter or a Proxy trap throws, it falls back to
 * `String(value)` and then to `"[unprintable cause]"`.
 *
 * @param value - The value to describe.
 * @returns A string of at most 4096 characters plus an ellipsis.
 */
export function describeUnknown(value: unknown): string {
  let text: string;
  try {
    if (typeof value === "object" && value !== null) {
      const depths = new WeakMap<object, number>();
      const seen = new WeakSet<object>();
      let nodes = 0;
      text =
        JSON.stringify(value, function (this: unknown, _key, item: unknown) {
          if (typeof item === "bigint") return `${item}n`;
          if (typeof item !== "object" || item === null) return item;
          if (seen.has(item)) return "[Circular]";
          const parent =
            typeof this === "object" && this !== null
              ? (depths.get(this) ?? -1)
              : -1;
          if (parent + 1 > DESCRIBE_MAX_DEPTH) return "[Object]";
          if (++nodes > DESCRIBE_MAX_NODES) return "[...]";
          seen.add(item);
          depths.set(item, parent + 1);
          return item;
        }) ?? String(value);
    } else {
      text = String(value);
    }
  } catch {
    try {
      text = String(value);
    } catch {
      text = "[unprintable cause]";
    }
  }
  return text.length > DESCRIBE_MAX_LENGTH
    ? `${text.slice(0, DESCRIBE_MAX_LENGTH)}…`
    : text;
}

/**
 * Schema for UnexpectedError serialization.
 */
export const UnexpectedErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("UnexpectedError"),
  message: Type.String(),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});
export type UnexpectedErrorData = Static<typeof UnexpectedErrorDataSchema>;

/**
 * Represents an unexpected error.
 * Use this for wrapping unknown errors or for truly unexpected conditions.
 */
export class UnexpectedError extends BaseError<UnexpectedErrorData> {
  override readonly name = "UnexpectedError" as const;

  constructor(options?: BaseErrorOptions) {
    super("An unexpected error has occurred", options);
    if (options?.cause) {
      // A cause is whatever was thrown, so reading it must not throw: a
      // getter, a Proxy or a cyclic chain would otherwise turn one failure
      // into another before it is reported.
      let causeMessage = "[unprintable cause]";
      let causeStack: string | undefined;
      try {
        let root: unknown = options.cause;
        for (let depth = 0; depth < 8; depth++) {
          if (!(root instanceof Error) || !root.cause) break;
          root = root.cause;
        }
        if (root instanceof Error) {
          causeMessage = describeUnknown(root.message);
          causeStack = typeof root.stack === "string" ? root.stack : undefined;
        } else {
          causeMessage = describeUnknown(root);
        }
      } catch {
        causeStack = undefined;
      }
      this.withContext({ causeMessage, causeStack });
    }
  }

  /**
   * Serializes error to a plain object.
   *
   * @returns Plain object representation of the error
   */
  override toSerializable(): UnexpectedErrorData {
    return this.baseSerializable() as UnexpectedErrorData;
  }
}
