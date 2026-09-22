import type { StaticDecode, TSchema } from "typebox";

import type { BaseError } from "@qlever-llc/result";
import type { Codec } from "../generated.ts";
import { sha256Base64urlSync } from "./json.ts";
type SubjectParam = `/${string}`;

function subjectToken(value: string): string {
  return btoa(String.fromCharCode(...new TextEncoder().encode(value)))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

/** Encodes one exact generated event descriptor identity for signed metadata. */
export function eventDescriptorIdentity(
  apiId: string,
  eventName: string,
  parameterCount: number,
): string {
  if (!Number.isSafeInteger(parameterCount) || parameterCount < 0) {
    throw new Error(
      "event parameter count must be a non-negative safe integer",
    );
  }
  return `v1.${subjectToken(apiId)}.${
    subjectToken(eventName)
  }.${parameterCount}`;
}

/** Derives one deployment-bound API request subject. */
export function boundApiSubject(
  family: "rpc" | "operation" | "feed",
  apiId: string,
  providerDeploymentId: string,
  action: string,
): string {
  return `${family}.v1.${subjectToken(apiId)}.${
    subjectToken(providerDeploymentId)
  }.${action}`;
}

/** Derives one API-qualified event subject template. */
export function eventSubject(apiId: string, action: string): string {
  return `events.v1.${subjectToken(apiId)}.${action}`;
}

/** Derives an owner control subscription or an exact subject for one Feed instance. */
export function feedControlSubject(
  feedSubject: string,
  ownerInstanceId: string,
  feedId?: string,
): string {
  const ownerSubject = `${feedSubject}.control.${
    subjectToken(ownerInstanceId)
  }`;
  return feedId === undefined
    ? `${ownerSubject}.*`
    : `${ownerSubject}.${subjectToken(feedId)}`;
}

/** Derives the stable queue shared by replicas subscribed to an exact route. */
export function routeQueueGroup(subject: string): string {
  return `trellis.${sha256Base64urlSync(subject)}`;
}

export type Schema<T> = {
  schema: unknown;
  readonly __trellisType?: T;
};

export type SchemaLike<T = unknown> = TSchema | Schema<T> | Codec<T>;

export type SerializableErrorData = {
  id: string;
  type: string;
  message: string;
  context?: Record<string, unknown>;
  traceId?: string;
} & Record<string, unknown>;

export type RpcErrorClass<
  TData extends SerializableErrorData = SerializableErrorData,
  TError extends BaseError = BaseError,
> = {
  fromSerializable(data: TData): TError;
};

export type RuntimeRpcErrorDesc<
  TType extends string = string,
  TSchema extends SchemaLike | undefined = SchemaLike | undefined,
  TError extends BaseError = BaseError,
> = {
  type: TType;
  schema?: TSchema;
  fromSerializable(
    data: TSchema extends SchemaLike ? InferSchemaType<TSchema>
      : SerializableErrorData,
  ): TError;
};

export type InferRuntimeRpcError<T extends RuntimeRpcErrorDesc> = T extends
  RuntimeRpcErrorDesc<string, SchemaLike | undefined, infer TError> ? TError
  : never;

export type InferSchemaType<S> = S extends Schema<infer T> ? T
  : S extends { decode(value: unknown): infer T } ? T
  : S extends TSchema ? StaticDecode<S>
  : unknown;

export function schema<T>(raw: unknown): Schema<T> {
  return { schema: raw } as Schema<T>;
}

export function unwrapSchema(raw: SchemaLike): unknown {
  if (raw && typeof raw === "object" && "schema" in raw) {
    return (raw as Schema<unknown>).schema;
  }
  return raw;
}

/** Exact permission metadata for one contract API surface action. */
export type PermissionAtom = Readonly<{
  /** Source API semantic identity. */
  apiId: string;
  /** Version of the source API surface. */
  apiVersion: `v${number}`;
  /** API surface family. */
  surfaceKind: "rpc" | "operation" | "event" | "feed" | "state";
  /** Exact API-local surface name. */
  surfaceName: string;
  /** Exact action required for the surface. */
  action:
    | "call"
    | "invoke"
    | "observe"
    | "cancel"
    | "control"
    | "publish"
    | "subscribe"
    | "read"
    | "write"
    | "delete"
    | "submit"
    | "process"
    | "consume";
}>;

export type RPCDesc<
  I extends SchemaLike = SchemaLike,
  O extends SchemaLike = SchemaLike,
  E extends readonly string[] | undefined = readonly string[] | undefined,
  TRuntimeErrors extends readonly RuntimeRpcErrorDesc[] | undefined =
    | readonly RuntimeRpcErrorDesc[]
    | undefined,
> = {
  subject: string;
  input: I;
  output: O;
  /** Exact permission required to call this RPC. */
  permission: PermissionAtom;
  callerCapabilities: readonly string[];
  transfer?: { direction: "receive" };
  authRequired?: boolean;
  errors?: E;
  runtimeErrors?: TRuntimeErrors;
  declaredErrorTypes?: readonly string[];
};

export type EventDesc<S extends SchemaLike = SchemaLike> = {
  subject: string;
  /** Canonical API, event, and parameter-shape identity bound into proofs. */
  descriptorIdentity: string;
  params?: readonly SubjectParam[];
  event: S;
  /** Exact permission required to publish this event. */
  publishPermission: PermissionAtom;
  /** Exact permission required to subscribe to this event. */
  subscribePermission: PermissionAtom;
  publishCapabilities: readonly string[];
  subscribeCapabilities: readonly string[];
};

export type FeedDesc<
  I extends SchemaLike = SchemaLike,
  E extends SchemaLike = SchemaLike,
> = {
  subject: string;
  input: I;
  event: E;
  /** Exact permission required to subscribe to this feed. */
  permission: PermissionAtom;
  subscribeCapabilities: readonly string[];
};

export type OperationDesc<
  I extends SchemaLike = SchemaLike,
  P extends SchemaLike | undefined = SchemaLike | undefined,
  O extends SchemaLike | undefined = SchemaLike | undefined,
  E extends readonly string[] | undefined = readonly string[] | undefined,
  TRuntimeErrors extends readonly RuntimeRpcErrorDesc[] | undefined =
    | readonly RuntimeRpcErrorDesc[]
    | undefined,
  U extends SchemaLike | undefined = SchemaLike | undefined,
> = {
  subject: string;
  input: I;
  progress?: P;
  update?: U;
  output?: O;
  /** Exact permissions required by each operation action. */
  permissions: Readonly<{
    invoke: PermissionAtom;
    observe: PermissionAtom;
    cancel: PermissionAtom;
    control: Readonly<Record<string, PermissionAtom>>;
  }>;
  errors?: E;
  runtimeErrors?: TRuntimeErrors;
  declaredErrorTypes?: readonly string[];
  transfer?: {
    direction: "send";
    store?: string;
    key?: `/${string}`;
    contentType?: `/${string}`;
    metadata?: `/${string}`;
    expiresInMs?: number;
    maxBytes?: number;
  };
  signals?: Record<string, { input: SchemaLike }>;
  callerCapabilities: readonly string[];
  observeCapabilities: readonly string[];
  cancelCapabilities: readonly string[];
  controlCapabilities: readonly string[];
  cancel?: boolean;
};

export type RuntimeApi = {
  rpc: Record<string, RPCDesc>;
  operations: Record<string, OperationDesc>;
  events: Record<string, EventDesc>;
  feeds?: Record<string, FeedDesc>;
  subjects: Record<string, unknown>;
};
