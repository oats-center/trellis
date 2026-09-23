import {
  type ConsumerMessages,
  jetstream,
  type JetStreamClient,
  jetstreamManager,
  type JsMsg,
} from "@nats-io/jetstream";
import {
  createInbox,
  headers as natsHeaders,
  type Msg,
  type MsgHdrs,
  type NatsConnection,
} from "@nats-io/nats-core";
import type {
  EventDesc,
  FeedDesc,
  InferSchemaType,
  RPCDesc,
} from "./participant.ts";
import {
  boundApiSubject,
  feedControlSubject,
  type PermissionAtom as DescriptorPermissionAtom,
  routeQueueGroup,
  type RuntimeApi,
} from "./participant_runtime/api.ts";
import type { Codec } from "./generated.ts";
import { encodeEventSubjectParameterToken } from "./helpers.ts";
import type { ParticipantKvMetadata } from "./participant_runtime/metadata.ts";
import type { EventConsumerResourceBinding } from "./participant_runtime/schemas.ts";
import type { StaticDecode } from "typebox";
import { buildEventProofInput } from "./auth/proof.ts";
import {
  AuthorizationProviderCache,
  type AuthorizationProviderEvent,
  type AuthorizationProviderRequest,
} from "./auth/authorization_context.ts";
import { AuthorizationProviderUnavailableError } from "./auth/authorization/provider_cache.ts";
import type {
  AuthorizationVerificationErrorCode,
  PermissionAtom as VerifierPermissionAtom,
  VerifiedAuthorizationContextTokenProjection,
} from "./auth/protocol_wasm.ts";
import {
  AsyncResult,
  BaseError,
  err,
  type InferErr,
  isErr,
  type MaybeAsync,
  ok,
  Result,
} from "@oatscenter/result";
import {
  context,
  createNatsHeaderCarrier,
  extractTraceContext,
  injectTraceContext,
  recordTrellisError,
  SpanStatusCode,
  startClientSpan,
  startServerSpan,
  trace,
  type TrellisErrorMetricAttributes,
  withSpanAsync,
} from "./telemetry/mod.ts";
import { Type } from "typebox";
import { AssertError, Pointer } from "typebox/value";
import { ulid } from "ulid";
import {
  encodeSchema,
  type JsonValue,
  parse,
  parseSchema,
  parseUnknownSchema,
} from "./codec.ts";
import {
  AuthError,
  BUILTIN_RPC_ERRORS,
  getBuiltinRpcError,
  machineErrorCode,
  SchemaValidationError,
  type StoreError,
  TransferError,
  TransportError,
  type TrellisErrorInstance,
  type TrellisErrorMap,
  type TrellisErrorName,
  UnexpectedError,
  ValidationError,
} from "./errors/index.ts";
import { RemoteError } from "./errors/RemoteError.ts";
import { logger, type LoggerLike } from "./globals.ts";
import {
  type KvRepresentation,
  type ResourceMigrations,
  TypedKV,
} from "./kv.ts";
import { TrellisErrorDataSchema } from "./errors/RemoteError.ts";
import type {
  ActiveJob,
  JobRef,
  JobTypeMetadata,
  JobUpdatesOptions,
  JobUpdateSubscription,
} from "./jobs.ts";
import type { StoreWaitOptions, TypedStore, TypedStoreEntry } from "./store.ts";
import {
  OperationInvoker,
  type OperationRefData,
  type OperationTransport,
} from "./operations.ts";
import type { Span } from "./telemetry/mod.ts";
import {
  API as STATE_API,
  Conflict as StateRpcConflict,
  type DeleteOutput as StateDeleteResponse,
  type GetOutput as StateGetResponse,
  type PutOutput as StateSetResponse,
} from "./internal_sdk/generated/apis/state/mod.js";
import { API as EVENTS_API } from "./internal_sdk/generated/apis/events/mod.js";
import {
  createTransferHandle,
  type FileInfo,
  type ReceiveTransferGrant,
  type ReceiveTransferHandle,
  type SendTransferGrant,
  type SendTransferHandle,
  type TransferBody,
  type TransferGrant,
} from "./transfer.ts";
import { TrellisTasks } from "./tasks.ts";
import { TrellisConnection } from "./connection.ts";

export type { NatsConnection } from "@nats-io/nats-core";

type RuntimeRpcErrorDesc = {
  type: string;
  schema?: unknown;
  fromSerializable(data: unknown): Error;
};

type InferRuntimeRpcError<T> = T extends {
  fromSerializable(data: unknown): infer TError;
} ? TError
  : never;

/** Caller projection returned after a context-bound proof is verified locally. */
export type VerifiedCaller = {
  type: "verified";
  sessionKey: string;
  principalId: string;
  principalKind: "user" | "service" | "device";
  participantId: string;
  contextDigest: string;
  connectionId: string;
  loginSessionId: string | null;
  identityKeyId: string | null;
  deploymentId: string | null;
  instanceId: string | null;
  grantRevision: number;
  platformPrivileges: ("trellis.auth::admin")[];
  inboxPrefix: string;
};

/** Internal system caller used only by explicitly unauthenticated surfaces. */
export type InternalCaller = {
  type: "internal";
  capabilities: readonly [];
};

/** Caller data exposed to Trellis handlers. */
export type SessionCaller = VerifiedCaller | InternalCaller;

type LocalAuthorizationRequestMessage = Pick<
  Msg,
  "data" | "headers" | "reply" | "subject"
>;
type LocalAuthorizationEventMessage = Pick<Msg, "data" | "headers" | "subject">;

type LocalAuthorizationArgs =
  | {
    kind: "request";
    cache: AuthorizationProviderCache | undefined;
    message: LocalAuthorizationRequestMessage;
    proofPayload?: Uint8Array;
    permission: DescriptorPermissionAtom | undefined;
    requiredCapabilities: readonly string[];
    identityOnly?: boolean;
  }
  | {
    kind: "event";
    cache: AuthorizationProviderCache | undefined;
    message: LocalAuthorizationEventMessage;
    permission: DescriptorPermissionAtom | undefined;
    descriptorIdentity: string;
    requiredCapabilities: readonly string[];
  };

type VerifyAuthorizationRequestResultLike =
  | {
    ok: true;
    contextDigest: string;
    context: VerifiedAuthorizationContextTokenProjection["context"];
  }
  | { ok: false; error: { code: AuthorizationVerificationErrorCode } };
type VerifyAuthorizationEventResultLike = VerifyAuthorizationRequestResultLike;

class EventVerificationAuthError extends AuthError {
  constructor(readonly retryable: boolean) {
    super({
      reason: retryable ? "authorization_unavailable" : "invalid_signature",
    });
  }
}

/** Verifies one received request or event through the provider-local WASM cache. */
export async function verifyLocalAuthorization(
  args: LocalAuthorizationArgs,
): Promise<Result<VerifiedCaller, AuthError>> {
  if (!args.permission && !(args.kind === "request" && args.identityOnly)) {
    return err(new AuthError({ reason: "insufficient_permissions" }));
  }
  if (!args.cache) {
    return err(new AuthError({ reason: "invalid_signature" }));
  }

  const proof = args.message.headers?.get("proof");
  const contextDigest = args.message.headers?.get("authorization-context");
  const sessionKey = args.message.headers?.get("session-key");
  if (!proof || !contextDigest || !sessionKey) {
    return err(new AuthError({ reason: "missing_proof" }));
  }

  let result:
    | VerifyAuthorizationRequestResultLike
    | VerifyAuthorizationEventResultLike;
  try {
    if (args.kind === "request") {
      const iatHeader = args.message.headers?.get("iat");
      const requestId = args.message.headers?.get("request-id");
      const reply = args.message.reply;
      const iat = Number(iatHeader);
      if (!Number.isSafeInteger(iat) || !requestId || !reply) {
        return err(new AuthError({ reason: "invalid_signature" }));
      }
      const request: AuthorizationProviderRequest = {
        contextDigest,
        sessionKey,
        subject: args.message.subject,
        reply,
        payload: args.proofPayload ??
          new Uint8Array(args.message.data ?? new Uint8Array()),
        iat,
        requestId,
        proof,
        requiredPermissions: args.permission
          ? [toVerifierPermission(args.permission)]
          : [],
        requiredCapabilities: [...args.requiredCapabilities],
      };
      result = await args.cache.verifyRequest(request);
    } else {
      const eventId = args.message.headers?.get("Nats-Msg-Id");
      const eventTime = args.message.headers?.get("Trellis-Event-Time");
      const descriptorIdentity = args.message.headers?.get(
        "Trellis-Event-Descriptor",
      );
      if (
        !eventId || !eventTime || descriptorIdentity !== args.descriptorIdentity
      ) {
        return err(new AuthError({ reason: "invalid_signature" }));
      }
      const event: AuthorizationProviderEvent = {
        contextDigest,
        sessionKey,
        descriptorIdentity,
        subject: args.message.subject,
        payload: new Uint8Array(args.message.data ?? new Uint8Array()),
        eventId,
        eventTime,
        proof,
        requiredCapabilities: [...args.requiredCapabilities],
      };
      result = await args.cache.verifyEvent(event);
    }
  } catch (error) {
    if (error instanceof AuthorizationProviderUnavailableError) {
      return err(
        args.kind === "event"
          ? new EventVerificationAuthError(true)
          : new AuthError({ reason: "authorization_unavailable" }),
      );
    }
    return err(new AuthError({ reason: "invalid_signature" }));
  }

  if (!result.ok) {
    return err(
      new AuthError({
        reason: localAuthorizationErrorReason(result.error.code),
      }),
    );
  }
  return ok(toVerifiedCaller(result.contextDigest, result.context));
}

function toVerifierPermission(
  permission: DescriptorPermissionAtom,
): VerifierPermissionAtom {
  if (
    permission.surfaceKind === "operation" && permission.action === "control"
  ) {
    const separator = permission.surfaceName.lastIndexOf(".");
    if (separator <= 0 || separator === permission.surfaceName.length - 1) {
      throw new Error("operation signal permission is invalid");
    }
    return {
      target: {
        kind: "operationSignal",
        api: `${permission.apiId}@${permission.apiVersion}`,
        operation: permission.surfaceName.slice(0, separator),
        signal: permission.surfaceName.slice(separator + 1),
      },
      action: permission.action,
    };
  }
  return {
    target: {
      kind: "apiSurface",
      api: `${permission.apiId}@${permission.apiVersion}`,
      surface: permission.surfaceKind,
      name: permission.surfaceName,
    },
    action: permission.action,
  };
}

function toVerifiedCaller(
  contextDigest: string,
  projection: VerifiedAuthorizationContextTokenProjection["context"],
): VerifiedCaller {
  return {
    type: "verified",
    sessionKey: projection.sessionKey,
    principalId: projection.principalId,
    principalKind: projection.principalKind,
    participantId: projection.participantId,
    contextDigest,
    connectionId: projection.connectionId,
    loginSessionId: projection.loginSessionId,
    identityKeyId: projection.identityKeyId,
    deploymentId: projection.deploymentId,
    instanceId: projection.instanceId,
    grantRevision: projection.grantRevision,
    platformPrivileges: [...projection.platformPrivileges],
    inboxPrefix: projection.inboxPrefix,
  };
}

function localAuthorizationErrorReason(
  code: AuthorizationVerificationErrorCode,
): string {
  switch (code) {
    case "PermissionDenied":
      return "insufficient_permissions";
    case "ReplySubjectMismatch":
      return "reply_subject_mismatch";
    case "ProofIatOutOfRange":
      return "iat_out_of_range";
    case "ContextExpired":
    case "EventRevoked":
      return "session_expired";
    default:
      return "invalid_signature";
  }
}

/**
 * Safely extract JSON from a NATS message.
 * The .json() method can throw if the message data is not valid JSON.
 */
export function safeJson(msg: Msg): Result<JsonValue, UnexpectedError> {
  return Result.try(() => msg.json() as JsonValue);
}

function transportCauseContext(cause: unknown): Record<string, unknown> {
  if (cause instanceof Error) {
    return {
      causeName: cause.name,
      causeMessage: cause.message,
    };
  }

  return { cause: String(cause) };
}

function createTransportError(args: {
  code: string;
  message: string;
  hint: string;
  context?: Record<string, unknown>;
  cause?: unknown;
}): TransportError {
  return new TransportError({
    code: args.code,
    message: args.message,
    hint: args.hint,
    cause: args.cause,
    context: {
      ...(args.context ?? {}),
      ...(args.cause === undefined ? {} : transportCauseContext(args.cause)),
    },
  });
}

function requestFailedTransportError(args: {
  code: string;
  method?: string;
  subject: string;
  hint: string;
  message: string;
  cause?: unknown;
  context?: Record<string, unknown>;
}): TransportError {
  return createTransportError({
    code: args.code,
    message: args.message,
    hint: args.hint,
    cause: args.cause,
    context: {
      subject: args.subject,
      ...(args.method === undefined ? {} : { method: args.method }),
      ...(args.context ?? {}),
    },
  });
}

function classifyRequestTransportFailure(args: {
  method?: string;
  subject: string;
  callerCapabilities?: readonly string[];
  cause: unknown;
}): TransportError {
  const message = args.cause instanceof Error
    ? args.cause.message
    : String(args.cause);
  const isNoResponders = message.includes("no responders");
  const isNatsPermission = args.cause instanceof Error &&
    [
      "PermissionViolationError",
      "AuthorizationError",
      "UserAuthenticationExpiredError",
    ]
      .includes(args.cause.name);

  return requestFailedTransportError({
    code: isNoResponders
      ? "trellis.request.unavailable"
      : isNatsPermission
      ? "trellis.request.denied"
      : "trellis.request.failed",
    message: isNoResponders
      ? "Trellis could not reach the requested capability."
      : isNatsPermission
      ? "Trellis denied this request."
      : "Trellis could not complete the request.",
    hint: isNoResponders
      ? "Check that the target service is installed and reachable, then try again."
      : isNatsPermission
      ? "Sign in with a profile that has the required capability, then try again."
      : "Retry the request. If it keeps failing, check Trellis runtime health.",
    cause: args.cause,
    method: args.method,
    subject: args.subject,
    context: {
      ...(args.callerCapabilities === undefined
        ? {}
        : { requiredCapabilities: args.callerCapabilities }),
      noResponders: isNoResponders,
      lowLevelMessage: message,
    },
  });
}

/** Creates the existing typed transport error for an unavailable optional action. */
export function createActionUnavailableError(
  action: string,
  capabilities: readonly string[],
): TransportError {
  return requestFailedTransportError({
    code: "trellis.request.unavailable",
    message: "Trellis could not reach the requested capability.",
    hint:
      "Wait for the optional capability to become available, then try again.",
    subject: action,
    context: { action, capabilities },
  });
}

function encodeRuntimeSchema(
  schema: unknown,
  data: unknown,
): Result<string, SchemaValidationError | ValidationError | UnexpectedError> {
  if (
    schema && typeof schema === "object" &&
    typeof Reflect.get(schema, "encode") === "function"
  ) {
    try {
      const encoded = (schema as Codec<unknown>).encode(data);
      const json = JSON.stringify(encoded);
      if (json === undefined) {
        throw new TypeError("Codec encoded no JSON value");
      }
      return ok(json);
    } catch (cause) {
      return err(new UnexpectedError({ cause }));
    }
  }
  return encodeSchema(schema as never, data);
}

function parseRuntimeSchema(
  schema: unknown,
  data: JsonValue,
): Result<unknown, SchemaValidationError | ValidationError | UnexpectedError> {
  if (
    schema && typeof schema === "object" &&
    typeof Reflect.get(schema, "decode") === "function"
  ) {
    try {
      return ok((schema as Codec<unknown>).decode(data));
    } catch (cause) {
      return err(new UnexpectedError({ cause }));
    }
  }
  return parseUnknownSchema(
    schema as Parameters<typeof parseUnknownSchema>[0],
    data,
  );
}

