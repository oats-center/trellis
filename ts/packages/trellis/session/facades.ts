import {
  type EventOpts,
  type LiveInputBuilder,
  type LiveRegistration,
  type LiveSubscribeOpts,
  type RequestOpts,
  type TrellisMode,
} from "./types.ts";
import {
  AsyncResult,
  BaseError,
  Result,
  type MaybeAsync,
} from "@oatscenter/result";
import {
  type ContractJobsFor,
  type ContractKvFor,
  type EventOf,
  type EventPayloadOf,
  type EventsOf,
  type HandlerErrorOf,
  type LiveEventOf,
  type LiveInputOf,
  type LivesOf,
  type MethodInputOf,
  type MethodOutputOf,
  type MethodsOf,
  type OperationsOf,
  type PreparedTrellisEvent,
  type RequestInputOf,
  type RequestMethodOf,
  type RequestOutputOf,
  type RpcInputOf,
  type RpcOutputOf,
  type RpcRequestShapes,
  type TrellisApiFor,
} from "./contract_types.ts";
import {
  TransportError,
  UnexpectedError,
  ValidationError,
  type StoreError,
} from "../errors/index.ts";
import { type LiveSubscription } from "../live/subscription.ts";
import { OperationInvoker } from "../operations.ts";
import { type RuntimeOperationDesc } from "./operations.ts";
import {
  type PermissionAtom as DescriptorPermissionAtom,
  type RuntimeApi,
} from "../participant_runtime/api.ts";
import { type EventListenerContext } from "./events.ts";
import { type SessionCaller } from "./local_authorization.ts";
import { type ParticipantKvMetadata } from "../participant_runtime/metadata.ts";
import { TypedKV } from "../kv.ts";
import {
  type StoreWaitOptions,
  type TypedStore,
  type TypedStoreEntry,
} from "../store.ts";
import {
  type ActiveJob,
  type JobRef,
  type JobTypeMetadata,
  type JobUpdateSubscription,
  type JobUpdatesOptions,
} from "../jobs.ts";

export type SurfaceGroups<TLeaf> = Record<string, Record<string, TLeaf>>;

export type RuntimeRpcLeaf = (
  input: unknown,
  opts?: RequestOpts,
) => AsyncResult<unknown, BaseError>;

export type RuntimeEventLeaf = {
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

export type RuntimeEventPublishLeaf = Omit<RuntimeEventLeaf, "listen">;

export type RuntimeLiveLeaf = (
  input: unknown,
  opts?: LiveSubscribeOpts,
) => AsyncResult<LiveSubscription<unknown>, BaseError>;

export type RuntimeOperationLeaf = OperationInvoker<RuntimeOperationDesc>;

export type PascalSurfaceName<T extends string> =
  T extends `${infer Head}.${infer Tail}`
    ? `${Capitalize<Head>}${PascalSurfaceName<Tail>}`
    : Capitalize<T>;

export type LowerCamelSurfaceName<T extends string> = Uncapitalize<
  PascalSurfaceName<T>
>;

export type SurfaceGroupName<T extends string> =
  T extends `${infer Head}.${string}`
    ? LowerCamelSurfaceName<Head>
    : LowerCamelSurfaceName<T>;

export type SurfaceLeafName<T extends string> =
  T extends `${string}.${infer Tail}`
    ? LowerCamelSurfaceName<Tail>
    : LowerCamelSurfaceName<T>;

export type SurfaceKeysForGroup<
  TKeys extends string,
  TGroup extends string,
> = TKeys extends string
  ? SurfaceGroupName<TKeys> extends TGroup
    ? TKeys
    : never
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

export type ActiveLiveFacade<TA extends RuntimeApi = RuntimeApi> = {
  readonly [TGroup in SurfaceGroupName<LivesOf<TA>>]: {
    readonly [
      F in SurfaceKeysForGroup<LivesOf<TA>, TGroup> as SurfaceLeafName<F>
    ]: (
      input: LiveInputOf<TA, F>,
      opts?: LiveSubscribeOpts,
    ) => AsyncResult<LiveSubscription<LiveEventOf<TA, F>>, BaseError>;
  };
};

export type ActiveOperationFacade<TA extends RuntimeApi = RuntimeApi> = {
  readonly [TGroup in SurfaceGroupName<OperationsOf<TA>>]: {
    readonly [
      O in SurfaceKeysForGroup<OperationsOf<TA>, TGroup> as SurfaceLeafName<O>
    ]: OperationInvoker<TA["operations"][O] & RuntimeOperationDesc>;
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

export type LiveSurface<
  TA extends RuntimeApi,
  TMode extends TrellisMode,
  F extends LivesOf<TA>,
> = TMode extends "service"
  ? LiveRegistration<LiveInputOf<TA, F>, LiveEventOf<TA, F>>
  : LiveInputBuilder<LiveInputOf<TA, F>, LiveEventOf<TA, F>>;

export type MaybePromise<T> = T | Promise<T>;

export type EventCallback<TMessage> = {
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

export type ProcessedRpcResponse = {
  readonly payload: string;
  readonly afterReply: readonly (() => MaybePromise<void>)[];
};

export type HandlerTrellis<
  TA extends RuntimeApi,
  TRequests = RpcRequestShapes<TA>,
> = {
  readonly rpc: ActiveRpcFacade<TA>;
  readonly event: ActiveEventPublishFacade<TA>;
  readonly live: ActiveLiveFacade<TA>;
  readonly operation: ActiveOperationFacade<TA>;
  request<const M extends RequestMethodOf<TRequests>>(
    method: M,
    input: RequestInputOf<TRequests, M>,
    opts?: RequestOpts,
  ): AsyncResult<RequestOutputOf<TRequests, M>, BaseError>;
  publish(
    event: string,
    data: Record<string, unknown>,
  ): AsyncResult<void, ValidationError | TransportError | UnexpectedError>;
  prepare(
    event: string,
    data: Record<string, unknown>,
  ): Result<PreparedTrellisEvent, ValidationError | UnexpectedError>;
  publishPrepared(
    event: PreparedTrellisEvent,
  ): AsyncResult<void, TransportError | UnexpectedError>;
  /** Stops durable event listener loops owned by this handler runtime. */
  stopEventListeners(): void;
};

export function surfaceGroupName(key: string): string {
  return lowerCamelIdent(key.split(".")[0] ?? key);
}

export function surfaceLeafName(key: string): string {
  const parts = key.split(".");
  parts.shift();
  return lowerCamelIdent(parts.length === 0 ? key : parts.join("."));
}

export function lowerCamelIdent(value: string): string {
  const pascal = value
    .split(/[^A-Za-z0-9]+/)
    .filter((part) => part.length > 0)
    .map((part) => part[0]!.toUpperCase() + part.slice(1))
    .join("");
  return pascal.length === 0 ? "_" : pascal[0]!.toLowerCase() + pascal.slice(1);
}

export function addSurfaceLeaf<TLeaf>(
  surface: SurfaceGroups<TLeaf>,
  key: string,
  leaf: TLeaf,
): void {
  const group = surfaceGroupName(key);
  surface[group] ??= {};
  surface[group][surfaceLeafName(key)] = leaf;
}

export function natsSubjectMatches(pattern: string, subject: string): boolean {
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

export type HandlerJobQueue<TPayload, TResult, TTrellis, TUpdate = never> = {
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

export type HandlerTrellisForContract<TContract> = HandlerTrellis<
  TrellisApiFor<TContract>
> & {
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