export function base64urlEncode(data: Uint8Array): string {
  const b64 = btoa(String.fromCharCode(...data));
  return b64.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

export function base64urlDecode(s: string): Uint8Array {
  const normalized = s.replace(/-/g, "+").replace(/_/g, "/");
  const padLen = (4 - (normalized.length % 4)) % 4;
  const padded = normalized + "=".repeat(padLen);
  const bin = atob(padded);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  const buf = data.buffer;
  if (buf instanceof ArrayBuffer) {
    return buf.slice(data.byteOffset, data.byteOffset + data.byteLength);
  }
  const copy = new Uint8Array(data.byteLength);
  copy.set(data);
  return copy.buffer;
}

export async function sha256(data: Uint8Array): Promise<Uint8Array> {
  const digest = await crypto.subtle.digest("SHA-256", toArrayBuffer(data));
  return new Uint8Array(digest);
}

export function buildProofInput(
  contextDigest: string,
  subject: string,
  reply: string,
  payloadHash: Uint8Array,
  iat: number,
  requestId: string,
): Uint8Array {
  const enc = new TextEncoder();
  const contextDigestBytes = base64urlDecode(contextDigest);
  if (contextDigestBytes.length !== 32) {
    throw new Error("authorization context digest must encode 32 bytes");
  }
  if (reply.length === 0) {
    throw new Error("request reply subject must not be empty");
  }
  const domainBytes = enc.encode(
    "trellis.authorization-request-proof.v1",
  );
  const subjectBytes = enc.encode(subject);
  const replyBytes = enc.encode(reply);
  const iatBytes = enc.encode(String(iat));
  const requestIdBytes = enc.encode(requestId);

  const components = [
    domainBytes,
    contextDigestBytes,
    subjectBytes,
    replyBytes,
    payloadHash,
    iatBytes,
    requestIdBytes,
  ];
  const total = components.reduce((sum, value) => sum + 4 + value.length, 0);
  const buf = new Uint8Array(total);
  const view = new DataView(buf.buffer);

  let offset = 0;
  for (const component of components) {
    view.setUint32(offset, component.length);
    offset += 4;
    buf.set(component, offset);
    offset += component.length;
  }

  return buf;
}

export type TrellisSigner = (
  data: Uint8Array,
) => Promise<Uint8Array> | Uint8Array;

export type TrellisAuth = {
  sessionKey: string;
  sign: TrellisSigner;
  currentIat?: () => number;
  /** Current authorization-context digest bound into v1 request and event proofs. */
  contextDigest?: string | (() => string);
  /** Provider-only local verifier for service request and event handling. */
  authorizationProviderCache?: AuthorizationProviderCache;
};

export type TrellisMode = "client" | "service";
type Simplify<T> = { [K in keyof T]: T[K] } & {};
type OwnedApiFor<TContract> = TContract extends
  { implements: readonly unknown[] } ? RuntimeApi
  : never;
type ContractKvFor<TContract> = TContract extends {
  resources: infer TResources extends Readonly<Record<string, unknown>>;
} ? {
    [
      K in keyof TResources as TResources[K] extends { kind: "kv" } ? K
        : never
    ]: TResources[K] extends {
      codec: infer TCodec;
      availability: infer TAvailability;
    } ? {
        value: TCodec extends Codec<infer TValue> ? TValue : unknown;
        schema: TCodec;
        required: TAvailability extends "required" ? true : false;
      }
      : never;
  }
  : {};
type ContractJobsFor<TContract> = TContract extends {
  resources: infer TResources extends Readonly<Record<string, unknown>>;
} ? {
    [
      K in keyof TResources as TResources[K] extends { kind: "job" } ? K
        : never
    ]: TResources[K] extends {
      payload: infer TPayload;
      result: infer TResult;
      update: infer TUpdate;
    } ? {
        payload: TPayload extends Codec<infer TValue> ? TValue : unknown;
        result: TResult extends Codec<infer TValue> ? TValue : void;
        update: TUpdate extends Codec<infer TValue> ? TValue : never;
      }
      : never;
  }
  : {};
export type RuntimeStateStoreShape = {
  kind: "value";
  value: unknown;
  codec: Codec<unknown>;
  version: number;
  migrations: Readonly<Record<number, Codec<unknown>>>;
};
export type RuntimeStateStores = Record<string, RuntimeStateStoreShape>;
export type RuntimeStateStoresForContract<TContract> = TContract extends {
  resources: infer TResources extends Readonly<Record<string, unknown>>;
} ? {
    [
      K in keyof TResources as TResources[K] extends { kind: "state" } ? K
        : never
    ]: TResources[K] extends { codec: infer TCodec } ? {
        kind: "value";
        value: TCodec extends Codec<infer TValue> ? TValue : unknown;
        codec: TCodec;
        version: TResources[K] extends { version: infer TVersion } ? TVersion
          : number;
        migrations: TResources[K] extends { migrations: infer TMigrations }
          ? TMigrations
          : {};
      }
      : never;
  }
  : {};
type TrellisApiFor<TContract> = OwnedApiFor<TContract>;
type RpcMethodsOf<TA extends RuntimeApi> = TA["rpc"];
export type MethodsOf<TA extends RuntimeApi> =
  & keyof RpcMethodsOf<TA>
  & string;
export type RpcMethodNameOf<TA extends RuntimeApi> = MethodsOf<TA>;
export type OperationsOf<TA extends RuntimeApi> =
  & keyof TA["operations"]
  & string;
type EventsOf<TA extends RuntimeApi> = keyof TA["events"] & string;
export type FeedsOf<TA extends RuntimeApi> =
  & keyof NonNullable<TA["feeds"]>
  & string;
type RpcMethodOf<TA extends RuntimeApi, M extends keyof TA["rpc"] & string> =
  RpcMethodsOf<TA>[M];
type MethodInputOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> = RpcMethodOf<TA, M> extends { input: infer TInput } ? InferSchemaType<TInput>
  : never;
export type RpcInputOf<
  TA extends RuntimeApi,
  M extends RpcMethodNameOf<TA>,
> = MethodInputOf<TA, M>;
type MethodOutputOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> = RpcMethodOf<TA, M> extends { output: infer TOutput }
  ? InferSchemaType<TOutput>
  : never;
export type RpcOutputOf<
  TA extends RuntimeApi,
  M extends RpcMethodNameOf<TA>,
> = MethodOutputOf<TA, M>;
type RpcRequestShapes<TA extends RuntimeApi> = {
  [M in keyof TA["rpc"] & string]: {
    input: MethodInputOf<TA, M>;
    output: MethodOutputOf<TA, M>;
  };
};
type RequestMethodOf<TRequests> = keyof TRequests & string;
type RequestInputOf<TRequests, M extends RequestMethodOf<TRequests>> =
  TRequests[M] extends { input: infer TInput } ? TInput : never;
type RequestOutputOf<TRequests, M extends RequestMethodOf<TRequests>> =
  TRequests[M] extends { output: infer TOutput } ? TOutput : never;
type RpcDescriptorOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> = RpcMethodOf<TA, M> extends {
  input: infer TInput;
  output: infer TOutput;
  errors?: infer TErrors;
  runtimeErrors?: infer TRuntimeErrors;
  declaredErrorTypes?: infer TDeclaredErrorTypes;
} ? {
    input: TInput;
    output: TOutput;
    errors?: TErrors;
    runtimeErrors?: TRuntimeErrors;
    declaredErrorTypes?: TDeclaredErrorTypes;
  } & RpcMethodOf<TA, M>
  : never;
type DeclaredBuiltinErrorOf<TNames> = TNames extends readonly (infer TName)[]
  ? TName extends TrellisErrorName ? TrellisErrorMap[TName]
  : never
  : never;
type DeclaredRuntimeErrorOf<TRuntimeErrors> = TRuntimeErrors extends readonly (
  infer TRuntimeError
)[] ? InferRuntimeRpcError<TRuntimeError>
  : never;
type MethodDeclaredErrorOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> = RpcDescriptorOf<TA, M> extends {
  errors?: infer TErrors;
  runtimeErrors?: infer TRuntimeErrors;
} ? DeclaredBuiltinErrorOf<TErrors> | DeclaredRuntimeErrorOf<TRuntimeErrors>
  : never;
type RequestErrorOf<TA extends RuntimeApi, M extends MethodsOf<TA>> =
  | MethodDeclaredErrorOf<TA, M>
  | RemoteError
  | TransportError
  | ValidationError
  | UnexpectedError;
type HandlerErrorOf<TA extends RuntimeApi, M extends MethodsOf<TA>> =
  | MethodDeclaredErrorOf<TA, M>
  | TrellisErrorInstance;

type OperationDescriptorOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends {
  input: infer TInput;
  progress?: infer TProgress;
  output?: infer TOutput;
  errors?: infer TErrors;
  runtimeErrors?: infer TRuntimeErrors;
  declaredErrorTypes?: infer TDeclaredErrorTypes;
} ? {
    input: TInput;
    progress?: TProgress;
    output?: TOutput;
    errors?: TErrors;
    runtimeErrors?: TRuntimeErrors;
    declaredErrorTypes?: TDeclaredErrorTypes;
  } & TA["operations"][O]
  : never;

type OperationDeclaredErrorOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = OperationDescriptorOf<TA, O> extends {
  errors?: infer TErrors;
  runtimeErrors?: infer TRuntimeErrors;
} ? DeclaredBuiltinErrorOf<TErrors> | DeclaredRuntimeErrorOf<TRuntimeErrors>
  : never;

export type OperationHandlerErrorOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = OperationDeclaredErrorOf<TA, O> | TrellisErrorInstance;
type EventMessageOf<TA extends RuntimeApi, E extends EventsOf<TA>> =
  TA["events"][E] extends EventDesc<infer TEvent> ? InferSchemaType<TEvent>
    : never;
type EventOf<TA extends RuntimeApi, E extends EventsOf<TA>> = EventMessageOf<
  TA,
  E
>;
type EventDescriptorOf<TA extends RuntimeApi, E extends EventsOf<TA>> =
  TA["events"][E] extends EventDesc<infer TEvent>
    ? EventDesc<TEvent> & TA["events"][E]
    : never;
type EventPayloadOf<TA extends RuntimeApi, E extends EventsOf<TA>> =
  & EventOf<TA, E>
  & Record<string, unknown>;
/** Runtime metadata assigned to every Trellis event message. */
export type TrellisEventHeader = Readonly<{
  /** Stable event id used for event identity and JetStream de-duplication. */
  id: string;
  /** Event creation time in ISO-8601 format. */
  time: string;
}>;
/** Event body plus Trellis runtime event metadata. */
export type TrellisEventMessage<
  TBody extends Record<string, unknown> = Record<string, unknown>,
> = Readonly<{
  /** User-authored contract event body. */
  body: Readonly<TBody>;
  /** Runtime metadata assigned by Trellis. */
  header: TrellisEventHeader;
}>;
/** A fully encoded event whose subject, payload, and headers are stable. */
export type PreparedTrellisEvent<
  TPayload extends Record<string, unknown> = Record<string, unknown>,
> = Readonly<{
  event: string;
  descriptorIdentity: string;
  subject: string;
  /** Runtime event metadata assigned when the event was prepared. */
  header: TrellisEventHeader;
  payload: Readonly<TPayload>;
  encodedPayload: string;
  headers: Readonly<Record<string, string>>;
}>;
export type FeedInputOf<TA extends RuntimeApi, F extends FeedsOf<TA>> =
  NonNullable<TA["feeds"]>[F] extends FeedDesc<infer TInput, infer _TEvent>
    ? InferSchemaType<TInput>
    : never;
export type FeedEventOf<TA extends RuntimeApi, F extends FeedsOf<TA>> =
  NonNullable<TA["feeds"]>[F] extends FeedDesc<infer _TInput, infer TEvent>
    ? InferSchemaType<TEvent>
    : never;
type FeedDescriptorOf<TA extends RuntimeApi, F extends FeedsOf<TA>> =
  NonNullable<TA["feeds"]>[F] extends FeedDesc<infer TInput, infer TEvent>
    ? FeedDesc<TInput, TEvent> & NonNullable<TA["feeds"]>[F]
    : never;
export type OperationInputOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { input: infer TInput }
  ? InferSchemaType<TInput>
  : never;
export type OperationProgressOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { progress?: infer TProgress }
  ? TProgress extends undefined ? unknown
  : InferSchemaType<NonNullable<TProgress>>
  : unknown;
/** Infers the transient update payload for an operation descriptor. */
export type OperationUpdateOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { update?: infer TUpdate }
  ? TUpdate extends undefined ? unknown : InferSchemaType<NonNullable<TUpdate>>
  : unknown;
export type OperationOutputOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { output?: infer TOutput }
  ? TOutput extends undefined ? unknown : InferSchemaType<NonNullable<TOutput>>
  : unknown;
export type OperationRuntimeHandle<
  TProgress,
  TOutput,
  TError extends BaseError,
  TUpdate = unknown,
> = {
  id: string;
  started(): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  progress(
    value: TProgress,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  /** Emits a transient update without changing the persisted snapshot. */
  emitUpdate(value: TUpdate): AsyncResult<void, BaseError>;
  complete(
    value: TOutput,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  fail(
    error: TError,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  cancel(): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  attach(
    job: { wait(): AsyncResult<unknown, BaseError> },
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  signals(): AsyncIterable<RuntimeOperationSignal>;
  nextSignal(
    name?: string,
  ): AsyncResult<RuntimeOperationSignal, BaseError>;
  /** Durably acknowledges a signal after its side effects have been accepted. */
  acknowledgeSignal(sequence: number): AsyncResult<void, BaseError>;
  defer(): OperationDeferred;
};
export type OperationDeferred = {
  kind: "deferred";
};

/**
 * Returns true when a handler result explicitly leaves operation completion to
 * an external control path.
 */
export function isOperationDeferred(
  value: unknown,
): value is OperationDeferred {
  return !!value && typeof value === "object" &&
    "kind" in value && value.kind === "deferred";
}
export type AcceptedOperation<
  TProgress,
  TOutput,
  TError extends BaseError,
  TUpdate = unknown,
> =
  & OperationRuntimeHandle<TProgress, TOutput, TError, TUpdate>
  & {
    ref: OperationRefData;
    snapshot: RuntimeOperationSnapshot & {
      progress?: TProgress;
      output?: TOutput;
    };
  };
export type OperationTransferHandle = {
  updates(): AsyncIterable<RuntimeOperationTransferProgress>;
  completed(): AsyncResult<FileInfo, TransferError>;
};
/** Decoded current State value and its opaque storage revision. */
export type StateValue<T> = Readonly<{
  value: T;
  revision: string;
  createdAt: string;
  updatedAt: string;
}>;

/** State CAS failure with the authoritative current value, when present. */
export class StateConflictError<T> extends BaseError {
  override readonly name = "StateConflictError" as const;
  readonly current?: StateValue<T>;

  constructor(current?: StateValue<T>) {
    super("State revision conflict");
    this.current = current;
  }

  override toSerializable() {
    return this.baseSerializable();
  }
}

/** Raised when an optional participant resource is not installed. */
export class ResourceUnavailableError extends BaseError {
  override readonly name = "ResourceUnavailableError" as const;

  /** Construct an unavailable-resource failure. */
  constructor(readonly kind: string, readonly resourceName: string) {
    super(`${kind} resource '${resourceName}' is unavailable`);
  }

  override toSerializable() {
    return this.baseSerializable();
  }
}

/** Typed single-value State handle. */
export type ValueStateStoreClient<TValue> = StateHandle<TValue>;
export type StateFacade<TState extends RuntimeStateStores> = {
  [K in keyof TState]: ValueStateStoreClient<TState[K]["value"]>;
};
export type OperationHandlerContext<
  TInput,
  TProgress,
  TOutput,
  TTransfer,
  TError extends BaseError,
  TUpdate = unknown,
> = {
  input: TInput;
  op: OperationRuntimeHandle<TProgress, TOutput, TError, TUpdate>;
  caller: SessionCaller;
  /** Aborted when cancellation is committed or this executor loses ownership. */
  signal: AbortSignal;
  /** Whether this handler invocation reclaimed an expired executor lease. */
  resuming: boolean;
  /** Last durably persisted progress available to a resumed handler. */
  progress?: TProgress;
} & (TTransfer extends undefined ? {} : { transfer: TTransfer });
export type OperationRegistration<
  TInput,
  TProgress,
  TOutput,
  TTransfer,
  TError extends BaseError,
  TUpdate = unknown,
> = {
  /**
   * Loads an existing operation by id and returns a service-side control handle.
   * The operation must belong to this service and registration name.
   */
  control(
    operationId: string,
  ): AsyncResult<
    OperationRuntimeHandle<TProgress, TOutput, TError, TUpdate>,
    BaseError
  >;
  handle(
    handler: (
      context: OperationHandlerContext<
        TInput,
        TProgress,
        TOutput,
        TTransfer,
        TError,
        TUpdate
      >,
    ) => unknown | Promise<unknown>,
  ): Promise<void>;
};
export type OperationTransferContextOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { transfer: infer TTransfer }
  ? TTransfer extends undefined ? undefined
  : OperationTransferHandle
  : undefined;
export type OperationSurface<
  TA extends RuntimeApi,
  TMode extends TrellisMode,
  O extends OperationsOf<TA>,
> = TMode extends "service" ? OperationRegistration<
    OperationInputOf<TA, O>,
    OperationProgressOf<TA, O>,
    OperationOutputOf<TA, O>,
    OperationTransferContextOf<TA, O>,
    OperationHandlerErrorOf<TA, O>,
    OperationUpdateOf<TA, O>
  >
  : OperationInvoker<TA["operations"][O] & RuntimeOperationDesc>;

export function isResultLike(
  value: unknown,
): value is Result<unknown, BaseError> {
  return value instanceof Result;
}

type SerializableRuntimeError = {
  id?: string;
  type: string;
  message: string;
  context?: Record<string, unknown>;
  traceId?: string;
} & Record<string, unknown>;

export type HandlerErrorAnnotationContext = {
  method?: string;
  event?: string;
  feed?: string;
  operation?: string;
  jobType?: string;
  requestId?: string;
  service?: string;
  contractId?: string;
  contractDigest?: string;
  traceId?: string;
};

function compactHandlerErrorContext(
  context: HandlerErrorAnnotationContext,
): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(context).filter(([key, value]) =>
      key !== "traceId" && value !== undefined
    ),
  );
}

function sanitizeHandlerErrorContext(error: BaseError): void {
  delete error.getContext().subject;
}

export function annotateHandlerBoundaryError(
  cause: unknown,
  context: HandlerErrorAnnotationContext,
): BaseError {
  const error = cause instanceof BaseError && !(cause instanceof RemoteError)
    ? cause
    : new UnexpectedError({ cause });
  sanitizeHandlerErrorContext(error);
  error.withContext(compactHandlerErrorContext(context));
  error.withTraceId(context.traceId);
  return error;
}

function recordRuntimeError(
  error: unknown,
  attributes: TrellisErrorMetricAttributes,
): void {
  recordTrellisError(error, {
    messagingSystem: "nats",
    ...attributes,
  });
}

export type RuntimeOperationDesc = {
  subject: string;
  input: unknown;
  revision: number;
  progress?: unknown;
  update?: unknown;
  output?: unknown;
  signals?: Record<string, { input: unknown }>;
  cancelCapabilities?: readonly string[];
  controlCapabilities?: readonly string[];
  transfer?: {
    store: string;
    key: `/${string}`;
    contentType?: `/${string}`;
    metadata?: `/${string}`;
    expiresInMs?: number;
    maxBytes?: number;
  };
  cancel?: boolean;
};

export type RuntimeOperationSignal = {
  operationId: string;
  sequence: number;
  requestId: string;
  signal: string;
  input?: JsonValue;
  acceptedAt: string;
  acknowledged: boolean;
};

export type RuntimeOperationSignalWaiter = (
  result: Result<RuntimeOperationSignal, BaseError>,
) => void;

export type RuntimeOperationTransferProgress = {
  chunkIndex: number;
  chunkBytes: number;
  transferredBytes: number;
};

export type RuntimeOperationState =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export type RuntimeOperationSnapshot = {
  id: string;
  service: string;
  operation: string;
  revision: number;
  state: RuntimeOperationState;
  createdAt: string;
  updatedAt: string;
  completedAt?: string;
  progress?: unknown;
  transfer?: RuntimeOperationTransferProgress;
  output?: unknown;
  error?: SerializableRuntimeError;
};

export type RuntimeOperationRecord = {
  id: string;
  service: string;
  operation: string;
  callerSessionKey: string;
  invocationDigest: string;
  caller: VerifiedCaller;
  creatorPrincipalId: string;
  creatorParticipantId: string;
  apiId: string;
  input: unknown;
  revision: number;
  ownerInstanceId: string;
  ownerConnectionId: string;
  ownerEpoch: number;
  leaseExpiresAt: string;
  cancelRequestedAt?: string;
  transferGrant?: SendTransferGrant;
  snapshot: RuntimeOperationSnapshot;
  sequence: number;
  signalSequence: number;
  signals: RuntimeOperationSignal[];
  terminal: boolean;
  watchers: Map<string, { includeUpdates: boolean }>;
  frameQueue: Promise<void>;
  signalWaiters: Set<RuntimeOperationSignalWaiter>;
  cancellation: AbortController;
  reclaimed?: boolean;
};

export type DurableOperationRecord = {
  invocationId: string;
  callerSessionKey: string;
  invocationDigest: string;
  caller: VerifiedCaller;
  creatorPrincipalId: string;
  creatorParticipantId: string;
  apiId: string;
  operation: string;
  input: unknown;
  revision: number;
  ownerInstanceId: string;
  ownerConnectionId: string;
  ownerEpoch: number;
  leaseExpiresAt: string;
  cancelRequestedAt?: string;
  transferGrant?: SendTransferGrant;
  sequence: number;
  signalSequence: number;
  signals: RuntimeOperationSignal[];
  snapshot: RuntimeOperationSnapshot;
};

const DurableOperationSignalSchema = Type.Object({
  operationId: Type.String(),
  sequence: Type.Number(),
  requestId: Type.String(),
  signal: Type.String(),
  input: Type.Optional(Type.Any()),
  acceptedAt: Type.String(),
  acknowledged: Type.Boolean(),
});

const DurableOperationSnapshotSchema = Type.Object({
  id: Type.String(),
  service: Type.String(),
  operation: Type.String(),
  revision: Type.Number(),
  state: Type.Union([
    Type.Literal("pending"),
    Type.Literal("running"),
    Type.Literal("completed"),
    Type.Literal("failed"),
    Type.Literal("cancelled"),
  ]),
  createdAt: Type.String(),
  updatedAt: Type.String(),
  completedAt: Type.Optional(Type.String()),
  progress: Type.Optional(Type.Any()),
  transfer: Type.Optional(Type.Object({
    chunkIndex: Type.Number(),
    chunkBytes: Type.Number(),
    transferredBytes: Type.Number(),
  })),
  output: Type.Optional(Type.Any()),
  error: Type.Optional(Type.Object({
    id: Type.Optional(Type.String()),
    type: Type.String(),
    message: Type.String(),
    context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
    traceId: Type.Optional(Type.String()),
  })),
});

export const DurableOperationRecordSchema = Type.Object({
  invocationId: Type.String(),
  callerSessionKey: Type.String(),
  apiId: Type.String(),
  operation: Type.String(),
  input: Type.Any(),
  ownerInstanceId: Type.String(),
  ownerConnectionId: Type.String(),
  ownerEpoch: Type.Number(),
  leaseExpiresAt: Type.String(),
  cancelRequestedAt: Type.Optional(Type.String()),
  transferGrant: Type.Optional(Type.Any()),
  sequence: Type.Number(),
  signalSequence: Type.Number(),
  signals: Type.Array(DurableOperationSignalSchema),
  snapshot: DurableOperationSnapshotSchema,
});

export type RuntimeOperationAcceptedEnvelope = {
  kind: "accepted";
  ref: OperationRefData;
  snapshot: RuntimeOperationSnapshot;
  transfer?: SendTransferGrant;
};

export type RuntimeOperationControlRequest =
  | {
    action: "get" | "cancel";
    operationId: string;
  }
  | {
    action: "watch";
    operationId: string;
    includeUpdates?: boolean;
  }
  | {
    action: "signal";
    operationId: string;
    signal: string;
    input?: JsonValue;
  };

export type RuntimeOperationController = {
  get(
    operationId: string,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  started(
    operationId: string,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  progress(
    operationId: string,
    progress: unknown,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  complete(
    operationId: string,
    output: unknown,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  fail(
    operationId: string,
    error: BaseError,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  cancel(
    operationId: string,
  ): AsyncResult<RuntimeOperationSnapshot, BaseError>;
  signals(operationId: string): AsyncIterable<RuntimeOperationSignal>;
  nextSignal(
    operationId: string,
    name?: string,
  ): AsyncResult<RuntimeOperationSignal, BaseError>;
};

export function buildRuntimeOperationSnapshot(
  runtime: Pick<
    RuntimeOperationRecord,
    "id" | "service" | "operation" | "snapshot"
  >,
  state: RuntimeOperationState,
  patch?: Partial<RuntimeOperationSnapshot>,
): RuntimeOperationSnapshot {
  const updatedAt = new Date().toISOString();
  const completedAt =
    state === "completed" || state === "failed" || state === "cancelled"
      ? (patch?.completedAt ?? updatedAt)
      : patch?.completedAt;
  return {
    id: runtime.id,
    service: runtime.service,
    operation: runtime.operation,
    revision: patch?.revision ?? runtime.snapshot.revision + 1,
    state,
    createdAt: patch?.createdAt ?? runtime.snapshot.createdAt,
    updatedAt,
    ...(completedAt ? { completedAt } : {}),
    ...(patch?.progress !== undefined
      ? { progress: patch.progress }
      : runtime.snapshot.progress !== undefined
      ? { progress: runtime.snapshot.progress }
      : {}),
    ...(patch?.transfer !== undefined
      ? { transfer: patch.transfer }
      : runtime.snapshot.transfer !== undefined
      ? { transfer: runtime.snapshot.transfer }
      : {}),
    ...(patch?.output !== undefined
      ? { output: patch.output }
      : runtime.snapshot.output !== undefined
      ? { output: runtime.snapshot.output }
      : {}),
    ...(patch?.error
      ? { error: patch.error }
      : runtime.snapshot.error
      ? { error: runtime.snapshot.error }
      : {}),
  };
}

function isRuntimeOperationSnapshot(
  value: unknown,
): value is RuntimeOperationSnapshot {
  return !!value && typeof value === "object" &&
    typeof (value as RuntimeOperationSnapshot).id === "string" &&
    typeof (value as RuntimeOperationSnapshot).service === "string" &&
    typeof (value as RuntimeOperationSnapshot).operation === "string" &&
    typeof (value as RuntimeOperationSnapshot).revision === "number" &&
    typeof (value as RuntimeOperationSnapshot).state === "string" &&
    typeof (value as RuntimeOperationSnapshot).createdAt === "string" &&
    typeof (value as RuntimeOperationSnapshot).updatedAt === "string";
}

export function isTerminalRuntimeOperationSnapshot(
  value: unknown,
): value is RuntimeOperationSnapshot {
  return isRuntimeOperationSnapshot(value) && (
    value.state === "completed" || value.state === "failed" ||
    value.state === "cancelled"
  );
}

type NoResponderRetryOpts = {
  maxAttempts?: number;
  baseDelayMs?: number;
};

export type TrellisOpts<TA extends RuntimeApi> = {
  log?: LoggerLike;
  timeout?: number;
  stream?: string;
  noResponderRetry?: NoResponderRetryOpts;
  api?: TA;
  state?: RuntimeStateStores;
  stateMigrations?: Readonly<
    Record<string, ResourceMigrations<unknown> | undefined>
  >;
  resourceGeneration?: () => number;
  resourceAvailability?: (name: string) => boolean;
  connection?: TrellisConnection;
  onSessionNotFound?: () => MaybePromise<void>;
  contractId?: string;
  contractDigest?: string;
};

export type RequestOpts = {
  timeout?: number;
  signal?: AbortSignal;
};

const FEED_CANCEL_TOMBSTONE_MS = 30_000;
const MAX_FEED_CANCEL_TOMBSTONES = 1_024;
const MAX_OPERATION_INPUT_BYTES = 256 * 1024;
const MAX_OPERATION_PROGRESS_BYTES = 64 * 1024;
const MAX_OPERATION_OUTPUT_BYTES = 512 * 1024;
const MAX_OPERATION_ERROR_BYTES = 32 * 1024;
const MAX_OPERATION_SIGNALS = 100;
const MAX_OPERATION_SIGNAL_BYTES = 64 * 1024;
const MAX_OPERATION_RECORD_BYTES = 1024 * 1024;

export type EventOpts = {
  mode?: "durable" | "ephemeral";
  /**
   * Contract event consumer group to use for durable service listeners when an
   * event is declared in more than one group.
   */
  group?: string;
  signal?: AbortSignal;
};

/** Context provided to event listener callbacks. */
export type EventListenerContext = {
  /** Stable event id from the Trellis event header. */
  id: string;
  /** Event creation time from the Trellis event header. */
  time: Date;
  /** NATS subject that delivered the event. */
  subject: string;
  /** Runtime listener mode that delivered the event. */
  mode: "durable" | "ephemeral";
  /** Durable event consumer group, when delivered through a group. */
  group?: string;
  /** JetStream sequence number, when available. */
  sequence?: number;
};

function createEventListenerContext(args: {
  subject: string;
  mode: "durable" | "ephemeral";
  group?: string;
  message: object;
}): EventListenerContext {
  const messageId = readMessageHeader(args.message, "Nats-Msg-Id");
  const messageTime = readMessageHeader(args.message, "Trellis-Event-Time");
  const sequence = Reflect.get(args.message, "seq");

  return {
    id: messageId ?? "",
    time: new Date(messageTime ?? 0),
    subject: args.subject,
    mode: args.mode,
    ...(args.group ? { group: args.group } : {}),
    ...(typeof sequence === "number" ? { sequence } : {}),
  };
}

function readMessageHeader(message: object, key: string): string | undefined {
  const headers = Reflect.get(message, "headers");
  if (typeof headers !== "object" || headers === null) return undefined;
  const get = Reflect.get(headers, "get");
  if (typeof get !== "function") return undefined;
  const value = get.call(headers, key);
  return typeof value === "string" ? value : undefined;
}

type RuntimeEventConsumers = {
  metadata?: RuntimeEventConsumerGroups;
  bindings?: Record<string, EventConsumerResourceBinding>;
};

type RuntimeEventConsumerGroup = {
  uses?: Readonly<Record<string, readonly string[]>>;
  self?: readonly string[];
};

type RuntimeEventConsumerGroups = Readonly<
  Record<string, RuntimeEventConsumerGroup>
>;

function eventConsumerGroupEvents(group: RuntimeEventConsumerGroup): string[] {
  const events = new Set<string>();
  for (const groupEvents of Object.values(group.uses ?? {})) {
    for (const event of groupEvents) events.add(event);
  }
  for (const event of group.self ?? []) events.add(event);
  return [...events].sort();
}

function jetStreamDeliveryProof(message: JsMsg): string {
  const raw = Reflect.get(message, "msg");
  const reply = raw && typeof raw === "object"
    ? Reflect.get(raw, "reply")
    : undefined;
  if (typeof reply !== "string" || !reply.startsWith("$JS.ACK.")) {
    throw new Error("JetStream delivery has no acknowledgement subject");
  }
  return reply;
}

function isConsumerNotFoundError(error: unknown): boolean {
  return error instanceof Error && (
    error.name === "ConsumerNotFoundError" ||
    error.message.includes("consumer not found")
  );
}

type TrellisInternalOpts<TA extends RuntimeApi> = TrellisOpts<TA> & {
  inboxPrefix?: string;
  eventConsumers?: RuntimeEventConsumers;
  apiBindings?: Readonly<Record<string, unknown>>;
};

const internalEventConsumers = Symbol("trellis.internal.eventConsumers");
const internalApiBindings = Symbol("trellis.internal.apiBindings");

type InternalizedTrellisOpts<TA extends RuntimeApi> = TrellisOpts<TA> & {
  [internalEventConsumers]?: RuntimeEventConsumers;
  [internalApiBindings]?: Readonly<Record<string, unknown>>;
};

/**
 * Creates a Trellis runtime with bootstrap-resolved bindings.
 *
 * @internal
 */
export function createTrellisInternal<
  TA extends RuntimeApi = RuntimeApi,
  TMode extends TrellisMode = "client",
  TState extends RuntimeStateStores = RuntimeStateStores,
>(
  name: string,
  nats: NatsConnection,
  auth: TrellisAuth,
  opts?: TrellisInternalOpts<TA>,
): Trellis<TA, TMode, TState> {
  const {
    eventConsumers,
    apiBindings,
    inboxPrefix,
    ...publicOpts
  } = opts ?? {};
  const internalOpts: InternalizedTrellisOpts<TA> = {
    ...publicOpts,
    [internalEventConsumers]: eventConsumers,
    [internalApiBindings]: apiBindings,
  };
  return new Trellis<TA, TMode, TState>(
    name,
    nats,
    auth,
    internalOpts,
    inboxPrefix,
  );
}

type DurableEventRegistration<TA extends RuntimeApi> = {
  event: EventsOf<TA>;
  ctx: EventDescriptorOf<TA, EventsOf<TA>>;
  subject: string;
  fn: EventCallback<EventOf<TA, EventsOf<TA>>>;
};

type DurableEventConsumerLoop<TA extends RuntimeApi> = {
  registrations: Array<DurableEventRegistration<TA>>;
  concurrency: number;
  startedWorkers: Set<number>;
  messages: Set<ConsumerMessages>;
};

type ConsumerReplayEnvelope = {
  deadLetterId: string;
  generation: number;
  resourceId: string;
  originalRecordSequence: number;
  originalSubject: string;
  originalPayloadBytes: number[];
  originalHeaders: Record<string, string[]>;
};

export type FeedSubscribeOpts = {
  signal?: AbortSignal;
};

export type FeedSubscription<TEvent> = AsyncIterable<TEvent>;

export type FeedInputBuilder<TInput, TEvent> = {
  input(input: TInput): {
    subscribe(
      opts?: FeedSubscribeOpts,
    ): AsyncResult<FeedSubscription<TEvent>, BaseError>;
  };
};

export type FeedHandlerContext<TInput, TEvent> = {
  input: TInput;
  caller: SessionCaller;
  signal: AbortSignal;
  emit(
    event: TEvent,
  ): AsyncResult<
    void,
    SchemaValidationError | ValidationError | UnexpectedError
  >;
};

export type FeedRegistration<TInput, TEvent> = {
  handle(
    handler: (
      context: FeedHandlerContext<TInput, TEvent>,
    ) => unknown | Promise<unknown>,
  ): Promise<void>;
};

type SurfaceGroups<TLeaf> = Record<string, Record<string, TLeaf>>;
type RuntimeRpcLeaf = (
  input: unknown,
  opts?: RequestOpts,
) => AsyncResult<unknown, BaseError>;
type RuntimeEventLeaf = {
  prepare(
    event: Record<string, unknown>,
  ): Result<PreparedTrellisEvent, ValidationError | UnexpectedError>;
  publish(
    event: Record<string, unknown>,
  ): AsyncResult<void, ValidationError | UnexpectedError>;
  listen(
    handler: EventCallback<unknown>,
    subjectData?: Record<string, unknown>,
    opts?: EventOpts,
  ): AsyncResult<void, ValidationError | UnexpectedError>;
};
type RuntimeEventPublishLeaf = Omit<RuntimeEventLeaf, "listen">;
type RuntimeFeedLeaf = (
  input: unknown,
  opts?: FeedSubscribeOpts,
) => AsyncResult<FeedSubscription<unknown>, BaseError>;
type RuntimeOperationLeaf = OperationInvoker<RuntimeOperationDesc>;
type PascalSurfaceName<T extends string> = T extends
  `${infer Head}.${infer Tail}`
  ? `${Capitalize<Head>}${PascalSurfaceName<Tail>}`
  : Capitalize<T>;
type LowerCamelSurfaceName<T extends string> = Uncapitalize<
  PascalSurfaceName<T>
>;
type SurfaceGroupName<T extends string> = T extends `${infer Head}.${string}`
  ? LowerCamelSurfaceName<Head>
  : LowerCamelSurfaceName<T>;
type SurfaceLeafName<T extends string> = T extends `${string}.${infer Tail}`
  ? LowerCamelSurfaceName<Tail>
  : LowerCamelSurfaceName<T>;
type SurfaceKeysForGroup<TKeys extends string, TGroup extends string> =
  TKeys extends string ? SurfaceGroupName<TKeys> extends TGroup ? TKeys : never
    : never;

export type ActiveRpcFacade<TA extends RuntimeApi = RuntimeApi> = {
  readonly [TGroup in SurfaceGroupName<MethodsOf<TA>>]: {
    readonly [
      M in SurfaceKeysForGroup<MethodsOf<TA>, TGroup> as SurfaceLeafName<M>
    ]: (
      input: RpcInputOf<TA, M>,
      opts?: RequestOpts,
    ) => AsyncResult<RpcOutputOf<TA, M>, BaseError>;
  };
};

export type ActiveEventFacade<TA extends RuntimeApi = RuntimeApi> = {
  readonly [TGroup in SurfaceGroupName<EventsOf<TA>>]: {
    readonly [
      E in SurfaceKeysForGroup<EventsOf<TA>, TGroup> as SurfaceLeafName<E>
    ]: {
      prepare(
        event: EventPayloadOf<TA, E>,
      ): Result<
        PreparedTrellisEvent<EventPayloadOf<TA, E>>,
        ValidationError | UnexpectedError
      >;
      publish(
        event: EventPayloadOf<TA, E>,
      ): AsyncResult<void, ValidationError | UnexpectedError>;
      listen(
        handler: EventCallback<EventOf<TA, E>>,
        subjectData?: Record<string, unknown>,
        opts?: EventOpts,
      ): AsyncResult<void, ValidationError | UnexpectedError>;
    };
  };
};

export type ActiveEventPublishFacade<TA extends RuntimeApi = RuntimeApi> = {
  readonly [TGroup in SurfaceGroupName<EventsOf<TA>>]: {
    readonly [
      E in SurfaceKeysForGroup<EventsOf<TA>, TGroup> as SurfaceLeafName<E>
    ]: {
      prepare(
        event: EventPayloadOf<TA, E>,
      ): Result<
        PreparedTrellisEvent<EventPayloadOf<TA, E>>,
        ValidationError | UnexpectedError
      >;
      publish(
        event: EventPayloadOf<TA, E>,
      ): AsyncResult<void, ValidationError | UnexpectedError>;
    };
  };
};

export type ActiveFeedFacade<TA extends RuntimeApi = RuntimeApi> = {
  readonly [TGroup in SurfaceGroupName<FeedsOf<TA>>]: {
    readonly [
      F in SurfaceKeysForGroup<FeedsOf<TA>, TGroup> as SurfaceLeafName<F>
    ]: (
      input: FeedInputOf<TA, F>,
      opts?: FeedSubscribeOpts,
    ) => AsyncResult<FeedSubscription<FeedEventOf<TA, F>>, BaseError>;
  };
};

export type ActiveOperationFacade<TA extends RuntimeApi = RuntimeApi> = {
  readonly [TGroup in SurfaceGroupName<OperationsOf<TA>>]: {
    readonly [
      O in SurfaceKeysForGroup<OperationsOf<TA>, TGroup> as SurfaceLeafName<O>
    ]: OperationInvoker<
      TA["operations"][O] & RuntimeOperationDesc
    >;
  };
};

export type ActiveRpcHandleFacade<
  TA extends RuntimeApi = RuntimeApi,
  TRequests = RpcRequestShapes<TA>,
> = {
  readonly [TGroup in SurfaceGroupName<MethodsOf<TA>>]: {
    readonly [
      M in SurfaceKeysForGroup<MethodsOf<TA>, TGroup> as SurfaceLeafName<M>
    ]: (
      handler: HandlerFn<TA, M, TA, HandlerTrellis<TA, TRequests>>,
    ) => Promise<void>;
  };
};

export type FeedSurface<
  TA extends RuntimeApi,
  TMode extends TrellisMode,
  F extends FeedsOf<TA>,
> = TMode extends "service"
  ? FeedRegistration<FeedInputOf<TA, F>, FeedEventOf<TA, F>>
  : FeedInputBuilder<FeedInputOf<TA, F>, FeedEventOf<TA, F>>;

type MaybePromise<T> = T | Promise<T>;

type EventCallback<TMessage> = {
  bivarianceHack(
    message: TMessage,
    context: EventListenerContext,
  ): MaybeAsync<void, BaseError>;
}["bivarianceHack"];

export type RpcHandlerContext = {
  caller: SessionCaller;
  sessionKey: string;
  inboxPrefix: string;
  /** Exact permission verified for this request. */
  permission: DescriptorPermissionAtom;
  /** Capabilities required by this request surface. */
  requiredCapabilities: readonly string[];
  requestId?: string;
  traceId?: string;
  /** Schedules work after the successful RPC response has reached NATS. */
  afterReply(task: () => MaybePromise<void>): void;
};

type ProcessedRpcResponse = {
  readonly payload: string;
  readonly afterReply: readonly (() => MaybePromise<void>)[];
};

export type HandlerTrellis<
  TA extends RuntimeApi,
  TRequests = RpcRequestShapes<TA>,
> = {
  readonly rpc: ActiveRpcFacade<TA>;
  readonly event: ActiveEventPublishFacade<TA>;
  readonly feed: ActiveFeedFacade<TA>;
  readonly operation: ActiveOperationFacade<TA>;
  request<const M extends RequestMethodOf<TRequests>>(
    method: M,
    input: RequestInputOf<TRequests, M>,
    opts?: RequestOpts,
  ): AsyncResult<RequestOutputOf<TRequests, M>, BaseError>;
  publish(
    event: string,
    data: Record<string, unknown>,
  ): AsyncResult<void, ValidationError | UnexpectedError>;
  prepare(
    event: string,
    data: Record<string, unknown>,
  ): Result<PreparedTrellisEvent, ValidationError | UnexpectedError>;
  publishPrepared(
    event: PreparedTrellisEvent,
  ): AsyncResult<void, UnexpectedError>;
  /** Stops durable event listener loops owned by this handler runtime. */
  stopEventListeners(): void;
};

function surfaceGroupName(key: string): string {
  return lowerCamelIdent(key.split(".")[0] ?? key);
}

function surfaceLeafName(key: string): string {
  const parts = key.split(".");
  parts.shift();
  return lowerCamelIdent(parts.length === 0 ? key : parts.join("."));
}

function lowerCamelIdent(value: string): string {
  const pascal = value
    .split(/[^A-Za-z0-9]+/)
    .filter((part) => part.length > 0)
    .map((part) => part[0]!.toUpperCase() + part.slice(1))
    .join("");
  return pascal.length === 0 ? "_" : pascal[0]!.toLowerCase() + pascal.slice(1);
}

function addSurfaceLeaf<TLeaf>(
  surface: SurfaceGroups<TLeaf>,
  key: string,
  leaf: TLeaf,
): void {
  const group = surfaceGroupName(key);
  surface[group] ??= {};
  surface[group][surfaceLeafName(key)] = leaf;
}

function natsSubjectMatches(pattern: string, subject: string): boolean {
  const patternParts = pattern.split(".");
  const subjectParts = subject.split(".");
  for (let index = 0; index < patternParts.length; index += 1) {
    const part = patternParts[index];
    if (part === ">") return true;
    const subjectPart = subjectParts[index];
    if (subjectPart === undefined) return false;
    if (part !== "*" && part !== subjectPart) return false;
  }
  return patternParts.length === subjectParts.length;
}

export type HandlerKvFacade<TKv extends ParticipantKvMetadata> = {
  [K in keyof TKv]: TKv[K]["required"] extends false
    ? TypedKV<TKv[K]["value"]> | undefined
    : TypedKV<TKv[K]["value"]>;
};

export type HandlerStoreHandle = {
  open(): AsyncResult<TypedStore, StoreError>;
  waitFor(
    key: string,
    options?: StoreWaitOptions,
  ): AsyncResult<TypedStoreEntry, StoreError>;
};

export type HandlerJobQueue<
  TPayload,
  TResult,
  TTrellis,
  TUpdate = never,
> = {
  create(
    payload: TPayload,
  ): AsyncResult<JobRef<TPayload, TResult, TUpdate>, BaseError>;
  updates(
    jobId: string,
    options?: JobUpdatesOptions,
  ): AsyncResult<JobUpdateSubscription<TUpdate>, BaseError>;
  handle(
    handler: (args: {
      job: ActiveJob<TPayload, TResult, TUpdate>;
      client: TTrellis;
    }) => Promise<Result<TResult, BaseError>>,
  ): void;
};

export type HandlerJobsFacade<
  TJobs extends Record<string, JobTypeMetadata>,
  TTrellis,
> = {
  [K in keyof TJobs]: HandlerJobQueue<
    TJobs[K]["payload"],
    TJobs[K]["result"],
    TTrellis,
    TJobs[K]["update"]
  >;
};

export type HandlerTrellisForContract<TContract> =
  & HandlerTrellis<TrellisApiFor<TContract>>
  & {
    kv: HandlerKvFacade<ContractKvFor<TContract>>;
    store: Record<string, HandlerStoreHandle>;
    jobs: HandlerJobsFacade<
      ContractJobsFor<TContract>,
      HandlerTrellisForContract<TContract>
    >;
  };

export type HandlerFn<
  TMountApi extends RuntimeApi,
  M extends MethodsOf<TMountApi>,
  TOutboundApi extends RuntimeApi = TMountApi,
  TTrellis = HandlerTrellis<TOutboundApi>,
> = (args: {
  input: MethodInputOf<TMountApi, M>;
  context: RpcHandlerContext;
  client: TTrellis;
}) => MaybePromise<
  Result<MethodOutputOf<TMountApi, M>, HandlerErrorOf<TMountApi, M>>
>;

const STATE_RUNTIME_RPC = {
  Get: {
    subject: "rpc.v1.state.Get",
    input: STATE_API.actions["rpc:Get"].input,
    output: STATE_API.actions["rpc:Get"].output,
    callerCapabilities: [],
    errors: STATE_API.actions["rpc:Get"].errors.map((error) => error.type),
    declaredErrorTypes: STATE_API.actions["rpc:Get"].errors.map((error) =>
      error.type
    ),
    runtimeErrors: STATE_API.actions["rpc:Get"].errors,
  },
  Put: {
    subject: "rpc.v1.state.Put",
    input: STATE_API.actions["rpc:Put"].input,
    output: STATE_API.actions["rpc:Put"].output,
    callerCapabilities: [],
    errors: STATE_API.actions["rpc:Put"].errors.map((error) => error.type),
    declaredErrorTypes: STATE_API.actions["rpc:Put"].errors.map((error) =>
      error.type
    ),
    runtimeErrors: STATE_API.actions["rpc:Put"].errors,
  },
  Delete: {
    subject: "rpc.v1.state.Delete",
    input: STATE_API.actions["rpc:Delete"].input,
    output: STATE_API.actions["rpc:Delete"].output,
    callerCapabilities: [],
    errors: STATE_API.actions["rpc:Delete"].errors.map((error) => error.type),
    declaredErrorTypes: STATE_API.actions["rpc:Delete"].errors.map((error) =>
      error.type
    ),
    runtimeErrors: STATE_API.actions["rpc:Delete"].errors,
  },
};

/** Transport calls consumed by a typed State handle. */
export type StateResourceCalls = Readonly<{
  get(
    input: { resourceName: string },
  ): PromiseLike<Result<StateGetResponse, BaseError>>;
  set(input: {
    resourceName: string;
    representationVersion: bigint;
    value: Uint8Array;
    mode: "create" | "set" | "replace";
    revision?: string;
  }): PromiseLike<Result<StateSetResponse, BaseError>>;
  delete(input: {
    resourceName: string;
    revision?: string;
  }): PromiseLike<Result<StateDeleteResponse, BaseError>>;
}>;

/** Typed handle for one generated single-value State resource. */
export class StateHandle<T> {
  readonly #resourceName: string;
  readonly #definition: KvRepresentation<T>;
  readonly #migrations: ResourceMigrations<T>;
  readonly #isCurrent: () => boolean;
  readonly #isAvailable: () => boolean;
  readonly #calls: StateResourceCalls;

  /** Construct a handle from generated metadata and settled State transport calls. */
  constructor(args: {
    resourceName: string;
    definition: KvRepresentation<T>;
    migrations?: ResourceMigrations<T>;
    isCurrent?: () => boolean;
    isAvailable?: () => boolean;
    calls: StateResourceCalls;
  }) {
    this.#resourceName = args.resourceName;
    this.#definition = args.definition;
    this.#migrations = args.migrations ?? {};
    this.#isCurrent = args.isCurrent ?? (() => true);
    this.#isAvailable = args.isAvailable ?? (() => true);
    this.#calls = args.calls;
  }

  async #project(
    entry: NonNullable<StateGetResponse["entry"]>,
  ): Promise<StateValue<T>> {
    const version = Number(entry.representationVersion);
    const encoded = JSON.parse(new TextDecoder().decode(entry.value));
    let value: T;
    if (version === this.#definition.version) {
      value = this.#definition.codec.decode(encoded);
    } else {
      const historicCodec = this.#definition.migrations[version];
      const migrate = this.#migrations[version];
      if (!historicCodec || !migrate) {
        throw new Error(`Unsupported State representation version ${version}`);
      }
      const migrated = await migrate(historicCodec.decode(encoded));
      value = this.#definition.codec.decode(this.#definition.codec.encode(
        migrated instanceof Result ? migrated.orThrow() : migrated,
      ));
    }
    return {
      value,
      revision: entry.revision,
      createdAt: entry.createdAt,
      updatedAt: entry.updatedAt,
    };
  }

  #assertCurrent(): void {
    if (!this.#isAvailable()) {
      throw new ResourceUnavailableError("State", this.#resourceName);
    }
    if (!this.#isCurrent()) {
      throw new Error(
        `State resource '${this.#resourceName}' has a stale generation`,
      );
    }
  }

  async #conflict(error: StateRpcConflict) {
    return Result.err(
      new StateConflictError(
        error.data.current
          ? await this.#project(error.data.current)
          : undefined,
      ),
    );
  }

  #write(
    value: T,
    mode: "create" | "set" | "replace",
    expectedRevision?: string,
  ): AsyncResult<StateValue<T>, BaseError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const result = await this.#calls.set({
          resourceName: this.#resourceName,
          representationVersion: BigInt(this.#definition.version),
          value: new TextEncoder().encode(
            JSON.stringify(this.#definition.codec.encode(value)),
          ),
          mode,
          ...(expectedRevision === undefined
            ? {}
            : { revision: expectedRevision }),
        });
        if (result.isErr()) {
          return result.error instanceof StateRpcConflict
            ? await this.#conflict(result.error)
            : result;
        }
        return Result.ok(
          await this.#project(
            result.unwrapOrElse(() => {
              throw new Error("State set unexpectedly failed");
            }).entry,
          ),
        );
      } catch (cause) {
        return Result.err(
          cause instanceof BaseError ? cause : new UnexpectedError({ cause }),
        );
      }
    })());
  }

  /** Read the current value, migrating only in memory when required. */
  get(): AsyncResult<StateValue<T> | undefined, BaseError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const result = await this.#calls.get({
          resourceName: this.#resourceName,
        });
        if (result.isErr()) return result;
        const response = result.unwrapOrElse(() => {
          throw new Error("State get unexpectedly failed");
        });
        return Result.ok(
          response.entry ? await this.#project(response.entry) : undefined,
        );
      } catch (cause) {
        return Result.err(
          cause instanceof BaseError ? cause : new UnexpectedError({ cause }),
        );
      }
    })());
  }

  /** Create the value only when no live value exists. */
  create(value: T): AsyncResult<StateValue<T>, BaseError> {
    return this.#write(value, "create");
  }

  /** Unconditionally replace the current value. */
  set(value: T): AsyncResult<StateValue<T>, BaseError> {
    return this.#write(value, "set");
  }

  /** Replace the value only while `revision` remains current. */
  replace(revision: string, value: T): AsyncResult<StateValue<T>, BaseError> {
    return this.#write(value, "replace", revision);
  }

  /** Delete unconditionally or only while `revision` remains current. */
  delete(revision?: string): AsyncResult<boolean, BaseError> {
    return AsyncResult.from((async () => {
      try {
        this.#assertCurrent();
        const result = await this.#calls.delete({
          resourceName: this.#resourceName,
          ...(revision === undefined ? {} : { revision }),
        });
        if (result.isErr()) {
          return result.error instanceof StateRpcConflict
            ? await this.#conflict(result.error)
            : result;
        }
        return Result.ok(
          result.unwrapOrElse(() => {
            throw new Error("State delete unexpectedly failed");
          }).deleted,
        );
      } catch (cause) {
        return Result.err(
          cause instanceof BaseError ? cause : new UnexpectedError({ cause }),
        );
      }
    })());
  }
}

export type RpcRequestErrorOf<
  TA extends RuntimeApi,
  M extends RpcMethodNameOf<TA>,
> = RequestErrorOf<TA, M>;
export type RpcHandlerErrorOf<
  TA extends RuntimeApi,
  M extends RpcMethodNameOf<TA>,
> = HandlerErrorOf<TA, M>;
export type EventName<TContract> = EventsOf<OwnedApiFor<TContract>>;
export type EventType<
  TContract,
  E extends EventName<TContract>,
> = EventOf<OwnedApiFor<TContract>, E>;
export type EventPayload<
  TContract,
  E extends EventName<TContract>,
> = EventPayloadOf<OwnedApiFor<TContract>, E>;

type DeepRecord<T> = {
  [k: string]: T | DeepRecord<T>;
};

const DEFAULT_NO_RESPONDER_MAX_RETRIES = 2;
const DEFAULT_NO_RESPONDER_RETRY_MS = 200;

function activeTraceId(span: Span): string | undefined {
  const traceId = span.spanContext().traceId;
  return traceId === "00000000000000000000000000000000" ? undefined : traceId;
}

function traceIdFromTraceparent(
  traceparent: string | undefined,
): string | undefined {
  const [version, traceId, parentId, flags, extra] = traceparent?.split("-") ??
    [];
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

const EMPTY_TRELLIS_API: RuntimeApi = {
  rpc: {},
  operations: {},
  events: {},
  feeds: {},
  subjects: {},
};

function isBrowserAuthRequiredError(error: unknown): boolean {
  return machineErrorCode(error) === "session_not_found";
}

function isDeclaredRpcError(
  errorNames: readonly string[] | undefined,
  type: string,
): boolean {
  return !!errorNames?.includes(type);
}

function isRuntimeRpcErrorDesc(value: unknown): value is RuntimeRpcErrorDesc {
  return !!value &&
    (typeof value === "object" || typeof value === "function") &&
    typeof Reflect.get(value, "type") === "string" &&
    typeof Reflect.get(value, "fromSerializable") === "function";
}

const payloadSizeEncoder = new TextEncoder();

function payloadByteLength(payload: string | Uint8Array): number {
  return typeof payload === "string"
    ? payloadSizeEncoder.encode(payload).byteLength
    : payload.byteLength;
}

function causeMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function causeLogData(cause: unknown): unknown {
  return cause instanceof Error
    ? { message: cause.message, stack: cause.stack, name: cause.name }
    : cause;
}

function reconstructDeclaredRpcError(
  errorNames: readonly string[] | undefined,
  runtimeErrors: readonly RuntimeRpcErrorDesc[] | undefined,
  data: StaticDecode<typeof TrellisErrorDataSchema>,
  json: JsonValue,
): BaseError | ValidationError | UnexpectedError | null {
  if (!isDeclaredRpcError(errorNames, data.type)) {
    return null;
  }

  const runtimeError = getBuiltinRpcError(data.type) ??
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

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

export class Trellis<
  TA extends RuntimeApi = RuntimeApi,
  TMode extends TrellisMode = "client",
  TState extends RuntimeStateStores = {},
  TRequests = RpcRequestShapes<TA>,
> {
  readonly name: string;
  readonly timeout: number;
  readonly stream: string;
  readonly state: StateFacade<TState>;
  readonly rpc: ActiveRpcFacade<TA>;
  readonly event: ActiveEventFacade<TA>;
  readonly feed: ActiveFeedFacade<TA>;
  readonly operation: ActiveOperationFacade<TA>;
  readonly handle: { readonly rpc: ActiveRpcHandleFacade<TA, TRequests> };
  /** Framework-neutral lifecycle handle for this Trellis runtime connection. */
  readonly connection: TrellisConnection;
  readonly contractId?: string;
  readonly contractDigest?: string;

  #nats: NatsConnection;
  #js: JetStreamClient;
  #auth: TrellisAuth;
  #inboxPrefix: string;
  readonly api: TA;
  #log: LoggerLike;
  #tasks: TrellisTasks;
  #hasExplicitApi: boolean;
  #noResponderMaxRetries: number;
  #noResponderRetryMs: number;
  #onSessionNotFound?: () => MaybePromise<void>;
  #operationStore?: Promise<TypedKV<DurableOperationRecord>>;
  #operationDeploymentId?: string;
  #feedOwnerId: string;
  #eventConsumers: RuntimeEventConsumers;
  #apiBindings: Readonly<Record<string, unknown>>;
  #durableEventLoops = new Map<string, DurableEventConsumerLoop<TA>>();
  #durableEventListenersStopped = false;
  #activeFeeds = new Map<string, {
    controller: AbortController;
    feedId: string;
    principalId: string;
    participantId: string;
  }>();
  #pendingFeedCancels = new Map<string, ReturnType<typeof setTimeout>>();
  #resourceGeneration: () => number;
  #resourceAvailability: (name: string) => boolean;
  #stateMigrations: Readonly<
    Record<string, ResourceMigrations<unknown> | undefined>
  >;

  constructor(
    name: string, // Must be unique for a service
    nats: NatsConnection,
    auth: TrellisAuth,
    opts?: TrellisOpts<TA>,
    inboxPrefix = "_INBOX",
  ) {
    const internalOpts = opts as InternalizedTrellisOpts<TA> | undefined;
    const api = opts?.api;

    this.name = name;
    this.#nats = nats;
    this.#js = jetstream(this.#nats);
    this.#auth = auth as TrellisAuth;
    this.#feedOwnerId = auth.sessionKey;
    this.#inboxPrefix = inboxPrefix;
    this.api = (api ?? EMPTY_TRELLIS_API) as TA;
    this.#log = (opts?.log ?? logger).child({ lib: "trellis" });
    this.timeout = opts?.timeout ?? 3000;
    this.stream = opts?.stream ?? "trellis";
    this.contractId = opts?.contractId;
    this.contractDigest = opts?.contractDigest;
    this.#hasExplicitApi = api !== undefined;
    this.#noResponderMaxRetries = opts?.noResponderRetry?.maxAttempts ??
      DEFAULT_NO_RESPONDER_MAX_RETRIES;
    this.#noResponderRetryMs = opts?.noResponderRetry?.baseDelayMs ??
      DEFAULT_NO_RESPONDER_RETRY_MS;
    this.#onSessionNotFound = opts?.onSessionNotFound;
    this.#resourceGeneration = opts?.resourceGeneration ?? (() => 0);
    this.#resourceAvailability = opts?.resourceAvailability ?? (() => true);
    this.#stateMigrations = opts?.stateMigrations ?? {};
    this.#eventConsumers = internalOpts?.[internalEventConsumers] ?? {};
    this.#apiBindings = internalOpts?.[internalApiBindings] ?? {};
    this.connection = opts?.connection ??
      new TrellisConnection({ kind: "client" });

    this.#tasks = new TrellisTasks({ log: this.#log });
    this.state = this.#createStateFacade(opts?.state as TState | undefined);
    this.rpc = this.#createRpcFacade();
    this.handle = { rpc: this.#createRpcHandleFacade() };
    this.event = this.#createEventFacade();
    this.feed = this.#createFeedFacade();
    this.operation = this.#createOperationFacade();
  }

  protected get nats(): NatsConnection {
    return this.#nats;
  }

  protected get js(): JetStreamClient {
    return this.#js;
  }

  protected get auth(): TrellisAuth {
    return this.#auth;
  }

  #createStateFacade(state: TState | undefined): StateFacade<TState> {
    const stores = (state ?? {}) as RuntimeStateStores;
    const facade: Record<string, ValueStateStoreClient<unknown>> = {};
    const stateRpc = (name: "Get" | "Put" | "Delete") => {
      const descriptor = Object.values(this.api.rpc).find((candidate) =>
        candidate.permission.apiId === "trellis.state" &&
        candidate.permission.apiVersion === "v1" &&
        candidate.permission.surfaceName === name
      );
      if (!descriptor) {
        throw new Error(`Generated participant is missing State.${name}`);
      }
      return descriptor;
    };
    for (const [resourceName, descriptor] of Object.entries(stores)) {
      Object.defineProperty(facade, resourceName, {
        enumerable: true,
        get: () => {
          const generation = this.#resourceGeneration();
          return new StateHandle({
            resourceName,
            definition: descriptor,
            migrations: this.#stateMigrations[resourceName],
            isCurrent: () => generation === this.#resourceGeneration(),
            isAvailable: () => this.#resourceAvailability(resourceName),
            calls: {
              get: (input) =>
                this.#requestBuiltRpc<StateGetResponse>(
                  "State.Get",
                  input,
                  {
                    ...STATE_RUNTIME_RPC.Get,
                    subject: stateRpc("Get").subject,
                  },
                ),
              set: (input) =>
                this.#requestBuiltRpc<StateSetResponse>(
                  "State.Put",
                  {
                    resourceName: input.resourceName,
                    representationVersion: Number(input.representationVersion),
                    value: input.value,
                    mode: input.mode,
                    ...(input.revision === undefined
                      ? {}
                      : { revision: input.revision }),
                  },
                  {
                    ...STATE_RUNTIME_RPC.Put,
                    subject: stateRpc("Put").subject,
                  },
                ),
              delete: (input) =>
                this.#requestBuiltRpc<StateDeleteResponse>(
                  "State.Delete",
                  {
                    resourceName: input.resourceName,
                    ...(input.revision === undefined
                      ? {}
                      : { revision: input.revision }),
                  },
                  {
                    ...STATE_RUNTIME_RPC.Delete,
                    subject: stateRpc("Delete").subject,
                  },
                ),
            },
          });
        },
      });
    }
    return facade as StateFacade<TState>;
  }

  #createRpcFacade(): ActiveRpcFacade<TA> {
    const surface: SurfaceGroups<RuntimeRpcLeaf> = {};
    for (const method of Object.keys(this.api.rpc ?? {})) {
      const leaf: RuntimeRpcLeaf = (input, opts) =>
        this.request(
          method as RequestMethodOf<TRequests>,
          input as RequestInputOf<TRequests, RequestMethodOf<TRequests>>,
          opts,
        );
      addSurfaceLeaf(surface, method, leaf);
    }
    return surface as ActiveRpcFacade<TA>;
  }

  #createRpcHandleFacade(): ActiveRpcHandleFacade<TA, TRequests> {
    const surface: SurfaceGroups<
      (
        handler: HandlerFn<
          TA,
          MethodsOf<TA>,
          TA,
          HandlerTrellis<TA, TRequests>
        >,
      ) => Promise<void>
    > = {};
    for (const method of Object.keys(this.api.rpc ?? {})) {
      addSurfaceLeaf(
        surface,
        method,
        (handler) =>
          this.mount(
            method,
            handler as Parameters<
              Trellis<TA, TMode, TState, TRequests>["mount"]
            >[1],
          ),
      );
    }
    return surface as ActiveRpcHandleFacade<TA, TRequests>;
  }

  #createEventFacade(): ActiveEventFacade<TA> {
    const surface: SurfaceGroups<RuntimeEventLeaf> = {};
    for (const event of Object.keys(this.api.events ?? {})) {
      addSurfaceLeaf(surface, event, {
        prepare: (payload) => this.prepare(event, payload),
        publish: (payload) => this.publish(event, payload),
        listen: (handler, subjectData = {}, opts = {}) =>
          this.listenEvent(event, subjectData, handler, opts),
      });
    }
    return surface as ActiveEventFacade<TA>;
  }

  #createEventPublishFacade(): ActiveEventPublishFacade<TA> {
    const surface: SurfaceGroups<RuntimeEventPublishLeaf> = {};
    for (const event of Object.keys(this.api.events ?? {})) {
      addSurfaceLeaf(surface, event, {
        prepare: (payload) => this.prepare(event, payload),
        publish: (payload) => this.publish(event, payload),
      });
    }
    return surface as ActiveEventPublishFacade<TA>;
  }

  #createFeedFacade(): ActiveFeedFacade<TA> {
    const surface: SurfaceGroups<RuntimeFeedLeaf> = {};
    for (const feed of Object.keys(this.api.feeds ?? {})) {
      const leaf: RuntimeFeedLeaf = (input, opts) =>
        this.feedHandle(feed as FeedsOf<TA>).input(
          input as FeedInputOf<TA, FeedsOf<TA>>,
        ).subscribe(opts) as AsyncResult<
          FeedSubscription<unknown>,
          BaseError
        >;
      addSurfaceLeaf(surface, feed, leaf);
    }
    return surface as ActiveFeedFacade<TA>;
  }

  #createHandlerTrellis(): HandlerTrellis<TA, TRequests> {
    return {
      rpc: this.rpc,
      event: this.#createEventPublishFacade(),
      feed: this.feed,
      operation: this.operation,
      request: this.request.bind(this),
      prepare: (event, data) => this.prepare(event, data),
      publish: (event, data) => this.publish(event, data),
      publishPrepared: (event) => this.publishPrepared(event),
      stopEventListeners: () => this.stopEventListeners(),
    };
  }

  #createOperationFacade(): ActiveOperationFacade<TA> {
    const surface: SurfaceGroups<RuntimeOperationLeaf> = {};
    for (const operation of Object.keys(this.api.operations ?? {})) {
      addSurfaceLeaf(
        surface,
        operation,
        this.operationHandle(operation) as OperationInvoker<
          RuntimeOperationDesc
        >,
      );
    }
    return surface as ActiveOperationFacade<TA>;
  }

  #unknownApiError(
    kind: "RPC method" | "operation" | "event" | "feed",
    name: string,
  ): Error {
    const base = `Unknown ${kind} '${name}'.`;
    if (this.#hasExplicitApi) {
      return new Error(`${base} Did you forget to include its API module?`);
    }
    return new Error(
      `${base} No API surface was provided to the private transport session.`,
    );
  }

  async operationStoreHandle(): Promise<
    TypedKV<DurableOperationRecord>
  > {
    if (!this.#operationStore) {
      if (!this.#operationDeploymentId) {
        throw new Error(
          "Service operation runtime requires a deployment operation store id",
        );
      }
      const bucket = `trellis_operations_${this.#operationDeploymentId}`;
      const operationStore = (async (): Promise<
        TypedKV<DurableOperationRecord>
      > => {
        const result = await TypedKV.open<DurableOperationRecord>(
          this.#nats,
          bucket,
          {
            version: 1,
            migrations: {},
            codec: {
              encode: (value: DurableOperationRecord) => value,
              decode: (value) => value as DurableOperationRecord,
            },
          },
          {
            bindOnly: true,
            ttl: 30 * 24 * 60 * 60 * 1_000,
            maxValueBytes: 1024 * 1024,
          },
        );
        const value = result.take();
        if (isErr(value)) {
          throw value.error;
        }
        return value;
      })();
      this.#operationStore = operationStore;
    }
    return this.#operationStore!;
  }

  protected setOperationDeploymentId(id: string): void {
    this.#operationDeploymentId = id;
  }

  protected setFeedOwnerId(id: string): void {
    this.#feedOwnerId = id;
  }

  async loadOperationRecord(
    operationId: string,
  ): Promise<DurableOperationRecord | null> {
    const store = await this.operationStoreHandle();
    const entry = await store.getEntry(operationId);
    const value = entry.take();
    if (isErr(value)) {
      throw value.error;
    }
    if (!value) return null;
    return value.value ?? null;
  }

  protected async listNonterminalOperationRecords(): Promise<
    DurableOperationRecord[]
  > {
    const store = await this.operationStoreHandle();
    const keys = await store.keys();
    const keyValue = keys.take();
    if (isErr(keyValue)) throw keyValue.error;
    const records: DurableOperationRecord[] = [];
    for await (const key of keyValue) {
      const record = await this.loadOperationRecord(key);
      if (
        record && record.snapshot.state !== "completed" &&
        record.snapshot.state !== "failed" &&
        record.snapshot.state !== "cancelled"
      ) records.push(record);
    }
    return records;
  }

  async saveOperationRecord(runtime: RuntimeOperationRecord): Promise<void> {
    const store = await this.operationStoreHandle();
    const loaded = await store.getEntry(runtime.id);
    const loadedValue = loaded.take();
    if (isErr(loadedValue)) throw loadedValue.error;
    const existing = loadedValue?.value;
    if (existing && existing.revision !== runtime.revision) {
      throw new Error("operation revision conflict");
    }
    const revision = existing ? runtime.revision + 1 : runtime.revision;
    const record: DurableOperationRecord = {
      invocationId: runtime.id,
      callerSessionKey: runtime.callerSessionKey,
      invocationDigest: runtime.invocationDigest,
      caller: runtime.caller,
      creatorPrincipalId: runtime.creatorPrincipalId,
      creatorParticipantId: runtime.creatorParticipantId,
      apiId: runtime.apiId,
      operation: runtime.operation,
      input: runtime.input,
      revision,
      ownerInstanceId: runtime.ownerInstanceId,
      ownerConnectionId: runtime.ownerConnectionId,
      ownerEpoch: runtime.ownerEpoch,
      leaseExpiresAt: runtime.leaseExpiresAt,
      ...(runtime.cancelRequestedAt
        ? { cancelRequestedAt: runtime.cancelRequestedAt }
        : {}),
      ...(runtime.transferGrant
        ? { transferGrant: runtime.transferGrant }
        : {}),
      sequence: runtime.sequence,
      signalSequence: runtime.signalSequence,
      signals: runtime.signals,
      snapshot: runtime.snapshot,
    };
    const byteLength = (value: unknown) =>
      new TextEncoder().encode(JSON.stringify(value)).byteLength;
    if (byteLength(record.input) > MAX_OPERATION_INPUT_BYTES) {
      throw new Error("operation input exceeds 256 KiB");
    }
    if (
      record.snapshot.progress !== undefined &&
      byteLength(record.snapshot.progress) > MAX_OPERATION_PROGRESS_BYTES
    ) throw new Error("operation progress exceeds 64 KiB");
    if (
      record.snapshot.output !== undefined &&
      byteLength(record.snapshot.output) > MAX_OPERATION_OUTPUT_BYTES
    ) throw new Error("operation output exceeds 512 KiB");
    if (
      record.snapshot.error !== undefined &&
      byteLength(record.snapshot.error) > MAX_OPERATION_ERROR_BYTES
    ) throw new Error("operation error exceeds 32 KiB");
    if ((record.signals?.length ?? 0) > MAX_OPERATION_SIGNALS) {
      throw new Error("operation signal limit exceeded");
    }
    for (const signal of record.signals) {
      if (byteLength(signal.input ?? null) > MAX_OPERATION_SIGNAL_BYTES) {
        throw new Error("operation signal payload exceeds 64 KiB");
      }
    }
    if (byteLength(record) > MAX_OPERATION_RECORD_BYTES) {
      throw new Error("operation record exceeds 1 MiB");
    }
    const saved = loadedValue === undefined
      ? await store.create(runtime.id, record)
      : await store.replace(runtime.id, loadedValue.revision, record);
    const value = saved.take();
    if (isErr(value)) throw value.error;
    runtime.revision = revision;
  }

  /**
   * Makes an authenticated request to a Trellis RPC method.
   *
   * @template M The specific RPC method being called.
   * @param method The name of the RPC method to call.
   * @param input The input data for the method, conforming to its schema.
   * @param opts Optional request-specific options.
   * @returns An `AsyncResult` containing either the method's output or an error.
   * @returns A `Result` object after awaiting:
   *              ok: A validated response for method M
   *              err: declared RPC errors | RemoteError | ValidationError | UnexpectedError
   */
  request<const M extends RequestMethodOf<TRequests>>(
    method: M,
    input: RequestInputOf<TRequests, M>,
    opts?: RequestOpts,
  ): AsyncResult<RequestOutputOf<TRequests, M>, BaseError>;
  request(
    method: string,
    input: unknown,
    opts?: RequestOpts,
  ): AsyncResult<unknown, BaseError> {
    const rpcApi = this.api["rpc"] as Record<string, unknown>;
    const ctx = rpcApi[method] as {
      subject: string;
      input: unknown;
      output: unknown;
      callerCapabilities: readonly string[];
      errors?: readonly string[];
      declaredErrorTypes?: readonly string[];
      runtimeErrors?: readonly RuntimeRpcErrorDesc[];
    } | undefined;
    if (!ctx) {
      return AsyncResult.from(Promise.resolve(err(
        new UnexpectedError({
          cause: this.#unknownApiError("RPC method", method.toString()),
          context: { method: method.toString() },
        }),
      )));
    }

    return this.#requestBuiltRpcUnknown(method, input, ctx, opts);
  }

  #requestBuiltRpcUnknown(
    method: string,
    input: unknown,
    ctx: {
      subject: string;
      input: unknown;
      output: unknown;
      callerCapabilities: readonly string[];
      errors?: readonly string[];
      declaredErrorTypes?: readonly string[];
      runtimeErrors?: readonly RuntimeRpcErrorDesc[];
    },
    opts?: RequestOpts,
  ): AsyncResult<unknown, BaseError> {
    return this.#requestBuiltRpc(method, input, ctx, opts);
  }

  #requestBuiltRpc<TOutput>(
    method: string,
    input: unknown,
    ctx: {
      subject: string;
      input: unknown;
      output: unknown;
      callerCapabilities: readonly string[];
      errors?: readonly string[];
      declaredErrorTypes?: readonly string[];
      runtimeErrors?: readonly RuntimeRpcErrorDesc[];
    },
    opts?: RequestOpts,
  ): AsyncResult<TOutput, BaseError> {
    return AsyncResult.from((async () => {
      this.#log.trace(
        { method: String(method) },
        `Calling ${method.toString()}.`,
      );

      const msg = encodeRuntimeSchema(ctx.input, input).take();
      if (isErr(msg)) {
        recordRuntimeError(msg.error, {
          surface: "rpc",
          direction: "client",
          operation: method,
          phase: "request_encoding",
        });
        return msg;
      }

      const subject = this.template(ctx.subject, input).take();
      if (isErr(subject)) {
        recordRuntimeError(subject.error, {
          surface: "rpc",
          direction: "client",
          operation: method,
          phase: "request_encoding",
        });
        return subject;
      }
      const span = startClientSpan(method, subject);
      const attempt = async (): Promise<Result<TOutput, BaseError>> => {
        const msgResult = await this.#requestMessageWithRetry({
          method,
          subject,
          payload: msg,
          timeout: opts?.timeout ?? this.timeout,
          signal: opts?.signal,
          callerCapabilities: ctx.callerCapabilities,
          span,
        });
        const response = msgResult.take();
        if (isErr(response)) {
          recordRuntimeError(response.error, {
            surface: "rpc",
            direction: "client",
            operation: method,
            phase: "request_send",
          });
          return response;
        }

        if (response.headers?.get("status") === "error") {
          const json = safeJson(response).take();
          if (isErr(json)) {
            const error = requestFailedTransportError({
              code: "trellis.request.invalid_response",
              message: "Trellis returned an invalid response.",
              hint:
                "Retry the request. If it keeps happening, check the Trellis capability handling this request.",
              method,
              subject,
              cause: json.error.cause,
            });
            recordRuntimeError(error, {
              surface: "rpc",
              direction: "client",
              operation: method,
              phase: "response_decoding",
            });
            return err(error);
          }

          const errorData = parse(TrellisErrorDataSchema, json).take();
          if (isErr(errorData)) {
            const error = requestFailedTransportError({
              code: "trellis.request.invalid_response",
              message: "Trellis returned an invalid response.",
              hint:
                "Retry the request. If it keeps happening, check the Trellis capability handling this request.",
              method,
              subject,
              cause: errorData.error,
            });
            recordRuntimeError(error, {
              surface: "rpc",
              direction: "client",
              operation: method,
              phase: "response_decoding",
            });
            return err(error);
          }

          const declaredErrorTypes = Array.isArray(ctx.declaredErrorTypes)
            ? ctx.declaredErrorTypes.filter((value): value is string =>
              typeof value === "string"
            )
            : ctx.errors;
          const runtimeErrors = Array.isArray(ctx.runtimeErrors)
            ? ctx.runtimeErrors.filter(isRuntimeRpcErrorDesc)
            : undefined;
          const reconstructed = reconstructDeclaredRpcError(
            declaredErrorTypes,
            runtimeErrors,
            errorData,
            json,
          );
          if (reconstructed) {
            await this.#handleBrowserAuthRequired(reconstructed);
            recordRuntimeError(new RemoteError({ error: errorData }), {
              surface: "rpc",
              direction: "client",
              operation: method,
              phase: "remote_error",
            });
            return err(reconstructed);
          }

          const remoteError = new RemoteError({ error: errorData });
          await this.#handleBrowserAuthRequired(remoteError);
          recordRuntimeError(remoteError, {
            surface: "rpc",
            direction: "client",
            operation: method,
            phase: "remote_error",
          });
          return err(remoteError);
        }

        const json = safeJson(response).take();
        if (isErr(json)) {
          const error = requestFailedTransportError({
            code: "trellis.request.invalid_response",
            message: "Trellis returned an invalid response.",
            hint:
              "Retry the request. If it keeps happening, check the Trellis capability handling this request.",
            method,
            subject,
            cause: json.error.cause,
          });
          recordRuntimeError(error, {
            surface: "rpc",
            direction: "client",
            operation: method,
            phase: "response_decoding",
          });
          return err(error);
        }

        const outputResult = parseRuntimeSchema(ctx.output, json).take();
        if (isErr(outputResult)) {
          recordRuntimeError(outputResult.error, {
            surface: "rpc",
            direction: "client",
            operation: method,
            phase: "response_decoding",
          });
          return err(outputResult.error);
        }

        return ok(outputResult as TOutput);
      };

      return await withSpanAsync(span, async () => {
        try {
          const result = await attempt();
          const value = result.take();
          if (isErr(value)) {
            span.setStatus({
              code: SpanStatusCode.ERROR,
              message: value.error.message,
            });
          } else {
            span.setStatus({ code: SpanStatusCode.OK });
          }
          return result;
        } catch (cause) {
          const unexpected = cause instanceof TransportError
            ? cause
            : new UnexpectedError({ cause });
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: unexpected.message,
          });
          span.recordException(unexpected);
          recordRuntimeError(unexpected, {
            surface: "rpc",
            direction: "client",
            operation: method,
            phase: "unexpected",
          });
          return err(unexpected);
        } finally {
          span.end();
        }
      });
    })());
  }

  async #handleBrowserAuthRequired(error: unknown): Promise<void> {
    if (
      !this.#onSessionNotFound || !isBrowserAuthRequiredError(error)
    ) {
      return;
    }

    await this.#onSessionNotFound();
  }

  async #authenticateFeedRequest(args: {
    msg: Msg;
    permission: DescriptorPermissionAtom | undefined;
    requiredCapabilities: readonly string[];
  }): Promise<Result<SessionCaller, BaseError>> {
    return await verifyLocalAuthorization({
      kind: "request",
      cache: this.#auth.authorizationProviderCache,
      message: args.msg,
      permission: args.permission,
      requiredCapabilities: args.requiredCapabilities,
    });
  }

  feedHandle<F extends FeedsOf<TA>>(
    feed: F,
  ):
    & FeedInputBuilder<FeedInputOf<TA, F>, FeedEventOf<TA, F>>
    & FeedRegistration<FeedInputOf<TA, F>, FeedEventOf<TA, F>> {
    const descriptor = this.api.feeds?.[feed] as
      | FeedDescriptorOf<TA, F>
      | undefined;
    if (!descriptor) {
      throw this.#unknownApiError("feed", feed.toString());
    }

    return {
      input: (input: FeedInputOf<TA, F>) => ({
        subscribe: (opts?: FeedSubscribeOpts) =>
          this.#subscribeFeed(
            feed.toString(),
            descriptor,
            input,
            opts,
          ) as AsyncResult<
            FeedSubscription<FeedEventOf<TA, F>>,
            BaseError
          >,
      }),
      handle: (
        handler: (
          context: FeedHandlerContext<FeedInputOf<TA, F>, FeedEventOf<TA, F>>,
        ) => unknown | Promise<unknown>,
      ) => this.#handleFeed(feed.toString(), descriptor, handler),
    };
  }

  #subscribeFeed<TInput, TEvent>(
    feed: string,
    descriptor: FeedDesc,
    input: TInput,
    opts?: FeedSubscribeOpts,
  ): AsyncResult<FeedSubscription<TEvent>, BaseError> {
    return AsyncResult.from((async () => {
      const payload = encodeRuntimeSchema(descriptor.input, input).take();
      if (isErr(payload)) {
        recordRuntimeError(payload.error, {
          surface: "feed",
          direction: "client",
          operation: feed,
          phase: "request_encoding",
        });
        return payload;
      }

      const subject = this.template(
        descriptor.subject,
        input as Record<string, unknown>,
      ).take();
      if (isErr(subject)) {
        recordRuntimeError(subject.error, {
          surface: "feed",
          direction: "client",
          operation: feed,
          phase: "request_template",
        });
        return subject;
      }

      const inbox = createInbox(this.#inboxPrefix);
      const authHeaders = await this.createRequestProof(
        subject,
        payload,
        inbox,
      );
      if (opts?.signal?.aborted) {
        const error = createTransportError({
          code: "trellis.feed.subscribe_aborted",
          message:
            "The feed subscription was aborted before Trellis acknowledged it.",
          hint: "Retry the subscription if the feed is still needed.",
          context: { feed, subject },
        });
        recordRuntimeError(error, {
          surface: "feed",
          direction: "client",
          operation: feed,
          phase: "handshake",
        });
        return err(error);
      }
      const headers = natsHeaders();
      headers.set("proof", authHeaders.proof);
      headers.set("iat", String(authHeaders.iat));
      headers.set("request-id", authHeaders.requestId);
      headers.set("authorization-context", authHeaders.contextDigest);
      headers.set("session-key", this.#auth.sessionKey);
      injectTraceContext(createNatsHeaderCarrier(headers));

      const sub = this.#nats.subscribe(inbox);
      const iterator = sub[Symbol.asyncIterator]();
      let feedId: string | undefined;
      let cancelRequested = false;
      let cancelSent = false;
      let controlSubject: string | undefined;
      const cancel = async () => {
        cancelRequested = true;
        if (
          cancelSent || controlSubject === undefined || feedId === undefined
        ) return;
        cancelSent = true;
        try {
          const cancelPayload = JSON.stringify({
            _trellisFeedCancel: inbox,
            feedId,
          });
          const auth = await this.createRequestProof(
            controlSubject,
            cancelPayload,
            inbox,
          );
          const cancelHeaders = natsHeaders();
          cancelHeaders.set("proof", auth.proof);
          cancelHeaders.set("iat", String(auth.iat));
          cancelHeaders.set("request-id", auth.requestId);
          cancelHeaders.set("authorization-context", auth.contextDigest);
          cancelHeaders.set("session-key", this.#auth.sessionKey);
          injectTraceContext(createNatsHeaderCarrier(cancelHeaders));
          this.#nats.publish(controlSubject, cancelPayload, {
            headers: cancelHeaders,
            reply: inbox,
          });
        } catch {
          // Best effort: the transport may already be closing with the feed.
        }
      };
      const abort = () => {
        sub.unsubscribe();
        void cancel();
      };
      opts?.signal?.addEventListener("abort", abort, { once: true });

      try {
        this.#nats.publish(subject, payload, { headers, reply: inbox });
      } catch (cause) {
        opts?.signal?.removeEventListener("abort", abort);
        sub.unsubscribe();
        await cancel();
        const error = createTransportError({
          code: "trellis.feed.subscribe_failed",
          message: "Trellis could not subscribe to the feed.",
          hint:
            "Retry the subscription. If it keeps failing, check Trellis runtime health.",
          cause,
          context: { feed, subject },
        });
        recordRuntimeError(error, {
          surface: "feed",
          direction: "client",
          operation: feed,
          phase: "request_send",
        });
        return err(error);
      }

      let timeoutId: ReturnType<typeof setTimeout> | undefined;
      let abortHandler: (() => void) | undefined;
      const handshakePromises: Array<
        Promise<IteratorResult<Msg> | "aborted" | "timeout">
      > = [
        iterator.next(),
        new Promise<"timeout">((resolve) => {
          timeoutId = setTimeout(() => resolve("timeout"), this.timeout);
        }),
      ];
      const signal = opts?.signal;
      if (signal) {
        handshakePromises.push(
          new Promise<"aborted">((resolve) => {
            abortHandler = () => resolve("aborted");
            signal.addEventListener("abort", abortHandler, { once: true });
          }),
        );
      }

      const firstFrame = await Promise.race(handshakePromises);
      if (timeoutId !== undefined) clearTimeout(timeoutId);
      if (signal && abortHandler) {
        signal.removeEventListener("abort", abortHandler);
      }
      if (firstFrame === "timeout" || firstFrame === "aborted") {
        opts?.signal?.removeEventListener("abort", abort);
        sub.unsubscribe();
        await cancel();
        const error = createTransportError({
          code: firstFrame === "timeout"
            ? "trellis.feed.subscribe_timeout"
            : "trellis.feed.subscribe_aborted",
          message: firstFrame === "timeout"
            ? "Trellis did not receive a feed acknowledgement."
            : "The feed subscription was aborted before Trellis acknowledged it.",
          hint: firstFrame === "timeout"
            ? "Check that the target service is running and has the current deployment digest, then retry."
            : "Retry the subscription if the feed is still needed.",
          context: { feed, subject },
        });
        recordRuntimeError(error, {
          surface: "feed",
          direction: "client",
          operation: feed,
          phase: "handshake",
        });
        return err(error);
      }
      if (firstFrame.done) {
        opts?.signal?.removeEventListener("abort", abort);
        sub.unsubscribe();
        await cancel();
        const error = createTransportError({
          code: "trellis.feed.subscribe_closed",
          message: "Trellis closed the feed before acknowledging it.",
          hint:
            "Retry the subscription. If it keeps failing, check Trellis runtime health.",
          context: { feed, subject },
        });
        recordRuntimeError(error, {
          surface: "feed",
          direction: "client",
          operation: feed,
          phase: "handshake",
        });
        return err(error);
      }
      const firstMessage = firstFrame.value;
      controlSubject = firstMessage.headers?.get("feed-control-subject");
      feedId = firstMessage.headers?.get("feed-id");
      if (cancelRequested) await cancel();
      if (firstMessage.headers?.get("status") === "error") {
        opts?.signal?.removeEventListener("abort", abort);
        sub.unsubscribe();
        await cancel();
        const error = createTransportError({
          code: "trellis.feed.failed",
          message: "Trellis rejected the feed subscription.",
          hint:
            "Retry the subscription. If it keeps failing, check Trellis runtime health and permissions.",
          context: { feed, subject, frame: firstMessage.string() },
        });
        recordRuntimeError(error, {
          surface: "feed",
          direction: "client",
          operation: feed,
          phase: "remote_error",
        });
        return err(error);
      }
      const firstEvent = firstMessage.headers?.get("feed-status") === "ready"
        ? undefined
        : firstMessage;

      const eventSchema = descriptor.event;
      return ok((async function* () {
        try {
          const parseFeedFrame = (msg: Msg): TEvent => {
            if (msg.headers?.get("status") === "error") {
              const error = createTransportError({
                code: "trellis.feed.failed",
                message: "Trellis stopped the feed.",
                hint:
                  "Retry the subscription. If it keeps failing, check Trellis runtime health.",
                context: { feed, subject, frame: msg.string() },
              });
              recordRuntimeError(error, {
                surface: "feed",
                direction: "client",
                operation: feed,
                phase: "remote_error",
              });
              throw error;
            }
            const json = safeJson(msg).take();
            if (isErr(json)) {
              recordRuntimeError(json.error, {
                surface: "feed",
                direction: "client",
                operation: feed,
                phase: "event_decoding",
              });
              throw json.error;
            }
            const parsed = parseRuntimeSchema(eventSchema, json).take();
            if (isErr(parsed)) {
              recordRuntimeError(parsed.error, {
                surface: "feed",
                direction: "client",
                operation: feed,
                phase: "event_validation",
              });
              throw parsed.error;
            }
            return parsed as TEvent;
          };
          if (firstEvent) yield parseFeedFrame(firstEvent);
          while (true) {
            const next = await iterator.next();
            if (next.done) break;
            yield parseFeedFrame(next.value);
          }
        } finally {
          opts?.signal?.removeEventListener("abort", abort);
          sub.unsubscribe();
          await cancel();
        }
      })());
    })());
  }

  async #handleFeed<TInput, TEvent>(
    feed: string,
    descriptor: FeedDesc,
    handler: (
      context: FeedHandlerContext<TInput, TEvent>,
    ) => unknown | Promise<unknown>,
  ): Promise<void> {
    const subject = this.template(descriptor.subject, {}, true).take();
    if (isErr(subject)) throw subject.error;
    let sub: ReturnType<NatsConnection["subscribe"]>;
    let controlSub: ReturnType<NatsConnection["subscribe"]>;
    const controlSubject = feedControlSubject(subject, this.#feedOwnerId);
    try {
      sub = this.#nats.subscribe(subject, { queue: routeQueueGroup(subject) });
      controlSub = this.#nats.subscribe(controlSubject);
    } catch (cause) {
      const error = createTransportError({
        code: "trellis.feed.listen_failed",
        message: "Trellis could not listen for feed requests.",
        hint:
          "Check the service deployment digest and runtime permissions, then restart the service.",
        cause,
        context: { feed, subject },
      });
      recordRuntimeError(error, {
        surface: "feed",
        direction: "server",
        operation: feed,
        phase: "listen",
      });
      throw error;
    }
    const task = AsyncResult.try(async () => {
      for await (const msg of sub) {
        void (async () => {
          try {
            const result = await this.#processFeedMessage(
              feed,
              descriptor,
              msg,
              handler,
            );
            const value = result.take();
            if (isErr(value)) {
              this.#respondWithError(msg, value.error);
            }
          } catch (cause) {
            const error = annotateHandlerBoundaryError(cause, {
              feed,
              requestId: msg.headers?.get("request-id"),
              service: this.name,
              contractId: this.contractId,
              contractDigest: this.contractDigest,
              traceId: traceIdFromTraceparent(msg.headers?.get("traceparent")),
            });
            recordRuntimeError(error, {
              surface: "feed",
              direction: "server",
              operation: feed,
              phase: "handler_throw",
            });
            this.#respondWithError(msg, error);
          }
        })();
      }
    });
    this.#tasks.add(`feed:${feed}`, task);
    this.#tasks.add(
      `feed:${feed}:control`,
      AsyncResult.try(async () => {
        for await (const msg of controlSub) {
          const result = await this.#processFeedMessage(
            feed,
            descriptor,
            msg,
            handler,
          );
          const value = result.take();
          if (isErr(value)) this.#respondWithError(msg, value.error);
        }
      }),
    );
  }

  async #processFeedMessage<TInput, TEvent>(
    feed: string,
    descriptor: FeedDesc,
    msg: Msg,
    handler: (
      context: FeedHandlerContext<TInput, TEvent>,
    ) => unknown | Promise<unknown>,
  ): Promise<Result<void, BaseError>> {
    const json = safeJson(msg).take();
    if (isErr(json)) {
      recordRuntimeError(json.error, {
        surface: "feed",
        direction: "server",
        operation: feed,
        phase: "request_decoding",
      });
      return json;
    }
    const caller = await this.#authenticateFeedRequest({
      msg,
      permission: descriptor.permission,
      requiredCapabilities: descriptor.subscribeCapabilities,
    });
    const callerValue = caller.take();
    if (isErr(callerValue)) {
      recordRuntimeError(callerValue.error, {
        surface: "feed",
        direction: "server",
        operation: feed,
        phase: "auth",
      });
      return callerValue;
    }
    const cancelReply = json && typeof json === "object" &&
        !Array.isArray(json) && Object.keys(json).length === 2 &&
        typeof (json as Record<string, unknown>)._trellisFeedCancel ===
          "string" &&
        typeof (json as Record<string, unknown>).feedId === "string"
      ? (json as Record<string, string>)._trellisFeedCancel
      : undefined;
    if (cancelReply !== undefined) {
      if (cancelReply !== msg.reply) {
        return err(
          new AuthError({
            reason: "reply_subject_mismatch",
            context: { expected: msg.reply, actual: cancelReply },
          }),
        );
      }
      const active = this.#activeFeeds.get(cancelReply);
      const controlFeedId = (json as Record<string, string>).feedId;
      if (
        !active || active.feedId !== controlFeedId ||
        msg.subject !==
          feedControlSubject(
            descriptor.subject,
            this.#feedOwnerId,
            controlFeedId,
          ) ||
        callerValue.type !== "verified" ||
        active.principalId !== callerValue.principalId ||
        active.participantId !== callerValue.participantId
      ) {
        return err(
          new AuthError({
            reason: "feed_control_denied",
            context: { feed, feedId: controlFeedId },
          }),
        );
      }
      active.controller.abort();
      if (this.#activeFeeds.delete(cancelReply)) return ok(undefined);
      const previous = this.#pendingFeedCancels.get(cancelReply);
      if (previous !== undefined) clearTimeout(previous);
      if (
        previous !== undefined ||
        this.#pendingFeedCancels.size < MAX_FEED_CANCEL_TOMBSTONES
      ) {
        const timeout = setTimeout(() => {
          this.#pendingFeedCancels.delete(cancelReply);
        }, FEED_CANCEL_TOMBSTONE_MS);
        this.#pendingFeedCancels.set(cancelReply, timeout);
      }
      return ok(undefined);
    }
    const parsed = parseRuntimeSchema(descriptor.input, json).take();
    if (isErr(parsed)) {
      recordRuntimeError(parsed.error, {
        surface: "feed",
        direction: "server",
        operation: feed,
        phase: "input_validation",
      });
      return parsed;
    }
    if (!msg.reply) {
      const error = new UnexpectedError({
        context: { feed, reason: "missing_reply" },
      });
      recordRuntimeError(error, {
        surface: "feed",
        direction: "server",
        operation: feed,
        phase: "handshake",
      });
      return err(error);
    }
    const pendingCancel = this.#pendingFeedCancels.get(msg.reply);
    if (pendingCancel !== undefined) {
      clearTimeout(pendingCancel);
      this.#pendingFeedCancels.delete(msg.reply);
      return ok(undefined);
    }
    const controller = new AbortController();
    if (callerValue.type !== "verified") {
      return err(new AuthError({ reason: "feed_creator_identity_required" }));
    }
    const feedId = crypto.randomUUID();
    this.#activeFeeds.get(msg.reply)?.controller.abort();
    this.#activeFeeds.set(msg.reply, {
      controller,
      feedId,
      principalId: callerValue.principalId,
      participantId: callerValue.participantId,
    });
    try {
      const readyHeaders = natsHeaders();
      readyHeaders.set("feed-status", "ready");
      readyHeaders.set("feed-id", feedId);
      readyHeaders.set(
        "feed-control-subject",
        feedControlSubject(descriptor.subject, this.#feedOwnerId, feedId),
      );
      this.#nats.publish(msg.reply, new Uint8Array(), {
        headers: readyHeaders,
      });
      if (controller.signal.aborted) return ok(undefined);

      const handlerResult = await handler({
        input: parsed as TInput,
        caller: callerValue,
        signal: controller.signal,
        emit: (event: TEvent) =>
          AsyncResult.from((async () => {
            const payload = encodeRuntimeSchema(descriptor.event, event).take();
            if (isErr(payload)) {
              recordRuntimeError(payload.error, {
                surface: "feed",
                direction: "server",
                operation: feed,
                phase: "event_encoding",
              });
              return payload;
            }
            if (!msg.reply) {
              const error = new UnexpectedError({
                context: { feed, reason: "missing_reply" },
              });
              recordRuntimeError(error, {
                surface: "feed",
                direction: "server",
                operation: feed,
                phase: "event_publish",
              });
              return err(error);
            }
            try {
              this.#nats.publish(msg.reply, payload);
            } catch (cause) {
              const error = new UnexpectedError({
                cause,
                context: { feed },
              });
              recordRuntimeError(error, {
                surface: "feed",
                direction: "server",
                operation: feed,
                phase: "event_publish",
              });
              return err(error);
            }
            return ok(undefined);
          })()),
      });
      const handlerOutcome = isResultLike(handlerResult)
        ? handlerResult.take()
        : handlerResult;
      if (isErr(handlerOutcome)) {
        const error = annotateHandlerBoundaryError(handlerOutcome.error, {
          feed,
          requestId: msg.headers?.get("request-id"),
          service: this.name,
          contractId: this.contractId,
          contractDigest: this.contractDigest,
          traceId: traceIdFromTraceparent(msg.headers?.get("traceparent")),
        });
        recordRuntimeError(error, {
          surface: "feed",
          direction: "server",
          operation: feed,
          phase: "handler_result",
        });
        return err(error);
      }
      return ok(undefined);
    } finally {
      if (this.#activeFeeds.get(msg.reply)?.controller === controller) {
        this.#activeFeeds.delete(msg.reply);
      }
      controller.abort();
    }
  }

  operationHandle<O extends OperationsOf<TA>>(
    operation: O,
    unavailable?: () => TransportError | undefined,
  ): OperationSurface<TA, TMode, O> {
    const descriptor = this.api["operations"]?.[operation];
    if (!descriptor) {
      throw this.#unknownApiError("operation", operation.toString());
    }

    const transport: OperationTransport = {
      requestJson: (subject, body) => {
        const error = unavailable?.();
        return error
          ? AsyncResult.from(Promise.resolve(err(error)))
          : this.#requestJson(subject, body as JsonValue);
      },
      watchJson: (subject, body) => {
        const error = unavailable?.();
        return error
          ? AsyncResult.from(Promise.resolve(err(error)))
          : this.#watchJson(subject, body as JsonValue);
      },
      putTransfer: (
        grant: SendTransferGrant,
        body: TransferBody,
      ): AsyncResult<FileInfo, TransferError> =>
        AsyncResult.from((async () => {
          const handle = createTransferHandle(
            this.#nats,
            this.#auth,
            this.timeout,
            grant,
            this.#inboxPrefix,
          );
          if (!(handle instanceof Object) || !("send" in handle)) {
            return err(
              new TransferError({
                operation: "transfer",
                context: { reason: "invalid_operation_transfer_grant" },
              }),
            );
          }
          return await handle.send(body);
        })()),
    };

    return new OperationInvoker(
      transport,
      descriptor as TA["operations"][O] & RuntimeOperationDesc,
    ) as OperationSurface<TA, TMode, O>;
  }

  /**
   * Creates a helper for a short-lived Trellis transfer grant.
   */
  transfer(grant: SendTransferGrant): SendTransferHandle;
  transfer(grant: ReceiveTransferGrant): ReceiveTransferHandle;
  transfer(grant: TransferGrant): ReturnType<typeof createTransferHandle> {
    return createTransferHandle(
      this.#nats,
      this.#auth,
      this.timeout,
      grant,
      this.#inboxPrefix,
    );
  }

  /*
   * Mount a handler to process requests made to a specific Trellis API
   */
  async mount(
    method: string,
    fn: (args: {
      input: unknown;
      context: RpcHandlerContext;
      client: HandlerTrellis<TA, TRequests>;
    }) => MaybePromise<Result<unknown, BaseError>>,
  ) {
    const methodName = method as MethodsOf<TA>;
    const ctx = this.api["rpc"][methodName];
    if (!ctx) {
      throw this.#unknownApiError("RPC method", method.toString());
    }
    const task = this.#handleRPC(
      methodName,
      fn as HandlerFn<TA, MethodsOf<TA>, TA, HandlerTrellis<TA, TRequests>>,
    );
    this.#tasks.add(methodName, task);
  }

  #handleRPC(
    method: MethodsOf<TA>,
    fn: HandlerFn<TA, MethodsOf<TA>, TA, HandlerTrellis<TA, TRequests>>,
    subjectData: Record<string, unknown> = {},
  ): AsyncResult<void, ValidationError | UnexpectedError> {
    // Get API details
    const ctx = this.api["rpc"][method] as RpcDescriptorOf<TA, MethodsOf<TA>>;

    const subject = this.template(ctx.subject, subjectData, true).take();
    if (isErr(subject)) {
      return AsyncResult.lift(subject);
    }

    const handlerTrellis = this.#createHandlerTrellis();

    this.#log.info(
      { method: String(method) },
      `Mounting ${method.toString()} RPC handler`,
    );
    const sub = this.#nats.subscribe(subject, {
      queue: routeQueueGroup(subject),
    });

    return AsyncResult.try(async () => {
      for await (const msg of sub) {
        const resultPromise = await this.#processRPCMessage(
          method,
          ctx,
          msg,
          fn,
          handlerTrellis,
        );
        const result = resultPromise.take();

        if (isErr(result)) {
          this.#respondWithError(msg, result.error, { method: String(method) });
          continue;
        }

        const sent = this.#respondWithPayload(msg, result.payload, undefined, {
          method: String(method),
          responseKind: "success",
        });
        if (sent.isErr()) {
          const responseBytes = payloadByteLength(result.payload);
          const message = causeMessage(sent.error.cause);
          this.#respondWithError(
            msg,
            new TransportError({
              code: "trellis.rpc.response_send_failed",
              message: message.includes("max_payload")
                ? "Trellis RPC response exceeded NATS max_payload."
                : "Trellis could not send the RPC response.",
              hint:
                "Reduce the requested page size or use a narrower RPC that does not include large detail payloads.",
              cause: sent.error.cause,
              context: {
                method: String(method),
                subject: msg.subject,
                responseBytes,
                causeMessage: message,
              },
            }),
            { method: String(method), responseBytes },
          );
          continue;
        }

        if (result.afterReply.length > 0) {
          for (const task of result.afterReply) {
            try {
              await task();
            } catch (error) {
              this.#log.error(
                { method: String(method), error },
                "RPC after-reply task failed",
              );
            }
          }
        }
      }
    });
  }

  async #processRPCMessage(
    method: MethodsOf<TA>,
    ctx: RpcDescriptorOf<TA, MethodsOf<TA>>,
    msg: Msg,
    fn: HandlerFn<TA, MethodsOf<TA>, TA, HandlerTrellis<TA, TRequests>>,
    handlerTrellis: HandlerTrellis<TA, TRequests>,
  ): Promise<Result<ProcessedRpcResponse, BaseError>> {
    this.#log.debug(
      { method: String(method), subject: msg.subject },
      "Processing RPC message",
    );

    // Extract trace context from incoming NATS headers
    const parentContext = extractTraceContext(
      createNatsHeaderCarrier({
        get: (k: string) => msg.headers?.get(k) ?? undefined,
        set: () => {}, // Server doesn't need to set headers on incoming messages
      }),
    );

    // Start a server span for this RPC handler
    const span = startServerSpan(method, msg.subject, parentContext);
    const incomingTraceId = traceIdFromTraceparent(
      msg.headers?.get("traceparent"),
    );

    // Execute the handler within the span's context
    return withSpanAsync(span, async () => {
      const afterReply: (() => MaybePromise<void>)[] = [];
      const execute = async (): Promise<
        Result<ProcessedRpcResponse, BaseError>
      > => {
        const jsonData = safeJson(msg).take();
        if (isErr(jsonData)) {
          this.#log.warn(
            { method, error: jsonData.error.message },
            "Failed to parse JSON",
          );
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: "Failed to parse JSON",
          });
          recordRuntimeError(jsonData.error, {
            surface: "rpc",
            direction: "server",
            operation: String(method),
            phase: "parse",
          });
          return jsonData;
        }

        const parsedInput = parseRuntimeSchema(ctx.input, jsonData).take();
        if (isErr(parsedInput)) {
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: "Input validation failed",
          });
          recordRuntimeError(parsedInput.error, {
            surface: "rpc",
            direction: "server",
            operation: String(method),
            phase: "input_validation",
          });
          return parsedInput;
        }

        let caller: SessionCaller;
        let callerInboxPrefix = "_INBOX";
        const handlerRequestIdFromHeader = msg.headers?.get("request-id") ?? "";
        const handlerTraceIdFromHeader = traceIdFromTraceparent(
          msg.headers?.get("traceparent"),
        );

        const authRequired = ctx.authRequired ?? true;
        if (!authRequired) {
          caller = {
            type: "internal",
            capabilities: [],
          };
        } else {
          const auth = await verifyLocalAuthorization({
            kind: "request",
            cache: this.#auth.authorizationProviderCache,
            message: msg,
            permission: ctx.permission,
            requiredCapabilities: ctx.callerCapabilities,
          });
          const authValue = auth.take();
          if (isErr(authValue)) {
            span.setStatus({
              code: SpanStatusCode.ERROR,
              message: authValue.error.message,
            });
            recordRuntimeError(authValue.error, {
              surface: "rpc",
              direction: "server",
              operation: String(method),
              phase: "auth",
            });
            return err(authValue.error);
          }
          caller = authValue;
          callerInboxPrefix = authValue.inboxPrefix;
        }

        span.setAttribute("auth.caller.type", caller.type);
        if (caller.type === "verified") {
          span.setAttribute("auth.principal.kind", caller.principalKind);
          span.setAttribute("auth.principal.id", caller.principalId);
          span.setAttribute("auth.participant.id", caller.participantId);
          span.setAttribute("auth.connection.id", caller.connectionId);
          span.setAttribute("auth.grant.revision", caller.grantRevision);
          if (caller.loginSessionId) {
            span.setAttribute("auth.login_session.id", caller.loginSessionId);
          }
          if (caller.deploymentId) {
            span.setAttribute("auth.deployment.id", caller.deploymentId);
          }
          if (caller.instanceId) {
            span.setAttribute("auth.instance.id", caller.instanceId);
          }
        }

        const invokeHandler = fn as (
          args: {
            input: unknown;
            context: RpcHandlerContext;
            client: HandlerTrellis<TA, TRequests>;
          },
        ) => MaybeAsync<unknown, BaseError>;
        const handlerResultWrapped = await AsyncResult.try(async () =>
          await Promise.resolve(
            invokeHandler({
              input: parsedInput,
              context: {
                caller,
                sessionKey: caller.type === "verified" ? caller.sessionKey : "",
                inboxPrefix: callerInboxPrefix,
                permission: ctx.permission,
                requiredCapabilities: ctx.callerCapabilities,
                requestId: handlerRequestIdFromHeader || undefined,
                traceId: handlerTraceIdFromHeader || undefined,
                afterReply: (task) => afterReply.push(task),
              },
              client: handlerTrellis,
            }),
          )
        );

        if (handlerResultWrapped.isErr()) {
          const error = annotateHandlerBoundaryError(
            handlerResultWrapped.error,
            {
              method: String(method),
              requestId: msg.headers?.get("request-id"),
              service: this.name,
              contractId: this.contractId,
              contractDigest: this.contractDigest,
              traceId: activeTraceId(span) ?? incomingTraceId,
            },
          );
          this.#log.error(
            {
              method,
              error: error.message,
              cause: error.cause instanceof Error
                ? { message: error.cause.message, stack: error.cause.stack }
                : error.cause,
            },
            "Handler threw unexpectedly.",
          );
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: error.message,
          });
          span.recordException(error);
          recordRuntimeError(error, {
            surface: "rpc",
            direction: "server",
            operation: String(method),
            phase: "handler_throw",
          });
          return err(error);
        }

        const handlerResult = handlerResultWrapped.take() as {
          take: () => unknown;
        };
        const handlerOutcome = handlerResult.take();
        if (isErr(handlerOutcome)) {
          const error = annotateHandlerBoundaryError(handlerOutcome.error, {
            method: String(method),
            requestId: msg.headers?.get("request-id"),
            service: this.name,
            contractId: this.contractId,
            contractDigest: this.contractDigest,
            traceId: activeTraceId(span) ?? incomingTraceId,
          });

          this.#log.error(
            {
              method,
              error: error.message,
              errorType: error.name,
              cause: error.cause instanceof Error
                ? { message: error.cause.message, stack: error.cause.stack }
                : error.cause,
            },
            "Handler returned error.",
          );
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: error.message,
          });
          recordRuntimeError(error, {
            surface: "rpc",
            direction: "server",
            operation: String(method),
            phase: "handler_result",
          });
          return err(error);
        }

        const encoded = encodeSchema(ctx.output, handlerOutcome).take();
        if (isErr(encoded)) {
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: "Output encoding failed",
          });
          recordRuntimeError(encoded.error, {
            surface: "rpc",
            direction: "server",
            operation: String(method),
            phase: "output_encoding",
          });
          return encoded;
        }

        span.setStatus({ code: SpanStatusCode.OK });
        return ok({ payload: encoded, afterReply });
      };

      const result = await execute();
      if (isErr(result)) {
        result.error.withTraceId(activeTraceId(span) ?? incomingTraceId);
      }
      span.end();
      return result;
    });
  }

  #respondWithPayload(
    msg: Msg,
    payload: string,
    options: { headers?: MsgHdrs } | undefined,
    context: {
      method?: string;
      responseKind: "success" | "error";
    },
  ): Result<void, UnexpectedError> {
    const responseBytes = payloadByteLength(payload);
    try {
      msg.respond(payload, options);
      return ok(undefined);
    } catch (cause) {
      const error = new UnexpectedError({
        cause,
        context: {
          method: context.method,
          responseKind: context.responseKind,
          subject: msg.subject,
          reply: msg.reply,
          responseBytes,
          causeMessage: causeMessage(cause),
        },
      });
      this.#log.error(
        {
          method: context.method,
          responseKind: context.responseKind,
          subject: msg.subject,
          reply: msg.reply,
          responseBytes,
          cause: causeLogData(cause),
        },
        "Failed to send RPC response",
      );
      return err(error);
    }
  }

  #respondWithError(
    msg: Msg,
    error: Error | BaseError,
    context: { method?: string; responseBytes?: number } = {},
  ): void {
    const trellisError = error instanceof BaseError &&
        !(error instanceof RemoteError)
      ? error
      : new UnexpectedError({ cause: error });

    this.#log.error(
      {
        method: context.method,
        subject: msg.subject,
        responseBytes: context.responseBytes,
        error: trellisError.toSerializable(),
      },
      "RPC error",
    );

    const errorData = trellisError.toSerializable();
    const hdrs = natsHeaders();
    hdrs.set("status", "error");

    const serialized = Result.try(() => JSON.stringify(errorData));
    if (serialized.isErr()) {
      this.#log.error(
        { error: serialized.error },
        "Failed to serialize error response",
      );
      this.#respondWithPayload(
        msg,
        '{"type":"UnexpectedError","message":"Failed to serialize error"}',
        { headers: hdrs },
        { method: context.method, responseKind: "error" },
      );
      return;
    }
    this.#respondWithPayload(
      msg,
      serialized.take() as string,
      { headers: hdrs },
      { method: context.method, responseKind: "error" },
    );
  }

  respondWithError(msg: Msg, error: Error | BaseError): void {
    this.#respondWithError(msg, error);
  }

  /**
   * Builds a stable event subject, encoded payload, and publish headers.
   *
   * The prepared event intentionally carries no contract id or digest so callers
   * can persist it in a service-owned outbox without coupling storage to the
   * publisher's current deployment metadata.
   */
  prepare(
    event: string,
    data: Record<string, unknown>,
  ): Result<PreparedTrellisEvent, ValidationError | UnexpectedError> {
    try {
      const eventName = event as EventsOf<TA>;
      const ctx = this.api["events"][eventName] as EventDescriptorOf<
        TA,
        typeof eventName
      >;
      if (!ctx) {
        const error = new UnexpectedError({
          cause: this.#unknownApiError("event", event.toString()),
          context: { event: event.toString() },
        });
        recordRuntimeError(error, {
          surface: "event",
          direction: "publisher",
          operation: event,
          phase: "prepare",
        });
        return err(error);
      }

      const subject = this.template(ctx.subject, data).take();
      if (isErr(subject)) {
        logger.error({ err: subject.error }, "Failed to template event.");
        recordRuntimeError(subject.error, {
          surface: "event",
          direction: "publisher",
          operation: event,
          phase: "request_encoding",
        });
        return subject;
      }

      const header = {
        id: ulid(),
        time: new Date().toISOString(),
      };
      const payload = Object.freeze({ ...data });
      const msg = encodeSchema(ctx.event, payload).take();
      if (isErr(msg)) {
        logger.error({ err: msg.error }, "Failed to encode event.");
        const error = new UnexpectedError({ cause: msg.error });
        recordRuntimeError(error, {
          surface: "event",
          direction: "publisher",
          operation: event,
          phase: "request_encoding",
        });
        return err(error);
      }

      const headers = natsHeaders();
      injectTraceContext(createNatsHeaderCarrier(headers));

      const headerRecord: Record<string, string> = {};
      for (const [key, value] of headers) {
        headerRecord[key] = value.join(",");
      }
      headerRecord["Trellis-Event-Descriptor"] = ctx.descriptorIdentity;

      return ok(Object.freeze({
        event: event.toString(),
        descriptorIdentity: ctx.descriptorIdentity,
        subject,
        header: Object.freeze(header),
        payload,
        encodedPayload: msg,
        headers: Object.freeze(headerRecord),
      }));
    } catch (cause) {
      const error = new UnexpectedError({
        cause,
        context: { event: event.toString() },
      });
      recordRuntimeError(error, {
        surface: "event",
        direction: "publisher",
        operation: event,
        phase: "prepare",
      });
      return err(error);
    }
  }

  /**
   * Publishes a previously prepared event without regenerating its id, time,
   * subject, payload, or headers.
   */
  publishPrepared(
    event: PreparedTrellisEvent,
  ): AsyncResult<void, UnexpectedError> {
    return AsyncResult.from((async () => {
      try {
        const headers = natsHeaders();
        for (const [key, value] of Object.entries(event.headers)) {
          headers.set(key, value);
        }
        headers.set("Nats-Msg-Id", event.header.id);
        headers.set("Trellis-Event-Time", event.header.time);
        headers.set("Trellis-Event-Descriptor", event.descriptorIdentity);
        const proof = await this.#createEventProof(event);
        headers.set("proof", proof.proof);
        headers.set("authorization-context", proof.contextDigest);
        headers.set("session-key", this.#auth.sessionKey);

        logger.trace(
          { subject: event.subject },
          `Publishing ${event.event} event.`,
        );
        await this.#js.publish(event.subject, event.encodedPayload, {
          headers,
        });
        return ok(undefined);
      } catch (cause) {
        const error = new UnexpectedError({
          cause,
          context: { event: event.event },
        });
        recordRuntimeError(error, {
          surface: "event",
          direction: "publisher",
          operation: event.event,
          phase: "publish",
        });
        return err(error);
      }
    })());
  }

  publish(
    event: string,
    data: Record<string, unknown>,
  ): AsyncResult<void, ValidationError | UnexpectedError> {
    return AsyncResult.from((async () => {
      const prepared = this.prepare(event, data).take();
      if (isErr(prepared)) return prepared;
      return await this.publishPrepared(prepared);
    })());
  }

  listenEvent<E extends EventsOf<TA>>(
    event: E,
    subjectData: Record<string, unknown>,
    fn: EventCallback<EventOf<TA, E>>,
    opts?: EventOpts,
  ): AsyncResult<void, ValidationError | UnexpectedError> {
    return AsyncResult.from((async () => {
      try {
        const eventName = event as EventsOf<TA>;
        const ctx = this.api["events"][eventName] as EventDescriptorOf<
          TA,
          typeof eventName
        >;
        if (!ctx) {
          return err(
            new UnexpectedError({
              cause: this.#unknownApiError("event", event.toString()),
              context: { event: event.toString() },
            }),
          );
        }
        const subject = this.template(ctx.subject, subjectData, true).take();
        if (isErr(subject)) return subject;

        if (opts?.mode === "ephemeral") {
          return await this.#startEphemeralEvent(
            eventName,
            ctx,
            subject,
            fn,
            opts.signal,
          );
        }

        const groupResult = this.#resolveEventConsumerGroup(eventName, opts);
        const group = groupResult.take();
        if (isErr(group)) return group;

        this.#registerDurableEventHandler({
          group,
          event: eventName,
          ctx,
          subject,
          fn,
          signal: opts?.signal,
        });
        return ok(undefined);
      } catch (cause) {
        return err(
          new UnexpectedError({ cause, context: { event: event.toString() } }),
        );
      }
    })());
  }

  async #startEphemeralEvent(
    event: EventsOf<TA>,
    ctx: EventDescriptorOf<TA, EventsOf<TA>>,
    subject: string,
    fn: EventCallback<EventOf<TA, EventsOf<TA>>>,
    signal?: AbortSignal,
  ): Promise<Result<void, ValidationError | UnexpectedError>> {
    let sub: ReturnType<NatsConnection["subscribe"]> | undefined;
    try {
      sub = this.#nats.subscribe(subject);
      if (signal) {
        if (signal.aborted) {
          sub.unsubscribe();
          return ok(undefined);
        }
        signal.addEventListener("abort", () => sub?.unsubscribe(), {
          once: true,
        });
      }
    } catch (cause) {
      if (sub) {
        sub.unsubscribe();
      }
      return err(
        new UnexpectedError({
          cause,
          context: { event: String(event), subject },
        }),
      );
    }

    const task = AsyncResult.try(async () => {
      for await (const msg of sub) {
        const proofResult = await this.#validateEventProof(event, ctx, msg);
        const proofValue = proofResult.take();
        if (isErr(proofValue)) {
          this.#log.warn(
            { error: proofValue.error, event, subject: msg.subject },
            "Event auth validation failed",
          );
          continue;
        }

        const parsedEvent = this.#parseEventMessage(event, ctx, msg);
        const m = parsedEvent.take();
        if (isErr(m)) {
          this.#log.error({ error: m.error }, "Event validation failed");
          recordRuntimeError(m.error, {
            surface: "event",
            direction: "consumer",
            operation: String(event),
            phase: "input_validation",
          });
          continue;
        }

        const handlerResult = await this.#invokeEventHandler({
          event,
          payload: m,
          mode: "ephemeral",
          message: msg,
          fn,
        });
        const handlerValue = handlerResult.take();
        if (isErr(handlerValue)) {
          recordRuntimeError(handlerValue.error, {
            surface: "event",
            direction: "consumer",
            operation: String(event),
            phase: "handler_result",
          });
          this.#log.error(
            {
              error: handlerValue.error.toSerializable(),
              event,
              subject: msg.subject,
            },
            "Event handler failed",
          );
        }
      }
    });

    this.#tasks.add(`event:${event}:${ulid()}`, task);
    return ok(undefined);
  }

  async #invokeEventHandler(args: {
    event: EventsOf<TA>;
    payload: unknown;
    mode: "durable" | "ephemeral";
    group?: string;
    message: Pick<Msg, "headers" | "subject"> & object;
    fn: EventCallback<EventOf<TA, EventsOf<TA>>>;
  }): Promise<Result<void, BaseError>> {
    const annotation = {
      event: String(args.event),
      service: this.name,
      contractId: this.contractId,
      contractDigest: this.contractDigest,
      traceId: traceIdFromTraceparent(args.message.headers?.get("traceparent")),
    };
    try {
      const result = await Promise.resolve(args.fn(
        args.payload as EventOf<TA, EventsOf<TA>>,
        createEventListenerContext({
          subject: args.message.subject,
          mode: args.mode,
          ...(args.group ? { group: args.group } : {}),
          message: args.message,
        }),
      ));
      const outcome = isResultLike(result) ? result.take() : result;
      if (isErr(outcome)) {
        return err(annotateHandlerBoundaryError(outcome.error, annotation));
      }
      return ok(undefined);
    } catch (cause) {
      return err(annotateHandlerBoundaryError(cause, annotation));
    }
  }

  #resolveEventConsumerGroup(
    event: EventsOf<TA>,
    opts: EventOpts | undefined,
  ): Result<string, UnexpectedError> {
    const metadata = this.#eventConsumers.metadata;
    const bindings = this.#eventConsumers.bindings ?? {};
    const groups = Object.entries(metadata ?? {})
      .filter(([, group]) =>
        eventConsumerGroupEvents(group).includes(String(event))
      )
      .map(([group]) => group);

    if (opts?.group) {
      if (!groups.includes(opts.group)) {
        return err(
          new UnexpectedError({
            cause: new Error(
              `Event '${
                String(event)
              }' is not declared in event consumer group '${opts.group}'.`,
            ),
            context: { event: String(event), group: opts.group },
          }),
        );
      }
      if (!bindings[opts.group]) {
        return err(
          new UnexpectedError({
            cause: new Error(
              `Event consumer group '${opts.group}' has no Trellis-provisioned binding.`,
            ),
            context: { event: String(event), group: opts.group },
          }),
        );
      }
      return ok(opts.group);
    }

    if (groups.length === 0) {
      return err(
        new UnexpectedError({
          cause: new Error(
            `Event '${
              String(event)
            }' is not declared in any event consumer group.`,
          ),
          context: { event: String(event) },
        }),
      );
    }
    if (groups.length > 1) {
      return err(
        new UnexpectedError({
          cause: new Error(
            `Event '${
              String(event)
            }' is declared in multiple event consumer groups; pass opts.group.`,
          ),
          context: { event: String(event), groups },
        }),
      );
    }

    const group = groups[0]!;
    if (!bindings[group]) {
      return err(
        new UnexpectedError({
          cause: new Error(
            `Event consumer group '${group}' has no Trellis-provisioned binding.`,
          ),
          context: { event: String(event), group },
        }),
      );
    }
    return ok(group);
  }

  #registerDurableEventHandler(args: {
    group: string;
    event: EventsOf<TA>;
    ctx: EventDescriptorOf<TA, EventsOf<TA>>;
    subject: string;
    fn: EventCallback<EventOf<TA, EventsOf<TA>>>;
    signal?: AbortSignal;
  }): void {
    if (args.signal?.aborted || this.#durableEventListenersStopped) return;

    const binding = this.#eventConsumers.bindings?.[args.group];
    if (!binding) {
      throw new Error(
        `Event consumer group '${args.group}' has no Trellis-provisioned binding.`,
      );
    }
    if (!Number.isInteger(binding.concurrency) || binding.concurrency < 1) {
      throw new Error(
        `Event consumer group '${args.group}' has invalid concurrency ${binding.concurrency}; expected a positive integer`,
      );
    }

    const loop = this.#durableEventLoops.get(args.group) ?? {
      registrations: [],
      concurrency: binding.concurrency,
      startedWorkers: new Set<number>(),
      messages: new Set<ConsumerMessages>(),
    };
    if (loop.concurrency !== binding.concurrency) {
      throw new Error(
        `Event consumer group '${args.group}' is already registered with concurrency ${loop.concurrency}; binding now requires ${binding.concurrency}`,
      );
    }
    const registration: DurableEventRegistration<TA> = {
      event: args.event,
      ctx: args.ctx,
      subject: args.subject,
      fn: args.fn,
    };
    loop.registrations.push(registration);
    this.#durableEventLoops.set(args.group, loop);

    args.signal?.addEventListener("abort", () => {
      const index = loop.registrations.indexOf(registration);
      if (index >= 0) loop.registrations.splice(index, 1);
      if (!this.#durableEventConsumerGroupReady(args.group, loop)) {
        for (const messages of loop.messages) messages.stop();
      }
    }, { once: true });

    this.#startDurableEventConsumer(args.group, loop);
  }

  #startDurableEventConsumer(
    group: string,
    loop: DurableEventConsumerLoop<TA>,
  ): void {
    if (
      this.#durableEventListenersStopped ||
      !this.#durableEventConsumerGroupReady(group, loop)
    ) {
      return;
    }
    for (
      let workerIndex = 0;
      workerIndex < loop.concurrency;
      workerIndex += 1
    ) {
      if (loop.startedWorkers.has(workerIndex)) continue;
      loop.startedWorkers.add(workerIndex);
      this.#tasks.add(
        `event-consumer:${group}:${workerIndex}:${ulid()}`,
        this.#runDurableEventConsumer(group, loop, workerIndex),
      );
    }
  }

  #durableEventConsumerGroupReady(
    group: string,
    loop: DurableEventConsumerLoop<TA>,
  ): boolean {
    const metadata = this.#eventConsumers.metadata?.[group];
    if (!metadata) return false;
    return eventConsumerGroupEvents(metadata).every((event) =>
      loop.registrations.some((registration) =>
        String(registration.event) === event
      )
    );
  }

  #runDurableEventConsumer(
    group: string,
    loop: DurableEventConsumerLoop<TA>,
    workerIndex: number,
  ): AsyncResult<void, ValidationError | UnexpectedError> {
    return AsyncResult.from((async () => {
      const binding = this.#eventConsumers.bindings?.[group];
      if (!binding) {
        return err(
          new UnexpectedError({
            cause: new Error(
              `Event consumer group '${group}' has no Trellis-provisioned binding.`,
            ),
            context: { group },
          }),
        );
      }

      let originalOpened = false;
      let fetchingReplay = true;
      try {
        const infoResult = await AsyncResult.try(async () => {
          const jsm = await jetstreamManager(this.#nats);
          return await jsm.consumers.info(binding.stream, binding.consumerName);
        });
        const info = infoResult.take();
        if (isErr(info)) {
          if (
            this.#durableEventListenersStopped ||
            !this.#durableEventConsumerGroupReady(group, loop)
          ) {
            return ok(undefined);
          }
          if (isConsumerNotFoundError(info.error.cause)) {
            this.#log.debug(
              { group, stream: binding.stream, consumer: binding.consumerName },
              "Durable event consumer is not available yet; retrying",
            );
            await sleep(25);
            return ok(undefined);
          }
          return info;
        }
        originalOpened = true;

        const replayInfo = await (await jetstreamManager(this.#nats)).consumers
          .info(
            binding.replayBinding.stream,
            binding.replayBinding.consumerName,
          );
        const consumers = [
          this.#js.consumers.getConsumerFromInfo(info),
          this.#js.consumers.getConsumerFromInfo(replayInfo),
        ];
        let replay = false;
        while (
          !this.#durableEventListenersStopped &&
          this.#durableEventConsumerGroupReady(group, loop)
        ) {
          fetchingReplay = replay;
          const messages = await consumers[replay ? 1 : 0]!.fetch({
            max_messages: 1,
            expires: 1_000,
          });
          const isReplay = replay;
          replay = !replay;
          loop.messages.add(messages);
          if (!this.#durableEventConsumerGroupReady(group, loop)) {
            messages.stop();
            loop.messages.delete(messages);
            break;
          }
          try {
            await this.#handleDurableEventConsumer(
              group,
              loop,
              messages,
              isReplay,
            )
              .orThrow();
          } finally {
            loop.messages.delete(messages);
          }
        }
      } catch (cause) {
        if (
          this.#durableEventListenersStopped ||
          !this.#durableEventConsumerGroupReady(group, loop)
        ) {
          return ok(undefined);
        }
        if (
          isConsumerNotFoundError(cause) &&
          (fetchingReplay || !originalOpened)
        ) {
          this.#log.debug(
            { group, stream: binding.stream, consumer: binding.consumerName },
            "Durable event consumer is not available yet; retrying",
          );
          await sleep(25);
          return ok(undefined);
        }
        await sleep(25);
        return err(new UnexpectedError({ cause, context: { group } }));
      } finally {
        loop.startedWorkers.delete(workerIndex);
        if (!this.#durableEventListenersStopped) {
          this.#startDurableEventConsumer(group, loop);
        }
      }
      return ok(undefined);
    })());
  }

  #handleDurableEventConsumer(
    group: string,
    loop: DurableEventConsumerLoop<TA>,
    messages: ConsumerMessages,
    isReplay = false,
  ): AsyncResult<void, ValidationError | UnexpectedError> {
    return AsyncResult.try(async () => {
      const binding = this.#eventConsumers.bindings?.[group];
      if (!binding) {
        throw new Error(`Event consumer group '${group}' is unavailable`);
      }
      for await (const msg of messages) {
        let replayEnvelope: ConsumerReplayEnvelope | undefined;
        let deliveredMessage: Pick<
          Msg,
          "data" | "headers" | "subject" | "json"
        > = msg;
        if (isReplay) {
          const parsedReplayEnvelope = JSON.parse(
            new TextDecoder().decode(msg.data),
          ) as ConsumerReplayEnvelope;
          replayEnvelope = parsedReplayEnvelope;
          if (
            parsedReplayEnvelope.resourceId !== binding.resourceId ||
            parsedReplayEnvelope.originalRecordSequence <= 0
          ) {
            msg.ack();
            continue;
          }
          let inspected: Record<string, unknown>;
          try {
            inspected = await this.#requestConsumerManagement(
              "DeadLetters.Inspect",
              {
                resourceId: binding.resourceId,
                deadLetterId: parsedReplayEnvelope.deadLetterId,
              },
            );
          } catch (error) {
            this.#log.warn(
              { error, group },
              "Replay state inspection unavailable",
            );
            const delivery = msg.info.deliveryCount;
            msg.nak(
              binding.backoffMs[
                Math.min(
                  Math.max(0, delivery - 1),
                  binding.backoffMs.length - 1,
                )
              ] ?? binding.ackWaitMs,
            );
            continue;
          }
          const detail = Reflect.get(inspected, "deadLetter");
          const deadLetter = detail && typeof detail === "object"
            ? Reflect.get(detail, "deadLetter")
            : undefined;
          const projectedGeneration =
            deadLetter && typeof deadLetter === "object"
              ? Reflect.get(deadLetter, "generation")
              : undefined;
          if (
            typeof projectedGeneration !== "number" ||
            projectedGeneration < parsedReplayEnvelope.generation
          ) {
            msg.nak(25);
            continue;
          }
          if (
            projectedGeneration !== parsedReplayEnvelope.generation ||
            !["replayPending", "replaying"].includes(
              Reflect.get(deadLetter, "state"),
            )
          ) {
            msg.ack();
            continue;
          }
          const originalHeaders = natsHeaders();
          for (
            const [name, values] of Object.entries(
              parsedReplayEnvelope.originalHeaders,
            )
          ) {
            for (const value of values) originalHeaders.append(name, value);
          }
          const data = new Uint8Array(
            parsedReplayEnvelope.originalPayloadBytes,
          );
          deliveredMessage = {
            data,
            headers: originalHeaders,
            subject: parsedReplayEnvelope.originalSubject,
            json: () => JSON.parse(new TextDecoder().decode(data)),
          };
        }
        if (!this.#durableEventConsumerGroupReady(group, loop)) {
          messages.stop();
          break;
        }
        const matching = loop.registrations.filter((registration) =>
          natsSubjectMatches(registration.subject, deliveredMessage.subject)
        );
        if (matching.length === 0) {
          this.#log.warn(
            { group, subject: msg.subject },
            "Durable event consumer received message without registered handler",
          );
          msg.nak();
          continue;
        }

        let failed = false;
        for (const registration of matching) {
          const proofResult = await this.#validateEventProof(
            registration.event,
            registration.ctx,
            deliveredMessage,
          );
          const proofValue = proofResult.take();
          if (isErr(proofValue)) {
            recordRuntimeError(proofValue.error, {
              surface: "event",
              direction: "consumer",
              operation: String(registration.event),
              phase: "auth",
            });
            this.#log.warn(
              {
                error: proofValue.error,
                event: registration.event,
                subject: msg.subject,
              },
              "Event auth validation failed",
            );
            if (
              proofValue.error instanceof EventVerificationAuthError &&
              proofValue.error.retryable
            ) {
              const delivery = msg.info.deliveryCount;
              msg.nak(
                binding.backoffMs[
                  Math.min(
                    Math.max(0, delivery - 1),
                    binding.backoffMs.length - 1,
                  )
                ] ?? binding.ackWaitMs,
              );
            } else {
              if (replayEnvelope) {
                try {
                  msg.working();
                  await this.#reportConsumerDelivery({
                    resourceId: binding.resourceId,
                    sourceStream: binding.replayBinding.stream,
                    sourceSequence: BigInt(msg.info.streamSequence),
                    deliveryCount: msg.info.deliveryCount,
                    deliveryProof: jetStreamDeliveryProof(msg),
                    replayGeneration: replayEnvelope.generation,
                    outcome: "unreplayable",
                    error: proofValue.error.message,
                  });
                } catch (error) {
                  this.#log.warn(
                    { error, group },
                    "Replay delivery report unavailable",
                  );
                  failed = true;
                  break;
                }
              }
              msg.term();
            }
            failed = true;
            break;
          }

          const parsedEvent = this.#parseEventMessage(
            registration.event,
            registration.ctx,
            deliveredMessage,
          );
          const eventPayload = parsedEvent.take();
          if (isErr(eventPayload)) {
            this.#log.error(
              { error: eventPayload.error },
              "Event validation failed",
            );
            recordRuntimeError(eventPayload.error, {
              surface: "event",
              direction: "consumer",
              operation: String(registration.event),
              phase: "input_validation",
            });
            msg.term();
            failed = true;
            break;
          }

          const delivery = msg.info.deliveryCount;
          const wait = binding.backoffMs[
            Math.min(Math.max(0, delivery - 1), binding.backoffMs.length - 1)
          ] ?? binding.ackWaitMs;
          const progress = setInterval(
            () => msg.working(),
            Math.max(1, Math.min(wait - 1, Math.floor(wait / 3))),
          );
          const handlerResult = await this.#invokeEventHandler({
            event: registration.event,
            payload: eventPayload,
            mode: "durable",
            group,
            message: deliveredMessage,
            fn: registration.fn,
          }).finally(() => clearInterval(progress));
          const handlerValue = handlerResult.take();
          if (isErr(handlerValue)) {
            recordRuntimeError(handlerValue.error, {
              surface: "event",
              direction: "consumer",
              operation: String(registration.event),
              phase: "handler_result",
            });
            this.#log.error(
              {
                error: handlerValue.error.toSerializable(),
                event: registration.event,
                subject: msg.subject,
              },
              "Event handler failed",
            );
            if (delivery >= binding.maxDeliver) {
              try {
                msg.working();
                await this.#reportConsumerDelivery({
                  resourceId: binding.resourceId,
                  sourceStream: isReplay
                    ? binding.replayBinding.stream
                    : binding.stream,
                  sourceSequence: BigInt(msg.info.streamSequence),
                  deliveryCount: delivery,
                  deliveryProof: jetStreamDeliveryProof(msg),
                  ...(replayEnvelope
                    ? { replayGeneration: replayEnvelope.generation }
                    : {}),
                  outcome: "exhausted",
                  error: JSON.stringify(handlerValue.error.toSerializable()),
                });
                msg.ack();
              } catch (error) {
                this.#log.warn({ error, group }, "Delivery report unavailable");
              }
              failed = true;
              break;
            }
            const delay = binding.backoffMs[
              Math.min(Math.max(0, delivery - 1), binding.backoffMs.length - 1)
            ] ?? 0;
            msg.nak(delay);
            failed = true;
            break;
          }
        }

        if (!failed) {
          if (replayEnvelope) {
            try {
              msg.working();
              await this.#reportConsumerDelivery({
                resourceId: binding.resourceId,
                sourceStream: binding.replayBinding.stream,
                sourceSequence: BigInt(msg.info.streamSequence),
                deliveryCount: msg.info.deliveryCount,
                deliveryProof: jetStreamDeliveryProof(msg),
                replayGeneration: replayEnvelope.generation,
                outcome: "succeeded",
              });
            } catch (error) {
              this.#log.warn({ error, group }, "Delivery report unavailable");
              continue;
            }
          }
          msg.ack();
        }
      }
    });
  }

  async #reportConsumerDelivery(input: Record<string, unknown>): Promise<void> {
    await this.#requestConsumerManagement("Consumers.ReportDelivery", input);
  }

  async #requestConsumerManagement(
    method: string,
    input: Record<string, unknown>,
  ): Promise<Record<string, unknown>> {
    const binding = this.#apiBindings[EVENTS_API.identity];
    const providerDeploymentId = binding && typeof binding === "object"
      ? Reflect.get(binding, "providerDeploymentId")
      : undefined;
    const action = method === "Consumers.ReportDelivery"
      ? EVENTS_API.actions["rpc:Consumers.ReportDelivery"]
      : method === "DeadLetters.Inspect"
      ? EVENTS_API.actions["rpc:DeadLetters.Inspect"]
      : undefined;
    if (typeof providerDeploymentId !== "string" || !action) {
      throw new Error(`Consumer management '${method}' is unavailable`);
    }
    return await this.#requestBuiltRpc<Record<string, unknown>>(method, input, {
      subject: boundApiSubject(
        "rpc",
        EVENTS_API.identity,
        providerDeploymentId,
        method,
      ),
      input: action.input,
      output: action.output,
      callerCapabilities: [],
      errors: action.errors.map((error) => error.type),
      declaredErrorTypes: action.errors.map((error) => error.type),
      runtimeErrors: action.errors,
    }).orThrow();
  }

  #parseEventMessage(
    event: EventsOf<TA>,
    ctx: EventDescriptorOf<TA, EventsOf<TA>>,
    msg: Pick<Msg, "json" | "subject">,
  ): Result<
    unknown,
    SchemaValidationError | ValidationError | UnexpectedError
  > {
    const jsonData = Result.try<JsonValue>(() => msg.json());
    const json = jsonData.take();
    if (isErr(json)) {
      this.#log.error(
        { error: json.error, event, subject: msg.subject },
        "Event parse failed",
      );
      return json;
    }

    return parseRuntimeSchema(ctx.event, json);
  }

  async #validateEventProof(
    event: EventsOf<TA>,
    descriptor: EventDescriptorOf<TA, EventsOf<TA>>,
    msg: Pick<Msg, "data" | "headers" | "subject">,
  ): Promise<Result<VerifiedCaller, BaseError>> {
    return await verifyLocalAuthorization({
      kind: "event",
      cache: this.#auth.authorizationProviderCache,
      message: msg,
      permission: descriptor.publishPermission,
      descriptorIdentity: descriptor.descriptorIdentity,
      requiredCapabilities: descriptor.publishCapabilities,
    });
  }

  wait(): AsyncResult<void, BaseError> {
    return this.#tasks.wait();
  }

  /** Stops durable event listener loops without closing the underlying transport. */
  stopEventListeners(): void {
    this.#durableEventListenersStopped = true;
    for (const loop of this.#durableEventLoops.values()) {
      loop.registrations.splice(0, loop.registrations.length);
      for (const messages of loop.messages) messages.stop();
    }
  }

  // FIXME: If are validating things twice in most cases...
  template(
    subject: string,
    data: unknown,
    allowWildcards = false,
  ): Result<string, ValidationError> {
    // Find all template placeholders and check if values exist
    const placeholders = subject.match(/\{([^}]+)\}/g) || [];
    for (const placeholder of placeholders) {
      const key = placeholder.slice(1, -1); // Remove { and }
      const value = Pointer.Get(data, key);

      if ((value === undefined || value === null) && !allowWildcards) {
        return err(
          new ValidationError({
            errors: [
              {
                path: key,
                message: "Missing required data for subject template",
              },
            ],
            context: { key },
          }),
        );
      }
      if (
        value !== undefined && value !== null && value !== "*" &&
        (typeof value !== "string" && typeof value !== "number" ||
          typeof value === "number" &&
            (!Number.isFinite(value) ||
              Number.isInteger(value) && !Number.isSafeInteger(value)))
      ) {
        return err(
          new ValidationError({
            errors: [{
              path: key,
              message: "Subject template values must be strings or numbers",
            }],
            context: { key },
          }),
        );
      }
    }

    const result = subject.replace(/\{([^}]+)\}/g, (_, key) => {
      const value = Pointer.Get(data, key);
      if (allowWildcards && value === "*") {
        return "*";
      }
      if (allowWildcards && (value === undefined || value === null)) {
        return "*";
      }
      return this.#encodeSubjectToken(`${Object.is(value, -0) ? 0 : value}`);
    });

    return ok(result);
  }

  #encodeSubjectToken(token: string): string {
    return encodeEventSubjectParameterToken(token);
  }

  #currentIat(): number {
    return this.#auth.currentIat?.() ?? Math.floor(Date.now() / 1000);
  }

  #contextDigest(): string {
    const digest = typeof this.#auth.contextDigest === "function"
      ? this.#auth.contextDigest()
      : this.#auth.contextDigest;
    if (digest === undefined) {
      throw new Error(
        "contextDigest is required to sign v1 request and event proofs",
      );
    }
    return digest;
  }

  /** Creates a context-bound request proof for an exact subject, payload, and reply. */
  protected async createRequestProof(
    subject: string,
    payload: string,
    reply: string,
  ): Promise<
    { proof: string; iat: number; requestId: string; contextDigest: string }
  > {
    const contextDigest = this.#contextDigest();
    const payloadBytes = new TextEncoder().encode(payload);
    const payloadHash = await sha256(payloadBytes);
    const iat = this.#currentIat();
    const requestId = ulid();
    const input = buildProofInput(
      contextDigest,
      subject,
      reply,
      payloadHash,
      iat,
      requestId,
    );
    const digest = await sha256(input);
    const sigBytes = await this.#auth.sign(digest);
    return {
      proof: base64urlEncode(sigBytes),
      iat,
      requestId,
      contextDigest,
    };
  }

  async #createEventProof(
    event: PreparedTrellisEvent,
  ): Promise<{ proof: string; contextDigest: string }> {
    const contextDigest = this.#contextDigest();
    const payloadHash = await sha256(
      new TextEncoder().encode(event.encodedPayload),
    );
    const input = buildEventProofInput(
      contextDigest,
      event.descriptorIdentity,
      event.subject,
      payloadHash,
      event.header.id,
      event.header.time,
    );
    const digest = await sha256(input);
    return {
      proof: base64urlEncode(await this.#auth.sign(digest)),
      contextDigest,
    };
  }

  async #requestMessageWithRetry(args: {
    method?: string;
    subject: string;
    payload: string;
    timeout: number;
    signal?: AbortSignal;
    callerCapabilities?: readonly string[];
    span?: Span;
  }): Promise<Result<Msg, TransportError>> {
    for (let retry = 0; retry <= this.#noResponderMaxRetries; retry++) {
      if (args.signal?.aborted) {
        return err(classifyRequestTransportFailure({
          method: args.method,
          subject: args.subject,
          callerCapabilities: args.callerCapabilities,
          cause: args.signal.reason ??
            new DOMException("Request aborted", "AbortError"),
        }));
      }
      if (this.#nats.isClosed()) {
        return err(requestFailedTransportError({
          code: "trellis.request.closed",
          message: "The Trellis connection is closed.",
          hint: "Connect to Trellis again before making another request.",
          method: args.method,
          subject: args.subject,
        }));
      }
      // Create the exact reply inbox before signing so the proof binds the
      // reply subject the response arrives on.
      const reply = createInbox(this.#inboxPrefix);
      const authHeaders = await this.createRequestProof(
        args.subject,
        args.payload,
        reply,
      );
      const headers = natsHeaders();
      headers.set("authorization-context", authHeaders.contextDigest);
      headers.set("session-key", this.#auth.sessionKey);
      headers.set("proof", authHeaders.proof);
      headers.set("iat", String(authHeaders.iat));
      headers.set("request-id", authHeaders.requestId);
      injectTraceContext(createNatsHeaderCarrier(headers), args.span);

      const result = await AsyncResult.try(async () => {
        const response = Promise.withResolvers<Msg>();
        const abort = () =>
          response.reject(
            args.signal?.reason ??
              new DOMException("Request aborted", "AbortError"),
          );
        args.signal?.addEventListener("abort", abort, { once: true });
        const subscription = this.#nats.subscribe(reply, {
          max: 1,
          timeout: args.timeout,
          callback: (error, message) => {
            if (error) response.reject(error);
            else if (
              message.data.length === 0 && message.headers?.code === 503
            ) {
              response.reject(new Error("no responders"));
            } else response.resolve(message);
          },
        });
        // NATS noMux requests abandon their promise when connection closure
        // cancels the subscription timer. Bind settlement to this subscription.
        subscription.closed.then((error) => {
          response.reject(
            error ?? new Error("connection closed before RPC response"),
          );
        });
        const initialStatus = this.connection.status;
        const stopObserving = this.connection.subscribe((status) => {
          // Publish denials arrive on the connection, not the reply inbox.
          if (status === initialStatus) return;
          const error = status.transport?.error;
          if (
            error instanceof Error &&
            ((error.name === "PermissionViolationError" &&
              Reflect.get(error, "operation") === "publish" &&
              Reflect.get(error, "subject") === args.subject) ||
              error.name === "AuthorizationError" ||
              error.name === "UserAuthenticationExpiredError")
          ) response.reject(error);
        });
        try {
          if (args.signal?.aborted) {
            abort();
          } else {
            this.#nats.publish(args.subject, args.payload, {
              headers,
              reply,
            });
          }
          return await response.promise;
        } finally {
          args.signal?.removeEventListener("abort", abort);
          stopObserving();
          subscription.unsubscribe();
        }
      });

      if (result.isOk()) {
        return ok(result.take() as Msg);
      }

      const cause = result.error.cause;
      const message = cause instanceof Error ? cause.message : String(cause);
      const isNoResponders = message.includes("no responders");

      if (isNoResponders && retry < this.#noResponderMaxRetries) {
        this.#log.debug(
          { method: args.method, subject: args.subject, retry },
          "No responders, retrying...",
        );
        await new Promise<void>((resolve) => {
          const done = () => {
            clearTimeout(timeout);
            args.signal?.removeEventListener("abort", done);
            resolve();
          };
          const timeout = setTimeout(
            done,
            this.#noResponderRetryMs * (retry + 1),
          );
          args.signal?.addEventListener("abort", done, { once: true });
        });
        continue;
      }

      this.#log.warn(
        { method: args.method, subject: args.subject, error: message },
        "NATS request failed",
      );
      return err(classifyRequestTransportFailure({
        method: args.method,
        subject: args.subject,
        callerCapabilities: args.callerCapabilities,
        cause,
      }));
    }

    return err(
      requestFailedTransportError({
        code: "trellis.request.retry_exhausted",
        message: "Trellis could not complete the request after retrying.",
        hint:
          "Retry the request. If it keeps failing, check that the target service is available.",
        method: args.method,
        subject: args.subject,
        context: { retries: this.#noResponderMaxRetries + 1 },
      }),
    );
  }

  #requestJson(
    subject: string,
    body: JsonValue,
  ): AsyncResult<JsonValue, TransportError | UnexpectedError> {
    return AsyncResult.from((async () => {
      const span = startClientSpan(subject, subject);
      return await withSpanAsync(span, async () => {
        try {
          const payload = JSON.stringify(body);
          const response = (await this.#requestMessageWithRetry({
            subject,
            payload,
            timeout: this.timeout,
            span,
          })).take();
          if (isErr(response)) {
            span.setStatus({
              code: SpanStatusCode.ERROR,
              message: response.error.message,
            });
            recordRuntimeError(response.error, {
              surface: "operation",
              direction: "client",
              operation: "requestJson",
              phase: "request_send",
            });
            return response;
          }

          const json = safeJson(response).take();
          if (isErr(json)) {
            const error = createTransportError({
              code: "trellis.request.invalid_response",
              message: "Trellis returned an invalid response.",
              hint:
                "Retry the request. If it keeps happening, reconnect to Trellis and try again.",
              cause: json.error.cause,
              context: { subject },
            });
            span.setStatus({
              code: SpanStatusCode.ERROR,
              message: error.message,
            });
            recordRuntimeError(error, {
              surface: "operation",
              direction: "client",
              operation: "requestJson",
              phase: "response_decoding",
            });
            return err(error);
          }

          span.setStatus({ code: SpanStatusCode.OK });
          return ok(json);
        } catch (cause) {
          const error = new UnexpectedError({ cause });
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: error.message,
          });
          span.recordException(error);
          recordRuntimeError(error, {
            surface: "operation",
            direction: "client",
            operation: "requestJson",
            phase: "unexpected",
          });
          return err(error);
        } finally {
          span.end();
        }
      });
    })());
  }

  #watchJson(
    subject: string,
    body: JsonValue,
  ): AsyncResult<
    AsyncIterable<Result<JsonValue, TransportError | UnexpectedError>>,
    TransportError | UnexpectedError
  > {
    return AsyncResult.from((async () => {
      const payload = JSON.stringify(body);
      const inbox = createInbox(this.#inboxPrefix);
      const authHeaders = await this.createRequestProof(
        subject,
        payload,
        inbox,
      );

      const headers = natsHeaders();
      headers.set("proof", authHeaders.proof);
      headers.set("iat", String(authHeaders.iat));
      headers.set("request-id", authHeaders.requestId);
      headers.set("authorization-context", authHeaders.contextDigest);
      headers.set("session-key", this.#auth.sessionKey);

      const sub = this.#nats.subscribe(inbox);

      try {
        this.#nats.publish(subject, payload, {
          headers,
          reply: inbox,
        });
      } catch (cause) {
        sub.unsubscribe();
        const error = createTransportError({
          code: "trellis.watch.failed",
          message: "Trellis could not start the operation watch.",
          hint:
            "Retry watching the operation. If it keeps failing, reconnect to Trellis and try again.",
          cause,
          context: { subject },
        });
        recordRuntimeError(error, {
          surface: "operation",
          direction: "client",
          operation: "watchJson",
          phase: "request_send",
        });
        return err(error);
      }

      const iterator = sub[Symbol.asyncIterator]();
      let timer: ReturnType<typeof setTimeout> | undefined;
      let first: IteratorResult<Msg>;
      try {
        first = await Promise.race([
          iterator.next(),
          new Promise<never>((_, reject) => {
            timer = setTimeout(
              () => reject(new Error("operation watch setup timed out")),
              this.timeout,
            );
          }),
        ]);
      } catch (cause) {
        sub.unsubscribe();
        const error = createTransportError({
          code: "trellis.watch.failed",
          message: "Trellis could not start the operation watch.",
          hint:
            "Retry watching the operation. If it keeps failing, reconnect to Trellis and try again.",
          cause,
          context: { subject },
        });
        recordRuntimeError(error, {
          surface: "operation",
          direction: "client",
          operation: "watchJson",
          phase: "response_wait",
        });
        return err(error);
      } finally {
        if (timer !== undefined) clearTimeout(timer);
      }

      return ok((async function* () {
        try {
          let next = first;
          while (!next.done) {
            const msg = next.value;
            if (msg.headers?.get("status") === "error") {
              const error = createTransportError({
                code: "trellis.watch.failed",
                message: "Trellis stopped the operation watch.",
                hint:
                  "Retry watching the operation. If it keeps happening, reconnect to Trellis and try again.",
                context: { subject, frame: msg.string() },
              });
              recordRuntimeError(error, {
                surface: "operation",
                direction: "client",
                operation: "watchJson",
                phase: "remote_error",
              });
              yield err(error);
              next = await iterator.next();
              continue;
            }

            const json = safeJson(msg).take();
            if (isErr(json)) {
              const error = createTransportError({
                code: "trellis.watch.invalid_response",
                message: "Trellis returned an invalid watch update.",
                hint:
                  "Retry watching the operation. If it keeps happening, reconnect to Trellis and try again.",
                cause: json.error.cause,
                context: { subject },
              });
              recordRuntimeError(error, {
                surface: "operation",
                direction: "client",
                operation: "watchJson",
                phase: "response_decoding",
              });
              yield err(error);
              next = await iterator.next();
              continue;
            }

            yield ok(json);
            next = await iterator.next();
          }
        } finally {
          await iterator.return?.();
          sub.unsubscribe();
        }
      })());
    })());
  }
}
