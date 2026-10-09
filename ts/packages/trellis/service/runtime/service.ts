import { jetstream, jetstreamManager } from "@nats-io/jetstream";
import {
  headers as natsHeaders,
  type MsgHdrs,
  type NatsConnection,
  type Subscription,
} from "@nats-io/nats-core";
import type { StoreError } from "../../errors/index.ts";
import {
  installConnectionAvailability,
  observeTrellisConnection,
  type TrellisAvailability,
} from "../../connection.ts";
import {
  TransportAuthorizationState,
} from "../../auth/authorization/transport_state.ts";
import { TypedKV } from "../../kv.ts";
import {
  type StoreOperationError,
  type StoreWaitOptions,
  TypedStore,
  type TypedStoreEntry,
} from "../../store.ts";
import {
  TrellisServiceRuntime,
  type TrellisServiceRuntimeFor,
} from "./core.ts";
import {
  base64urlEncode,
  createAuth,
  type TrellisAuth as SessionAuth,
} from "../../auth.ts";
import {
  AuthorizationContextCache,
  AuthorizationContextRefreshError,
  AuthorizationProviderCache,
  startAuthorizationContextRefresh,
} from "../../auth/authorization_context.ts";
import {
  type AuthorizationCandidateSnapshot,
  installAuthorizationRefresh,
} from "../../auth/authorization/install_refresh.ts";
import { TrellisHttpError } from "../../auth/http_error.ts";
import { transportAuthorizationDigestWasm } from "../../auth/protocol_wasm.ts";
import {
  fixedTransportProvider,
  TransportGenerationManager,
  type TransportGenerationPrepared,
  type TransportLease,
  type TrellisTransportProvider,
} from "../../transport/generations.ts";
import type { InferSchemaType } from "../../participant.ts";
import type {
  PermissionAtom,
  RuntimeApi,
} from "../../participant_runtime/api.ts";
import type {
  ParticipantJobsMetadata,
  ParticipantKvMetadata,
} from "../../participant_runtime/metadata.ts";
import type { ContractEventConsumers } from "../../participant_runtime/schemas.ts";
import {
  bindApiRoutes,
  type GeneratedParticipant,
  getParticipantRuntime,
  participantAvailability,
  participantEvidence,
  refreshApiRoutes,
} from "../../participant_runtime/participant.ts";
import {
  type ConnectedActionName,
  lowerCamelSurfaceName,
} from "../../participant_runtime/surface_names.ts";
import {
  AsyncResult,
  BaseError,
  isErr,
  type MaybeAsync,
  Result,
} from "@oatscenter/result";
import { Value } from "typebox/value";
import {
  type ServiceHealth,
  type ServiceHealthCheckFn,
  type ServiceHealthInfoFn,
  ServiceHealthRuntime,
} from "./health.ts";
import { publishHealthHeartbeatSample } from "../../health_transport.ts";
import type { EventDesc } from "../../participant.ts";
import type {
  ActiveEventFacade,
  ActiveEventPublishFacade,
  EventListenerContext,
  EventOpts,
  HandlerTrellis,
  LiveEventOf,
  LiveHandlerContext,
  LiveInputOf,
  LiveRegistration as RootLiveRegistration,
  LivesOf,
  OperationHandlerContext,
  OperationHandlerErrorOf,
  OperationOutputOf,
  OperationProgressOf,
  OperationRegistration as RootOperationRegistration,
  OperationTransferContextOf,
  OperationUpdateOf,
  PreparedTrellisEvent,
  ProviderGenerationAttachment,
  RpcHandlerContext,
  RpcHandlerErrorOf,
  TrellisOpts,
} from "../../session.ts";
import {
  annotateHandlerBoundaryError,
  createTrellisInternal,
} from "../../session.ts";
import type { TrellisServiceRuntimeDeps } from "./runtime.ts";
import { ServiceTransfer } from "./transfer.ts";
import { logger as noopLogger, type LoggerLike } from "../../globals.ts";
import {
  createProviderRuntime,
  PROVIDER_CALLER,
  type ProviderCaller,
  type ProviderHandlerClient,
  type ProviderRuntime,
} from "../../provider.ts";
import {
  DEFAULT_SERVICE_RUNTIME_WAIT_ON_FIRST_CONNECT,
  GENERATION_MAX_RECONNECT_ATTEMPTS,
  selectRuntimeTransportServers,
} from "../../runtime_transport.ts";
import { serviceRuntimeLogger } from "./logger.ts";
import {
  type TransferError,
  TransportError,
  UnexpectedError,
  ValidationError,
} from "../../errors/index.ts";
import type { ReceiveTransferGrant } from "../../transfer.ts";
import {
  ActiveJob as PublicActiveJob,
  decodeJobUpdateEnvelope,
  type JobHandlerOptions,
  type JobLogEntry,
  JobNotEnqueuedError,
  type JobProgress,
  JobRef,
  type JobSnapshot,
  type JobSubmitOutcome,
  type JobUpdatesOptions,
  type JobUpdateSubscription,
  JobWorkerHostAdapter,
  RetryJobError,
  type TerminalJob,
} from "../../jobs.ts";
import { parseSchema } from "../../codec.ts";
import type { Codec } from "../../generated.ts";
import { isJsonValue } from "../../participant_runtime/json.ts";
import { ulid } from "ulid";
import {
  JobManager as InternalJobManager,
  JobProcessError as InternalJobProcessError,
  prepareJobSubmission,
} from "./internal_jobs/job-manager.ts";
import {
  getLatestLifecycleEvent,
  type JobReceivingSession,
  startNatsWorkerHostFromBinding,
  type StartNatsWorkerHostOptions,
} from "./internal_jobs/runtime-worker.ts";
import { startManagedJobWorkerHost } from "./internal_jobs/managed-worker.ts";
import { ActiveJobCancellationRegistry } from "./internal_jobs/cancellation-registry.ts";
import {
  createNatsJobKeyCoordinator,
  normalizeJobKeyPolicy,
} from "./internal_jobs/key-coordinator.ts";
import type {
  JobKeyConcurrencyBinding,
  JobQueuePolicyBinding,
} from "./internal_jobs/key-coordinator.ts";
import type {
  JobsBinding,
  JobsQueueBinding,
} from "./internal_jobs/bindings.ts";
import type { ActiveJob as InternalActiveJob } from "./internal_jobs/active-job.ts";
import {
  type JobContext as InternalJobContext,
  type JobEvent as InternalJobEvent,
  JobEventSchema,
  type PreparedJobSubmission as InternalPreparedJobSubmission,
  PreparedJobSubmissionSchema,
} from "./internal_jobs/types.ts";
import {
  observeNatsTrellisConnection,
  startConnectionTelemetry,
  transitionConnectionAvailability,
  type TrellisConnection,
} from "../../connection.ts";
import { recordTrellisDuration } from "../../telemetry/mod.ts";
import {
  defaultSqlOutboxTables,
  OutboxDispatcher,
  type OutboxDispatcherOptions,
  type OutboxDispatchRuntime,
  type OutboxJobDispatchOutcome,
  type OutboxMessage,
  type PreparedOutboxRecord,
  preparedTrellisEventToOutboxRecord,
  type SqlDialect,
  type SqlExecutor,
  SqlOutboxRepository,
  type SqlOutboxTables,
} from "../outbox_inbox.ts";
import {
  closeFailedServiceBootstrapConnection,
  fetchServiceBootstrapInfo,
  loadDefaultServiceRuntimeDeps,
} from "./bootstrap.ts";

type ResourceBindingJobsQueue = {
  queueType: string;
  publishPrefix: string;
  workSubject: string;
  consumerName: string;
  payload: { schema: string };
  update?: { schema: string };
  updatesPrefix?: string;
  result?: { schema: string };
  maxDeliver: number;
  backoffMs: number[];
  ackWaitMs: number;
  defaultDeadlineMs?: number;
  keyConcurrency?: JobKeyConcurrencyBinding;
  queue?: JobQueuePolicyBinding;
};

type ResourceBindingJobs = {
  serviceName: string;
  namespace: string;
  workStream?: string;
  queues: Record<string, ResourceBindingJobsQueue>;
};

function normalizeResourceJobsBinding(
  binding: ResourceBindingJobs,
): JobsBinding {
  const queues: Record<string, JobsQueueBinding> = {};
  for (const [name, queue] of Object.entries(binding.queues)) {
    const baseQueue = baseJobsQueueBinding(queue);
    if (!queue.keyConcurrency) {
      queues[name] = {
        ...baseQueue,
        ...(queue.queue ? { queue: normalizeQueuePolicy(queue.queue) } : {}),
      };
      continue;
    }

    const policy = normalizeJobKeyPolicy({
      keyConcurrency: queue.keyConcurrency,
      queue: queue.queue,
    });
    queues[name] = {
      ...baseQueue,
      keyConcurrency: {
        key: policy.key,
        maxActive: policy.maxActive,
        heartbeatIntervalMs: policy.heartbeatIntervalMs,
        heartbeatTtlMs: policy.heartbeatTtlMs,
        stalePolicy: policy.stalePolicy,
      },
      queue: policy.queue,
    };
  }
  return {
    serviceName: binding.serviceName,
    namespace: binding.namespace,
    queues,
  };
}

function baseJobsQueueBinding(
  queue: ResourceBindingJobsQueue,
): Omit<JobsQueueBinding, "keyConcurrency" | "queue"> {
  return {
    queueType: queue.queueType,
    publishPrefix: queue.publishPrefix,
    workSubject: queue.workSubject,
    consumerName: queue.consumerName,
    payload: queue.payload,
    ...(queue.update ? { update: queue.update } : {}),
    ...(queue.updatesPrefix ? { updatesPrefix: queue.updatesPrefix } : {}),
    ...(queue.result ? { result: queue.result } : {}),
    maxDeliver: queue.maxDeliver ?? 5,
    backoffMs: queue.backoffMs ?? [5_000, 30_000, 120_000, 600_000],
    ackWaitMs: queue.ackWaitMs ?? 5_000,
    ...(queue.defaultDeadlineMs !== undefined
      ? { defaultDeadlineMs: queue.defaultDeadlineMs }
      : {}),
  };
}

function normalizeQueuePolicy(
  queue: JobQueuePolicyBinding,
): JobsQueueBinding["queue"] {
  return {
    maxQueuedPerKey: queue.maxQueuedPerKey ?? 0,
    whenFull: queue.whenFull ?? "reject",
  };
}

type ResourceBindingEventConsumer = {
  stream: string;
  consumerName: string;
  resourceId: string;
  filterSubjects: string[];
  replay: "new" | "all";
  concurrency: number;
  ackWaitMs: number;
  maxDeliver: number;
  backoffMs: number[];
  replayBinding: { stream: string; consumerName: string };
};

type RpcMethodName<TA extends RuntimeApi> = keyof TA["rpc"] & string;
type RpcMethodInput<TA extends RuntimeApi, M extends RpcMethodName<TA>> =
  InferSchemaType<TA["rpc"][M]["input"]>;
type RpcMethodOutput<TA extends RuntimeApi, M extends RpcMethodName<TA>> =
  InferSchemaType<TA["rpc"][M]["output"]>;
type TrellisServiceRuntimeCreateOpts<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi | undefined = TOwnedApi,
> = {
  requestLimits?: TrellisOpts<RuntimeApi>["requestLimits"];
  log?: LoggerLike | false;
  timeout?: number;
  stream?: string;
  noResponderRetry?: { maxAttempts?: number; baseDelayMs?: number };
  api: TOwnedApi;
  trellisApi?: TTrellisApi;
  version?: string;
  health?: TrellisServiceHealthOpts;
};

export type TrellisServiceHealthOpts = {
  publishIntervalMs?: number;
};

export type TrellisServiceRuntimeOpts = {
  /** Maximum ordinary verification workers, including workers starting. Defaults to three; reserved verification uses one additional worker. Starts with one ordinary worker and retains added capacity until shutdown. */
  maxVerificationWorkers?: number;
  /** Minimum age of the oldest queued ordinary proof before sustained-backlog timing begins. Defaults to 20 milliseconds; zero permits immediate backlog timing. */
  verificationWorkerQueueAgeMs?: number;
  /** How long all ordinary workers must remain occupied with an eligible queued proof before growing. Defaults to 100 milliseconds; zero permits growth as soon as the queue-age threshold is met. */
  verificationWorkerBacklogDurationMs?: number;
  /** Bound local provider dispatch across all endpoints and transport generations. */
  requestLimits?: TrellisOpts<RuntimeApi>["requestLimits"];
  log?: LoggerLike | false;
  timeout?: number;
  stream?: string;
  noResponderRetry?: { maxAttempts?: number; baseDelayMs?: number };
  version?: string;
  health?: TrellisServiceHealthOpts;
};

function resolveServiceLogger(log?: LoggerLike | false): LoggerLike {
  return log === false ? noopLogger : log ?? serviceRuntimeLogger;
}

function surfaceGroupName(key: string): string {
  return lowerCamelIdent(key.split(".")[0] ?? key);
}

function surfaceLeafName(key: string): string {
  const parts = key.split(".");
  parts.shift();
  return lowerCamelIdent(parts.length === 0 ? key : parts.join("."));
}

function lowerCamelIdent(value: string): string {
  return lowerCamelSurfaceName(value) || "_";
}

function addSurfaceLeaf<TLeaf>(
  surface: Record<string, Record<string, TLeaf>>,
  key: string,
  leaf: TLeaf,
): void {
  const group = surfaceGroupName(key);
  surface[group] ??= {};
  surface[group][surfaceLeafName(key)] = leaf;
}

export type ResourceBindingKV = {
  bucket: string;
  history: number;
  ttlMs: number;
  maxValueBytes?: number;
};

export type ResourceBindingStore = {
  name: string;
  ttlMs: number;
  maxObjectBytes?: number;
  maxTotalBytes?: number;
};

export type ResourceBindings = {
  kv: Record<string, ResourceBindingKV>;
  store: Record<string, ResourceBindingStore>;
  jobs?: ResourceBindingJobs;
  eventConsumers?: Record<string, ResourceBindingEventConsumer>;
};

const storeHandleConstructorToken: unique symbol = Symbol(
  "StoreHandle.constructorToken",
);

const trellisServiceConstructorToken: unique symbol = Symbol(
  "TrellisService.constructorToken",
);

/**
 * Internal provider-ingress handle carried on a connected service session.
 *
 * Lets the service runtime install and retire per-generation provider intake
 * without widening the public `TrellisServiceSession` surface. @internal
 */
export const SERVICE_PROVIDER_INGRESS: unique symbol = Symbol(
  "trellis.service.providerIngress",
);

/**
 * Logical transport lifetime of a connected service.
 *
 * Installed after the generation manager exists. Its `closed()` resolves when
 * the logical connection ends (explicit close or an authoritative terminal
 * failure), not when one physical generation is reaped. @internal
 */
export const SERVICE_TRANSPORT_LIFETIME: unique symbol = Symbol(
  "trellis.service.transportLifetime",
);

/** Logical transport lifetime surface used by the service wait loop. @internal */
type ServiceTransportLifetime = {
  closed(): Promise<void | Error>;
};

/** @internal */
export type ServiceProviderIngress = {
  install(target: {
    id: number;
    nc: NatsConnection;
    contextDigest: string;
    lease?: () => TransportLease;
    /**
     * Push each owning ingress's concrete cleanup scope into the manager before
     * async work, so cleanup is never a fallible late lookup. @internal
     */
    registerDisposal?: (handle: ProviderGenerationAttachment) => boolean;
  }): Promise<void>;
  /** Stop generic intake on a superseded generation. */
  retireIntake(id: number): void;
};

/** One Trellis surface that owns per-generation provider ingress. @internal */
type ProviderIngressOwner = {
  installProviderIngress(
    target: Parameters<ServiceProviderIngress["install"]>[0],
  ): Promise<ProviderGenerationAttachment>;
  retireProviderIngress(id: number): void;
};

export abstract class StoreHandle {
  abstract readonly binding: ResourceBindingStore;

  abstract open(): AsyncResult<TypedStore, StoreError>;

  /**
   * Waits for a staged object to appear in the bound store and returns its entry.
   */
  abstract waitFor(
    key: string,
    options?: StoreWaitOptions,
  ): AsyncResult<TypedStoreEntry, StoreOperationError>;
}

class InternalStoreHandle extends StoreHandle {
  readonly binding: ResourceBindingStore;
  readonly #transport: TrellisTransportProvider;
  readonly #acquireTimeoutMs: number;

  constructor(
    transport: TrellisTransportProvider,
    acquireTimeoutMs: number,
    binding: ResourceBindingStore,
    token: typeof storeHandleConstructorToken,
  ) {
    super();
    if (token !== storeHandleConstructorToken) {
      throw new TypeError(
        "StoreHandle instances are created by TrellisService",
      );
    }
    this.#transport = transport;
    this.#acquireTimeoutMs = acquireTimeoutMs;
    this.binding = binding;
  }

  open(): AsyncResult<TypedStore, StoreError> {
    return TypedStore.open(this.#transport, this.binding.name, {
      ttlMs: this.binding.ttlMs,
      maxObjectBytes: this.binding.maxObjectBytes,
      maxTotalBytes: this.binding.maxTotalBytes,
      bindOnly: true,
      acquireTimeoutMs: this.#acquireTimeoutMs,
    });
  }

  /**
   * Waits for a staged object to appear in the bound store and returns its entry.
   */
  waitFor(
    key: string,
    options: StoreWaitOptions = {},
  ): AsyncResult<TypedStoreEntry, StoreOperationError> {
    return this.open().andThen((store) => store.waitFor(key, options));
  }
}

function openServiceKvBindings<TKv extends ParticipantKvMetadata>(args: {
  /** Logical transport provider the handle acquires a generation from. */
  transport: TrellisTransportProvider;
  /** Budget for acquiring a generation on a finite resource operation. */
  acquireTimeoutMs: number;
  /**
   * Current resource bindings, re-read on every access so a resource that
   * materializes after connect becomes usable on the same attachment.
   */
  bindings: () => Readonly<Record<string, ResourceBindingKV>>;
  contractKv: TKv;
}): ServiceKvFacade<TKv> {
  const handles = new Map<
    string,
    { bucket: string; handle: TypedKV<unknown> }
  >();
  const facade: Record<string, unknown> = {};
  for (const [alias, metadata] of Object.entries(args.contractKv)) {
    Object.defineProperty(facade, alias, {
      enumerable: true,
      get: () => {
        const binding = args.bindings()[alias];
        if (!binding) {
          if (!metadata.required) return undefined;
          throw new Error(`Required KV binding '${alias}' is unavailable`);
        }
        const cached = handles.get(alias);
        if (cached && cached.bucket === binding.bucket) return cached.handle;
        const handle = TypedKV.bind(
          args.transport,
          binding.bucket,
          metadata.schema,
          {
            history: binding.history,
            ttl: binding.ttlMs,
            maxValueBytes: binding.maxValueBytes,
            bindOnly: true,
            isCurrent: () => args.bindings()[alias]?.bucket === binding.bucket,
            acquireTimeoutMs: args.acquireTimeoutMs,
          },
        ) as TypedKV<unknown>;
        handles.set(alias, { bucket: binding.bucket, handle });
        return handle;
      },
    });
  }
  return facade as ServiceKvFacade<TKv>;
}

export type TrellisServiceConnectOpts<
  TOwnedApi extends RuntimeApi = RuntimeApi,
  TTrellisApi extends RuntimeApi | undefined = TOwnedApi,
> = {
  trellisUrl: string;
  participant: GeneratedServiceParticipant<TOwnedApi, TTrellisApi>;
  name?: string;
  /** Immutable provisioned service identity. */
  seed: string;
  /** Controls automatic telemetry initialization. Enabled by default. */
  telemetry?: TrellisServiceConnectTelemetryOpts;
  /** Configures the connected service runtime. */
  runtime?: TrellisServiceRuntimeOpts;
};

/** Controls automatic telemetry initialization for `TrellisService.connect()`. */
export type TrellisServiceConnectTelemetryOpts = false | {
  /** Whether automatic telemetry initialization is enabled. Defaults to `true`. */
  enabled?: boolean;
};

type ServiceKvFacade<TKv extends ParticipantKvMetadata> = {
  [K in keyof TKv]: TKv[K]["required"] extends false
    ? TypedKV<TKv[K]["value"]> | undefined
    : TypedKV<TKv[K]["value"]>;
};

type ServiceHandlerResources<
  TKv extends ParticipantKvMetadata,
  TJobs extends ParticipantJobsMetadata,
  TTrellisApi extends RuntimeApi,
> = {
  kv: ServiceKvFacade<TKv>;
  store: Record<string, StoreHandle>;
  jobs: ServiceJobsFacadeOf<TJobs, Trellis<TTrellisApi, TKv, TJobs>>;
};

export type Trellis<
  TTrellisApi extends RuntimeApi,
  TKv extends ParticipantKvMetadata = ParticipantKvMetadata,
  TJobs extends ParticipantJobsMetadata = ParticipantJobsMetadata,
> =
  & HandlerTrellis<TTrellisApi>
  & ServiceHandlerResources<TKv, TJobs, TTrellisApi>;

export type GeneratedServiceParticipant<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi | undefined,
  TJobs extends ParticipantJobsMetadata = ParticipantJobsMetadata,
  TKv extends ParticipantKvMetadata = ParticipantKvMetadata,
> = GeneratedParticipant & {
  readonly __runtimeTypes?: {
    ownedApi: TOwnedApi;
    api: TTrellisApi;
    jobs: TJobs;
    kv: TKv;
  };
};

type ParticipantOwnedApi<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata
  >,
> = TContract extends {
  readonly __runtimeTypes?: { ownedApi: infer TOwnedApi };
} ? Extract<TOwnedApi, RuntimeApi>
  : RuntimeApi;

type ParticipantTrellisApi<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata
  >,
> = TContract extends {
  readonly __runtimeTypes?: { api: infer TApi };
} ? Extract<TApi, RuntimeApi>
  : ParticipantOwnedApi<TContract>;

type ParticipantJobsOf<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
> = TContract extends { readonly __runtimeTypes?: { jobs: infer TJobs } }
  ? Extract<TJobs, ParticipantJobsMetadata>
  : Record<string, never>;

type ParticipantKvOf<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
> = TContract extends { readonly __runtimeTypes?: { kv: infer TKv } }
  ? Extract<TKv, ParticipantKvMetadata>
  : Record<string, never>;

/**
 * Flat, contract-selected client supplied to every public service callback.
 *
 * It exposes only selected outbound actions plus owned publishers, the bound
 * KV/Store resources, queues with the handler callback rebound to this client,
 * and the connection/availability utilities. Registration and lifecycle
 * methods live on the connected service instead.
 */
export type ServiceHandlerClient<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
> = ProviderHandlerClient<
  TContract,
  TrellisServiceSession<
    ParticipantOwnedApi<TContract>,
    ParticipantTrellisApi<TContract>,
    ParticipantJobsOf<TContract>,
    ParticipantKvOf<TContract>
  >
>;

type ParticipantEventName<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata
  >,
> = ServiceEventName<ParticipantTrellisApi<TContract>>;

type ContractOperationName<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata
  >,
> = keyof ParticipantOwnedApi<TContract>["operations"] & string;

type ContractLiveName<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata
  >,
> = LivesOf<ParticipantOwnedApi<TContract>>;

type ContractJobName<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
> = keyof ParticipantJobsOf<TContract> & string;

/** Typed RPC handler function for an extracted Trellis service handler. */
export type RpcHandler<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
  M extends RpcMethodName<ParticipantOwnedApi<TContract>>,
> = ({
  input,
  context,
  client,
}: {
  input: RpcMethodInput<ParticipantOwnedApi<TContract>, M>;
  context: RpcHandlerContext;
  client: ServiceHandlerClient<TContract>;
}) =>
  | Promise<
    Result<
      RpcMethodOutput<ParticipantOwnedApi<TContract>, M>,
      RpcHandlerErrorOf<ParticipantOwnedApi<TContract>, M>
    >
  >
  | Result<
    RpcMethodOutput<ParticipantOwnedApi<TContract>, M>,
    RpcHandlerErrorOf<ParticipantOwnedApi<TContract>, M>
  >;

/** Typed event listener function for an extracted Trellis service listener. */
export type ServiceEventHandler<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
  E extends ParticipantEventName<TContract>,
> = (
  args: {
    event: ServiceEventOf<ParticipantTrellisApi<TContract>, E>;
    context: EventListenerContext;
    client: ServiceHandlerClient<TContract>;
  },
) => MaybeAsync<void, BaseError>;

/** Typed operation handler function for an extracted Trellis service handler. */
export type OperationHandler<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
  O extends ContractOperationName<TContract>,
> = (
  args:
    & OperationHandlerContext<
      InferSchemaType<ParticipantOwnedApi<TContract>["operations"][O]["input"]>,
      OperationProgressOf<ParticipantOwnedApi<TContract>, O>,
      OperationOutputOf<ParticipantOwnedApi<TContract>, O>,
      OperationTransferContextOf<ParticipantOwnedApi<TContract>, O>,
      OperationHandlerErrorOf<ParticipantOwnedApi<TContract>, O>,
      OperationUpdateOf<ParticipantOwnedApi<TContract>, O>
    >
    & {
      client: ServiceHandlerClient<TContract>;
    },
) => unknown | Promise<unknown>;

/** Typed live handler function for an extracted Trellis service handler. */
export type LiveHandler<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
  F extends ContractLiveName<TContract>,
> = (
  context:
    & LiveHandlerContext<
      LiveInputOf<ParticipantOwnedApi<TContract>, F>,
      LiveEventOf<ParticipantOwnedApi<TContract>, F>
    >
    & { client: ServiceHandlerClient<TContract> },
) => unknown | Promise<unknown>;

/** Typed job handler function for an extracted Trellis service job. */
export type JobHandler<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
  K extends ContractJobName<TContract>,
> = (args: {
  job: PublicActiveJob<
    ParticipantJobsOf<TContract>[K]["payload"],
    ParticipantJobsOf<TContract>[K]["result"],
    ParticipantJobsOf<TContract>[K]["update"]
  >;
  client: ServiceHandlerClient<TContract>;
}) => Promise<Result<ParticipantJobsOf<TContract>[K]["result"], BaseError>>;

/** Typed health info function for an extracted service health handler. */
export type HealthInfoHandler = ServiceHealthInfoFn;

/** Typed health check function for an extracted service health handler. */
export type HealthCheckHandler = ServiceHealthCheckFn;

/**
 * One service job queue whose worker callback receives `TClient`.
 */
export type ServiceJobQueue<TPayload, TResult, TUpdate, TClient> = {
  create(
    payload: TPayload,
  ): AsyncResult<JobRef<TPayload, TResult, TUpdate>, BaseError>;
  submit(
    payload: TPayload,
  ): AsyncResult<JobSubmitOutcome<TPayload, TResult, TUpdate>, BaseError>;
  updates(
    jobId: string,
    options?: JobUpdatesOptions,
  ): AsyncResult<JobUpdateSubscription<TUpdate>, BaseError>;
  handle(
    handler: (args: {
      job: PublicActiveJob<TPayload, TResult, TUpdate>;
      client: TClient;
    }) => Promise<Result<TResult, BaseError>>,
    options?: JobHandlerOptions,
  ): void;
};

/**
 * Jobs facade that rebinds every queue's worker callback to `TClient`.
 */
export type ServiceJobsFacadeOf<
  TJobs extends ParticipantJobsMetadata,
  TClient,
> = {
  [K in keyof TJobs]: ServiceJobQueue<
    TJobs[K]["payload"],
    TJobs[K]["result"],
    TJobs[K]["update"],
    TClient
  >;
};

type ServiceEventName<TA extends RuntimeApi> = keyof TA["events"] & string;
type ServiceEventOf<
  TA extends RuntimeApi,
  E extends ServiceEventName<TA>,
> = TA["events"][E] extends EventDesc<infer TEvent> ? InferSchemaType<TEvent>
  : never;
type ServiceEventPayloadOf<
  TA extends RuntimeApi,
  E extends ServiceEventName<TA>,
> = ServiceEventOf<TA, E> & Record<string, unknown>;

/** Runs SQL outbox work inside a caller-owned service database transaction. */
export type SqlOutboxTransactionRunner<TTx> = <TResult>(
  work: (context: { tx: TTx; executor: SqlExecutor }) =>
    | Promise<TResult>
    | TResult,
) => Promise<TResult>;

/** Options shared by all Trellis SQL outbox service bindings. */
export type TrellisServiceSqlOutboxCommonOptions = {
  /** SQL dialect used by the Trellis helper tables. */
  readonly dialect: SqlDialect;
  /** Optional Trellis helper-table names; omitted names use Trellis defaults. */
  readonly tables?: Partial<SqlOutboxTables>;
  /** Optional process-local dispatcher tuning. */
  readonly dispatcher?: OutboxDispatcherOptions;
};

/** Options for binding a Trellis service to generic caller-owned SQL storage. */
export type TrellisServiceSqlOutboxExecutorOptions<TTx> =
  & TrellisServiceSqlOutboxCommonOptions
  & {
    /** Non-transactional executor used by the process-local dispatcher. */
    readonly executor: SqlExecutor;
    /** Service-owned transaction runner for handler-scoped work. */
    readonly transaction: SqlOutboxTransactionRunner<TTx>;
  };

/** Options for binding a Trellis service to caller-owned SQL outbox storage. */
export type TrellisServiceSqlOutboxOptions<TTx> =
  TrellisServiceSqlOutboxExecutorOptions<TTx>;

/** Typed transaction-scoped event facade that enqueues prepared events. */
export type SqlOutboxEventEnqueueFacade<
  TEventApi extends RuntimeApi = RuntimeApi,
> = {
  readonly [TGroup in SurfaceGroupName<ServiceEventName<TEventApi>>]: {
    readonly [
      E in SurfaceKeysForGroup<
        ServiceEventName<TEventApi>,
        TGroup
      > as SurfaceLeafName<E>
    ]: {
      enqueue(
        event: ServiceEventPayloadOf<TEventApi, E>,
      ): AsyncResult<OutboxMessage, ValidationError | UnexpectedError>;
    };
  };
};

/** Returned by outbox.job.<queue>.create/submit inside a transaction. */
export type SqlOutboxJobSubmission = {
  readonly submissionId: string;
  readonly jobId: string;
  readonly queue: string;
  readonly mode: "create" | "submit";
};

/** Transaction-scoped facade for enqueuing job creation/submission intents. */
export type SqlOutboxJobEnqueueFacade<TJobs extends ParticipantJobsMetadata> = {
  readonly [K in keyof TJobs]: {
    create(
      payload: TJobs[K]["payload"],
    ): AsyncResult<SqlOutboxJobSubmission, ValidationError | UnexpectedError>;
    submit(
      payload: TJobs[K]["payload"],
    ): AsyncResult<SqlOutboxJobSubmission, ValidationError | UnexpectedError>;
  };
};

/** Context supplied to `outbox.transaction(...)` work callbacks. */
export type SqlOutboxTransactionContext<
  TTx,
  TEventApi extends RuntimeApi = RuntimeApi,
  TJobs extends ParticipantJobsMetadata = ParticipantJobsMetadata,
> = {
  /** Service-owned transaction object supplied by the configured runner. */
  readonly tx: TTx;
  /** Transaction-scoped typed event enqueue facade. */
  readonly event: SqlOutboxEventEnqueueFacade<TEventApi>;
  /** Transaction-scoped typed job enqueue facade. */
  readonly job: SqlOutboxJobEnqueueFacade<TJobs>;
};

/** Startup-created SQL outbox dependency for transactional event/job enqueue. */
export type SqlOutbox<
  TTx,
  TEventApi extends RuntimeApi = RuntimeApi,
  TJobs extends ParticipantJobsMetadata = ParticipantJobsMetadata,
> = {
  /**
   * Runs service DB work and typed event/job enqueue operations in one SQL
   * transaction, notifying the dispatcher once after a successful commit.
   */
  transaction<TResult>(
    work: (context: SqlOutboxTransactionContext<TTx, TEventApi, TJobs>) =>
      | Promise<TResult>
      | TResult,
  ): AsyncResult<TResult, ValidationError | UnexpectedError>;
  /** Returns the durable queue-admission outcome once a job submission dispatches. */
  jobSubmissionOutcome(
    submissionId: string,
  ): AsyncResult<OutboxJobDispatchOutcome | undefined, UnexpectedError>;
};

const MANAGED_JOB_WORKERS = Symbol("trellis.managedJobWorkers");

type ManagedJobWorkers = {
  start(): AsyncResult<JobWorkerHostAdapter, BaseError>;
  stop(): AsyncResult<void, BaseError>;
};

type ManagedJobsFacade<
  TJobs extends ParticipantJobsMetadata,
  TClient,
> = ServiceJobsFacadeOf<TJobs, TClient> & {
  [MANAGED_JOB_WORKERS]: ManagedJobWorkers;
};

type ServiceHandleOperationLeaf =
  & ((handler: (context: unknown) => unknown) => Promise<void>)
  & Pick<
    RootOperationRegistration<unknown, unknown, unknown, undefined, BaseError>,
    "control" | "reconcile"
  >;

type ServiceHandleFacade = {
  readonly rpc: Record<
    string,
    Record<string, (handler: (args: unknown) => unknown) => Promise<void>>
  >;
  readonly live: Record<
    string,
    Record<string, (handler: (args: unknown) => unknown) => Promise<void>>
  >;
  readonly operation: Record<
    string,
    Record<string, ServiceHandleOperationLeaf>
  >;
};

type ServiceEventPublishLeaf = {
  prepare(
    event: Record<string, unknown>,
  ): ReturnType<HandlerTrellis<RuntimeApi>["prepare"]>;
  publish(
    event: Record<string, unknown>,
  ): ReturnType<HandlerTrellis<RuntimeApi>["publish"]>;
};

type ServiceEventLeaf = ServiceEventPublishLeaf & {
  listen(
    handler: (
      event: unknown,
      context: EventListenerContext,
    ) => MaybeAsync<void, BaseError>,
    subjectData?: Record<string, unknown>,
    opts?: EventOpts,
  ): AsyncResult<void, ValidationError | UnexpectedError>;
};

function createServiceEventPublishFacade<TA extends RuntimeApi>(outbound: {
  readonly api: TA;
  prepare(
    event: string,
    data: Record<string, unknown>,
  ): ReturnType<HandlerTrellis<TA>["prepare"]>;
  publish(
    event: string,
    data: Record<string, unknown>,
  ): ReturnType<HandlerTrellis<TA>["publish"]>;
}): ActiveEventPublishFacade<TA> {
  const surface: Record<string, Record<string, ServiceEventPublishLeaf>> = {};
  for (const event of Object.keys(outbound.api.events ?? {})) {
    addSurfaceLeaf(surface, event, {
      prepare: (payload) => outbound.prepare(event, payload),
      publish: (payload) => outbound.publish(event, payload),
    });
  }
  return surface as ActiveEventPublishFacade<TA>;
}

type SurfaceGroupName<T extends string> = T extends `${infer Head}.${string}`
  ? ConnectedActionName<Head>
  : ConnectedActionName<T>;
type SurfaceLeafName<T extends string> = T extends `${string}.${infer Tail}`
  ? ConnectedActionName<Tail>
  : ConnectedActionName<T>;
type SurfaceKeysForGroup<TKeys extends string, TGroup extends string> =
  TKeys extends string ? SurfaceGroupName<TKeys> extends TGroup ? TKeys : never
    : never;

type TypedServiceHandleFacade<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi,
  TKv extends ParticipantKvMetadata,
  TJobs extends ParticipantJobsMetadata,
> = {
  readonly rpc: {
    readonly [TGroup in SurfaceGroupName<RpcMethodName<TOwnedApi>>]: {
      readonly [
        M in SurfaceKeysForGroup<
          RpcMethodName<TOwnedApi>,
          TGroup
        > as SurfaceLeafName<M>
      ]: (
        handler: RpcHandleFn<TOwnedApi, TTrellisApi, M, TKv, TJobs>,
      ) => Promise<void>;
    };
  };
  readonly live: {
    readonly [TGroup in SurfaceGroupName<keyof TOwnedApi["lives"] & string>]: {
      readonly [
        F in SurfaceKeysForGroup<
          keyof TOwnedApi["lives"] & string,
          TGroup
        > as SurfaceLeafName<F>
      ]: (
        handler: LiveHandleFn<TOwnedApi, TTrellisApi, F, TKv, TJobs>,
      ) => Promise<void>;
    };
  };
  readonly operation: {
    readonly [
      TGroup in SurfaceGroupName<keyof TOwnedApi["operations"] & string>
    ]: {
      readonly [
        O in SurfaceKeysForGroup<
          keyof TOwnedApi["operations"] & string,
          TGroup
        > as SurfaceLeafName<O>
      ]: OperationHandleFn<
        TOwnedApi,
        TTrellisApi,
        O,
        TKv,
        TJobs
      >;
    };
  };
};

type RpcHandleFn<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi,
  M extends RpcMethodName<TOwnedApi>,
  TKv extends ParticipantKvMetadata,
  TJobs extends ParticipantJobsMetadata,
> = (args: {
  input: RpcMethodInput<TOwnedApi, M>;
  context: RpcHandlerContext;
  client: Trellis<TTrellisApi, TKv, TJobs>;
}) =>
  | Promise<
    Result<RpcMethodOutput<TOwnedApi, M>, RpcHandlerErrorOf<TOwnedApi, M>>
  >
  | Result<RpcMethodOutput<TOwnedApi, M>, RpcHandlerErrorOf<TOwnedApi, M>>;

type LiveHandleFn<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi,
  F extends keyof TOwnedApi["lives"] & string,
  TKv extends ParticipantKvMetadata,
  TJobs extends ParticipantJobsMetadata,
> = (context: {
  input: LiveInputOf<TOwnedApi, F>;
  caller: unknown;
  signal: AbortSignal;
  emit(
    event: LiveEventOf<TOwnedApi, F>,
  ): AsyncResult<void, ValidationError | UnexpectedError>;
  client: Trellis<TTrellisApi, TKv, TJobs>;
}) => unknown | Promise<unknown>;

type OperationHandleFn<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi,
  O extends keyof TOwnedApi["operations"] & string,
  TKv extends ParticipantKvMetadata,
  TJobs extends ParticipantJobsMetadata,
> =
  & OperationControlRegistration<TOwnedApi, O>
  & ((
    handler: (
      context:
        & OperationHandlerContext<
          InferSchemaType<TOwnedApi["operations"][O]["input"]>,
          OperationProgressOf<TOwnedApi, O>,
          OperationOutputOf<TOwnedApi, O>,
          OperationTransferContextOf<TOwnedApi, O>,
          OperationHandlerErrorOf<TOwnedApi, O>,
          OperationUpdateOf<TOwnedApi, O>
        >
        & { client: Trellis<TTrellisApi, TKv, TJobs> },
    ) => unknown | Promise<unknown>,
  ) => Promise<void>);

/** Owner-fenced control and owner-routed reconciliation for a service operation. */
export type OperationControlRegistration<
  TOwnedApi extends RuntimeApi,
  O extends keyof TOwnedApi["operations"] & string,
> = Pick<
  RootOperationRegistration<
    InferSchemaType<TOwnedApi["operations"][O]["input"]>,
    OperationProgressOf<TOwnedApi, O>,
    OperationOutputOf<TOwnedApi, O>,
    OperationTransferContextOf<TOwnedApi, O>,
    OperationHandlerErrorOf<TOwnedApi, O>,
    OperationUpdateOf<TOwnedApi, O>
  >,
  "control" | "reconcile"
>;

type ServiceOperationRegistration<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi,
  O extends keyof TOwnedApi["operations"] & string,
  TKv extends ParticipantKvMetadata = ParticipantKvMetadata,
  TJobs extends ParticipantJobsMetadata = ParticipantJobsMetadata,
> = OperationControlRegistration<TOwnedApi, O> & {
  handle(
    handler: (
      args:
        & OperationHandlerContext<
          InferSchemaType<TOwnedApi["operations"][O]["input"]>,
          OperationProgressOf<TOwnedApi, O>,
          OperationOutputOf<TOwnedApi, O>,
          OperationTransferContextOf<TOwnedApi, O>,
          OperationHandlerErrorOf<TOwnedApi, O>,
          OperationUpdateOf<TOwnedApi, O>
        >
        & { client: Trellis<TTrellisApi, TKv, TJobs> },
    ) => unknown | Promise<unknown>,
  ): Promise<void>;
};

/**
 * Public connected-provider operation registration: the callable `handleXxx`
 * plus owner-fenced `control`/`reconcile`.
 */
export type OperationRegistration<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
  O extends ContractOperationName<TContract>,
> =
  & ((handler: OperationHandler<TContract, O>) => Promise<void>)
  & OperationControlRegistration<ParticipantOwnedApi<TContract>, O>;

export type LiveRegistration<
  TOwnedApi extends RuntimeApi,
  F extends keyof TOwnedApi["lives"] & string,
> = RootLiveRegistration<LiveInputOf<TOwnedApi, F>, LiveEventOf<TOwnedApi, F>>;

export type TrellisServiceConnectArgs<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
> = {
  trellisUrl: string;
  participant: TContract;
  name?: string;
  /** Immutable provisioned service identity. */
  seed: string;
  /** Controls automatic telemetry initialization. Enabled by default. */
  telemetry?: TrellisServiceConnectTelemetryOpts;
  /** Configures the connected service runtime. */
  runtime?: TrellisServiceRuntimeOpts;
};

/** Connected provider runtime inferred from a service contract. */
export type ConnectedTrellisService<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
> = ProviderRuntime<
  TContract,
  TrellisServiceSession<
    ParticipantOwnedApi<TContract>,
    ParticipantTrellisApi<TContract>,
    ParticipantJobsOf<TContract>,
    ParticipantKvOf<TContract>
  >
>;

/**
 * @internal Shared by Trellis-owned service bootstrap paths.
 */
export function createConnectedService<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi,
  TJobs extends ParticipantJobsMetadata = ParticipantJobsMetadata,
  TKv extends ParticipantKvMetadata = ParticipantKvMetadata,
>(args: {
  name: string;
  auth: SessionAuth;
  nc: NatsConnection;
  /**
   * Logical transport provider owning this service's physical generations.
   * When present, outbound work and provider ingress follow the current
   * generation instead of one fixed `nc`. @internal
   */
  transport?: TrellisTransportProvider;
  /** Current admitted physical connection, falling back to the initial `nc`. @internal */
  activeNats?: () => NatsConnection;
  inboxPrefix: string;
  contextDigest: string | (() => string);
  operationConnectionId: string;
  contractId?: string;
  contractDigest?: string;
  participantDigest?: string;
  contractJobs: TJobs;
  contractKv: TKv;
  contractEventConsumers?: ContractEventConsumers;
  apiBindings?: Readonly<Record<string, unknown>>;
  /** `event:<Name>` subscribe needs explicitly declared by this participant. */
  ephemeralEventNeeds?: ReadonlySet<string>;
  runtime: TrellisServiceRuntimeCreateOpts<TOwnedApi, TTrellisApi>;
  bindings: ResourceBindings;
  /**
   * Live resource bindings, re-read so a resource that materializes after
   * connect is reachable through the service resource facade. @internal
   */
  liveBindings?: () => ResourceBindings;
  /**
   * Shared transport-admission gate for this service. Every outbound surface
   * (RPC, Live, events, operations) and every resource check consults it.
   * @internal
   */
  availability: TrellisAvailability;
  healthIdentity?: {
    instanceId: string;
    deploymentId: string;
  };
  authorizationProviderCache?: AuthorizationProviderCache;
  /** Additional raw-transport hook for application-level bookkeeping. @internal */
  onTransportEvent?: (event: unknown) => void;
  /** Process-local connection handle started before the transport connected. @internal */
  telemetry?: ReturnType<typeof startConnectionTelemetry>;
}): Promise<TrellisServiceSession<TOwnedApi, TTrellisApi, TJobs, TKv>> {
  const resolvedLog = resolveServiceLogger(args.runtime.log);
  const liveBindings = args.liveBindings ?? (() => args.bindings);
  // Resource handles follow the logical connection's current generation; a
  // runtime without a generation manager wraps its single connection.
  const serviceTransport: TrellisTransportProvider = args.transport ??
    fixedTransportProvider(args.nc);
  const acquireTimeoutMs = args.runtime.timeout ?? 30_000;
  const onTransportEvent = (event: unknown) => {
    args.authorizationProviderCache?.observeTransportEvent(event);
    args.onTransportEvent?.(event);
  };
  const connection = args.transport
    ? observeTrellisConnection({
      kind: "service",
      transport: args.transport,
      availability: args.availability,
      onTransportEvent,
      log: false,
      lifecycleLog: {
        log: resolvedLog,
        context: { service: args.name },
      },
      ...(args.telemetry ? { telemetry: args.telemetry } : {}),
    })
    : observeNatsTrellisConnection({
      kind: "service",
      nc: args.nc,
      availability: args.availability,
      onTransportEvent,
      log: false,
      lifecycleLog: {
        log: resolvedLog,
        context: { service: args.name },
      },
      ...(args.telemetry ? { telemetry: args.telemetry } : {}),
    });
  const currentApi = (args.runtime.trellisApi ?? args.runtime.api) as
    & TOwnedApi
    & TTrellisApi;
  const runtimeApi = currentApi as TOwnedApi & TTrellisApi;

  const runtime = TrellisServiceRuntime.create(
    args.name,
    args.nc,
    {
      sessionKey: args.auth.sessionKey,
      sign: args.auth.sign,
      contextDigest: args.contextDigest,
      authorizationProviderCache: args.auth.authorizationProviderCache,
    },
    {
      log: resolvedLog,
      timeout: args.runtime.timeout,
      requestLimits: args.runtime.requestLimits,
      stream: args.runtime.stream,
      noResponderRetry: args.runtime.noResponderRetry,
      api: runtimeApi,
      contractId: args.contractId,
      contractDigest: args.contractDigest,
      connection,
      transferSupport: {
        openOperationTransfer: (transferArgs) =>
          getTransfer().createOperationUpload(transferArgs),
        openStagedOperation: (storageKey, committed) =>
          getTransfer().openStagedOperation(storageKey, committed),
      },
      operationDeploymentId: args.healthIdentity?.deploymentId,
      operationConnectionId: args.operationConnectionId,
      // The operation runtime owns per-generation start/reconcile intake through
      // the same generation provider as the outbound framework surfaces.
      ...(args.transport ? { transport: args.transport } : {}),
    },
  );

  const outbound = createTrellisInternal<TTrellisApi>(
    args.name,
    args.nc,
    {
      sessionKey: args.auth.sessionKey,
      sign: args.auth.sign,
      contextDigest: args.contextDigest,
      authorizationProviderCache: args.auth.authorizationProviderCache,
    },
    {
      log: resolvedLog,
      timeout: args.runtime.timeout,
      stream: args.runtime.stream,
      noResponderRetry: args.runtime.noResponderRetry,
      api: runtimeApi,
      contractId: args.contractId,
      contractDigest: args.contractDigest,
      inboxPrefix: args.inboxPrefix,
      eventConsumers: {
        metadata: args.contractEventConsumers,
        bindings: args.bindings.eventConsumers,
      },
      apiBindings: args.apiBindings,
      ephemeralEventNeeds: args.ephemeralEventNeeds ?? new Set(),
      ...(args.transport ? { transport: args.transport } : {}),
      connection,
    },
  );

  const resources: {
    transfer?: ServiceTransfer;
    handlerResources?: ServiceHandlerResources<TKv, TJobs, TTrellisApi>;
  } = {};
  const getTransfer = (): ServiceTransfer => {
    if (!resources.transfer) {
      throw new Error("service transfer helper accessed before initialization");
    }
    return resources.transfer;
  };
  const getHandlerResources = (): ServiceHandlerResources<
    TKv,
    TJobs,
    TTrellisApi
  > => {
    if (!resources.handlerResources) {
      throw new Error(
        "service resource handles accessed before initialization",
      );
    }
    return resources.handlerResources;
  };

  const handlerTrellis: Trellis<TTrellisApi, TKv, TJobs> = {
    rpc: outbound.rpc,
    event: createServiceEventPublishFacade(outbound),
    live: outbound.live,
    operation: outbound.operation,
    request: outbound.request.bind(outbound),
    prepare: (event, data) => outbound.prepare(event, data),
    publish: (event, data) => outbound.publish(event, data),
    publishPrepared: (event) => outbound.publishPrepared(event),
    stopEventListeners: () => outbound.stopEventListeners(),
    stopRequestListeners: () => outbound.stopRequestListeners(),
    get kv() {
      return getHandlerResources().kv;
    },
    get store() {
      return getHandlerResources().store;
    },
    get jobs() {
      return getHandlerResources().jobs;
    },
  };

  const health = new ServiceHealthRuntime({
    serviceName: args.name,
    instanceId: args.healthIdentity?.instanceId,
    contractId: args.contractId ?? "unknown",
    contractDigest: args.participantDigest ?? "unknown",
    publishIntervalMs: args.runtime.health?.publishIntervalMs ?? 30_000,
  });
  health.add("nats", () => ({
    status: serviceTransport.isClosed() ||
        serviceTransport.currentNats().isClosed()
      ? "failed"
      : "ok",
    ...(serviceTransport.isClosed()
      ? { summary: "NATS connection closed" }
      : {}),
  }));

  const heartbeatEnabled = args.healthIdentity !== undefined;
  let healthPublishTimer: ReturnType<typeof setInterval> | undefined;
  const healthPublishingAbort = new AbortController();
  let publishingHeartbeat: Promise<void> | undefined;
  const publishHealthHeartbeat = (): Promise<void> => {
    if (!args.healthIdentity || healthPublishingAbort.signal.aborted) {
      return Promise.resolve();
    }
    if (publishingHeartbeat) return publishingHeartbeat;
    const identity = args.healthIdentity;
    publishingHeartbeat = (async () => {
      let lease: TransportLease | undefined;
      try {
        lease = await serviceTransport.acquireCurrent({
          signal: healthPublishingAbort.signal,
          deadlineMs: Date.now() + acquireTimeoutMs,
        });
        if (healthPublishingAbort.signal.aborted) return;
        const sample = await health.sample();
        if (healthPublishingAbort.signal.aborted) return;
        // Pin the admitted generation through sampling and the JetStream PubAck.
        await publishHealthHeartbeatSample({
          nc: lease.nc,
          identity: {
            sessionKey: args.auth.sessionKey,
            participantKind: "service",
            contractId: health.contractId,
            contractDigest: health.contractDigest,
            deploymentId: identity.deploymentId,
            instanceId: health.instanceId,
          },
          sample,
        });
      } catch (error) {
        resolvedLog.warn(
          { error },
          "Failed to build or publish health heartbeat",
        );
      } finally {
        lease?.release();
        publishingHeartbeat = undefined;
      }
    })();
    return publishingHeartbeat;
  };
  const stopHealthPublishing = (): Promise<void> => {
    healthPublishingAbort.abort();
    if (healthPublishTimer !== undefined) {
      clearInterval(healthPublishTimer);
      healthPublishTimer = undefined;
    }
    // Already-published work finishes its PubAck before service teardown; stop
    // cancels pending admission and never acquires a replacement generation.
    return publishingHeartbeat ?? Promise.resolve();
  };

  const kv = openServiceKvBindings({
    transport: serviceTransport,
    acquireTimeoutMs,
    bindings: () => liveBindings().kv ?? {},
    contractKv: args.contractKv,
  });

  const operationTransfer = new ServiceTransfer({
    log: resolvedLog,
    name: args.name,
    connectionId: args.operationConnectionId,
    transport: serviceTransport,
    auth: { ...args.auth, contextDigest: args.contextDigest },
    stores: Object.fromEntries(
      Object.entries(args.bindings.store ?? {}).map(([alias, binding]) => [
        alias,
        new InternalStoreHandle(
          serviceTransport,
          acquireTimeoutMs,
          binding,
          storeHandleConstructorToken,
        ),
      ]),
    ),
    ...(args.healthIdentity
      ? {
        operationStagingBucket:
          `trellis_operation_staging_${args.healthIdentity.deploymentId}`,
      }
      : {}),
  });

  const service = Reflect.construct(TrellisServiceSession, [
    args.name,
    args.auth,
    args.nc,
    runtime,
    outbound.event,
    outbound,
    handlerTrellis,
    kv,
    args.contractJobs,
    args.bindings,
    operationTransfer,
    health,
    stopHealthPublishing,
    connection,
    serviceTransport,
    acquireTimeoutMs,
    resolvedLog,
    trellisServiceConstructorToken,
  ]) as TrellisServiceSession<TOwnedApi, TTrellisApi, TJobs, TKv>;
  resources.handlerResources = {
    kv: service.kv,
    store: service.store,
    jobs: service.jobs,
  };
  resources.transfer = operationTransfer;

  if (heartbeatEnabled) {
    // Generation 1 activates after construction, so admission must not block it.
    void publishHealthHeartbeat();
    healthPublishTimer = setInterval(() => {
      void publishHealthHeartbeat();
    }, health.publishIntervalMs);
    void serviceTransport.closed().then(
      stopHealthPublishing,
      stopHealthPublishing,
    );
  }

  return Promise.resolve(service);
}

type RegisteredJobHandler<TPayload, TResult> = (
  job: PublicActiveJob<TPayload, TResult, unknown>,
) => Promise<Result<TResult, BaseError>>;

function toUnexpectedError(cause: unknown): UnexpectedError {
  return cause instanceof UnexpectedError
    ? cause
    : new UnexpectedError({ cause });
}

function resolveSqlOutboxTables(
  tables: Partial<SqlOutboxTables> | undefined,
): SqlOutboxTables {
  return {
    outbox: tables?.outbox ?? defaultSqlOutboxTables.outbox,
    inbox: tables?.inbox ?? defaultSqlOutboxTables.inbox,
  };
}

function createSqlOutboxBaseExecutor<TTx>(
  options: TrellisServiceSqlOutboxOptions<TTx>,
): SqlExecutor {
  return options.executor;
}

function createSqlOutboxTransactionRunner<TTx>(
  options: TrellisServiceSqlOutboxOptions<TTx>,
): SqlOutboxTransactionRunner<TTx> {
  return options.transaction;
}

type SqlOutboxEventEnqueueLeaf = {
  enqueue(
    event: Record<string, unknown>,
  ): AsyncResult<OutboxMessage, ValidationError | UnexpectedError>;
};

function createSqlOutboxEventEnqueueFacade<TEventApi extends RuntimeApi>(args: {
  event: ActiveEventFacade<TEventApi>;
  repository: SqlOutboxRepository;
  onEnqueued(): void;
}): SqlOutboxEventEnqueueFacade<TEventApi> {
  const facade: Record<string, Record<string, SqlOutboxEventEnqueueLeaf>> = {};
  const source = args.event as Record<
    string,
    Record<string, ServiceEventLeaf>
  >;

  for (const [groupName, leaves] of Object.entries(source)) {
    const group: Record<string, SqlOutboxEventEnqueueLeaf> = {};
    for (const [leafName, leaf] of Object.entries(leaves)) {
      group[leafName] = {
        enqueue: (payload) =>
          AsyncResult.from((async () => {
            const prepared = leaf.prepare(payload).take();
            if (isErr(prepared)) return Result.err(prepared.error);
            const record = preparedTrellisEventToOutboxRecord(prepared);
            try {
              const message = await args.repository.enqueue(record);
              args.onEnqueued();
              return Result.ok(message);
            } catch (cause) {
              return Result.err(toUnexpectedError(cause));
            }
          })()),
      };
    }
    Object.defineProperty(facade, groupName, {
      value: group,
      enumerable: true,
      configurable: true,
    });
  }

  return facade as SqlOutboxEventEnqueueFacade<TEventApi>;
}

function createSqlOutboxJobEnqueueFacade<TJobs extends ParticipantJobsMetadata>(
  args: {
    contractJobs: TJobs;
    jobsBinding?: ResourceBindingJobs;
    repository: SqlOutboxRepository;
    onEnqueued(): void;
  },
): SqlOutboxJobEnqueueFacade<TJobs> {
  const facade: Record<string, Record<string, unknown>> = {};

  for (
    const queueType of Object.keys(args.contractJobs as Record<string, unknown>)
  ) {
    const jobsBinding = args.jobsBinding;
    const queueBinding = jobsBinding?.queues[queueType];
    const queue = queueType;
    facade[queue] = {
      create: (payload: unknown) => {
        if (!queueBinding) {
          return AsyncResult.err(
            new ValidationError({
              errors: [{
                path: "/",
                message: `Jobs binding unavailable for queue '${queue}'`,
              }],
            }),
          );
        }
        const submissionId = ulid();
        const jobId = ulid();
        const submission = prepareJobSubmission({
          submissionId,
          mode: "create",
          service: jobsBinding.serviceName,
          queue,
          jobId,
          payload,
          createdAt: new Date().toISOString(),
        });

        const record: PreparedOutboxRecord = {
          id: submissionId,
          kind: "job.create",
          name: queue,
          subject: `${queueBinding.publishPrefix}.${jobId}.created`,
          payload: JSON.stringify(submission),
          headers: {
            "request-id": submission.context.requestId,
            "traceparent": submission.context.traceparent,
            ...(submission.context.tracestate
              ? { "tracestate": submission.context.tracestate }
              : {}),
          },
        };

        return AsyncResult.from((async () => {
          try {
            await args.repository.enqueue(record);
            args.onEnqueued();
            return Result.ok({
              submissionId,
              jobId,
              queue,
              mode: "create",
            });
          } catch (cause) {
            return Result.err(toUnexpectedError(cause));
          }
        })());
      },
      submit: (payload: unknown) => {
        if (!queueBinding) {
          return AsyncResult.err(
            new ValidationError({
              errors: [{
                path: "/",
                message: `Jobs binding unavailable for queue '${queue}'`,
              }],
            }),
          );
        }
        const submissionId = ulid();
        const jobId = ulid();
        const submission = prepareJobSubmission({
          submissionId,
          mode: "submit",
          service: jobsBinding.serviceName,
          queue,
          jobId,
          payload,
          createdAt: new Date().toISOString(),
        });

        const record: PreparedOutboxRecord = {
          id: submissionId,
          kind: "job.submit",
          name: queue,
          subject: `${queueBinding.publishPrefix}.${jobId}.created`,
          payload: JSON.stringify(submission),
          headers: {
            "request-id": submission.context.requestId,
            "traceparent": submission.context.traceparent,
            ...(submission.context.tracestate
              ? { "tracestate": submission.context.tracestate }
              : {}),
          },
        };

        return AsyncResult.from((async () => {
          try {
            await args.repository.enqueue(record);
            args.onEnqueued();
            return Result.ok({
              submissionId,
              jobId,
              queue,
              mode: "submit",
            });
          } catch (cause) {
            return Result.err(toUnexpectedError(cause));
          }
        })());
      },
    };
  }

  return facade as SqlOutboxJobEnqueueFacade<TJobs>;
}

function serializeJobHandlerError(error: BaseError): string {
  try {
    return JSON.stringify(error.toSerializable());
  } catch {
    return error.message;
  }
}

function okVoid(): Result<void, never> {
  return Result.ok(undefined);
}

function wrapVoidTask(task: () => Promise<void>): AsyncResult<void, BaseError> {
  return AsyncResult.from((async () => {
    try {
      await task();
      return okVoid();
    } catch (cause) {
      return Result.err(toUnexpectedError(cause));
    }
  })());
}

function isTerminalJobState(
  state: string,
): state is TerminalJob<unknown, unknown>["state"] {
  return state === "completed" || state === "failed" || state === "cancelled" ||
    state === "expired" || state === "skipped" || state === "stale" ||
    state === "dead" || state === "dismissed";
}

function isTerminalJobSnapshot<TPayload, TResult>(
  snapshot: JobSnapshot<TPayload, TResult>,
): snapshot is TerminalJob<TPayload, TResult> {
  return isTerminalJobState(snapshot.state);
}

function operationOutputsEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (typeof left !== typeof right || left === null || right === null) {
    return false;
  }
  if (Array.isArray(left) || Array.isArray(right)) {
    return Array.isArray(left) && Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => operationOutputsEqual(value, right[index]));
  }
  if (typeof left === "object") {
    if (Object.getPrototypeOf(left) !== Object.prototype) return false;
    if (Object.getPrototypeOf(right) !== Object.prototype) return false;
    const leftRecord = left as Record<string, unknown>;
    const rightRecord = right as Record<string, unknown>;
    const leftKeys = Object.keys(leftRecord);
    const rightKeys = Object.keys(rightRecord);
    return leftKeys.length === rightKeys.length &&
      leftKeys.every((key) =>
        Object.hasOwn(rightRecord, key) &&
        operationOutputsEqual(leftRecord[key], rightRecord[key])
      );
  }
  return false;
}

function parseJobLifecycleEvent<TPayload, TResult>(
  data: Uint8Array,
): InternalJobEvent<TPayload, TResult> | undefined {
  try {
    const decoded = JSON.parse(new TextDecoder().decode(data));
    if (!Value.Check(JobEventSchema, decoded)) {
      return undefined;
    }
    return decoded as InternalJobEvent<TPayload, TResult>;
  } catch {
    return undefined;
  }
}

function jobLifecycleKey(
  service: string,
  jobType: string,
  jobId: string,
): string {
  return `${service}.${jobType}.${jobId}`;
}

function subjectMatchesLifecycleEvent(
  subject: string,
  queueBinding: Pick<ResourceBindingJobsQueue, "publishPrefix">,
  event: InternalJobEvent,
): boolean {
  const prefix = `${queueBinding.publishPrefix}.`;
  if (!subject.startsWith(prefix)) return false;

  const suffix = subject.slice(prefix.length).split(".");
  return suffix.length === 2 && suffix[0] === event.jobId &&
    suffix[1] === event.eventType;
}

function snapshotFromLifecycleEvent<TPayload, TResult>(
  current: JobSnapshot<TPayload, TResult>,
  event: InternalJobEvent<TPayload, TResult>,
): JobSnapshot<TPayload, TResult> {
  if (
    event.service !== current.service || event.jobType !== current.type ||
    event.jobId !== current.id
  ) {
    return current;
  }
  if (isTerminalJobState(current.state)) {
    return current;
  }

  const base: JobSnapshot<TPayload, TResult> = {
    ...current,
    state: event.state,
    updatedAt: event.timestamp,
    tries: event.tries,
    ...(event.maxTries !== undefined ? { maxTries: event.maxTries } : {}),
    ...(event.deadline !== undefined ? { deadline: event.deadline } : {}),
    ...(event.trigger !== undefined ? { trigger: event.trigger } : {}),
    ...(event.lineage !== undefined ? { lineage: event.lineage } : {}),
  };

  switch (event.eventType) {
    case "created":
    case "retried":
      return event.payload === undefined ? base : {
        ...base,
        payload: event.payload,
      };
    case "started":
      return { ...base, startedAt: event.timestamp };
    case "progress":
      return event.progress === undefined ? base : {
        ...base,
        progress: event.progress,
      };
    case "logged":
      return event.logs === undefined ? base : {
        ...base,
        logs: [...(current.logs ?? []), ...event.logs],
      };
    case "waiting": {
      if (event.waitEdge === undefined) return base;
      const waitingOn = (current.waitingOn ?? []).filter((edge) =>
        edge.id !== event.waitEdge?.id
      );
      return { ...base, waitingOn: [...waitingOn, event.waitEdge] };
    }
    case "resumed":
      return event.waitEdge === undefined ? base : {
        ...base,
        waitingOn: (current.waitingOn ?? []).filter((edge) =>
          edge.id !== event.waitEdge?.id
        ),
      };
    case "completed":
      return {
        ...base,
        completedAt: event.timestamp,
        ...(event.result !== undefined ? { result: event.result } : {}),
      };
    case "failed":
    case "cancelled":
    case "expired":
    case "skipped":
    case "stale":
    case "staleCompletionIgnored":
    case "dead":
    case "dismissed":
      return event.error === undefined ? base : {
        ...base,
        lastError: event.error,
      };
  }

  return base;
}

function headersFromJobContext(context: InternalJobContext): MsgHdrs {
  const headers = natsHeaders();
  headers.set("request-id", context.requestId);
  headers.set("traceparent", context.traceparent);
  if (context.tracestate) {
    headers.set("tracestate", context.tracestate);
  }
  return headers;
}

type JobLifecycleWaiter<TPayload, TResult> = {
  resolve(snapshot: TerminalJob<TPayload, TResult>): void;
  reject(cause: BaseError): void;
};

type JobLifecycleTracker = {
  watch(
    queueBinding: ResourceBindingJobsQueue | JobsQueueBinding,
    jobId?: string,
  ): void;
  seed<TPayload, TResult>(snapshot: JobSnapshot<TPayload, TResult>): void;
  get<TPayload, TResult>(args: {
    service: string;
    jobType: string;
    id: string;
  }): JobSnapshot<TPayload, TResult> | undefined;
  attempt(args: {
    service: string;
    jobType: string;
    id: string;
  }): number | undefined;
  apply<TPayload, TResult>(
    event: InternalJobEvent<TPayload, TResult>,
  ): JobSnapshot<TPayload, TResult> | undefined;
  wait<TPayload, TResult>(
    snapshot: JobSnapshot<TPayload, TResult>,
  ): Promise<TerminalJob<TPayload, TResult>>;
  stop(): void;
};

function createJobLifecycleTracker(nc: NatsConnection): JobLifecycleTracker {
  const snapshots = new Map<string, JobSnapshot<unknown, unknown>>();
  const attempts = new Map<string, number>();
  const waiters = new Map<string, JobLifecycleWaiter<unknown, unknown>[]>();
  const subscriptions = new Map<string, Subscription>();
  let stopped = false;

  const notify = (key: string, snapshot: JobSnapshot<unknown, unknown>) => {
    if (!isTerminalJobSnapshot(snapshot)) return;
    const pending = waiters.get(key) ?? [];
    waiters.delete(key);
    for (const waiter of pending) waiter.resolve(snapshot);
  };

  const apply = <TPayload, TResult>(
    event: InternalJobEvent<TPayload, TResult>,
  ): JobSnapshot<TPayload, TResult> | undefined => {
    const key = jobLifecycleKey(event.service, event.jobType, event.jobId);
    attempts.set(key, Math.max(attempts.get(key) ?? 0, event.tries));
    const current = snapshots.get(key) as
      | JobSnapshot<TPayload, TResult>
      | undefined;
    if (!current) return undefined;

    const next = snapshotFromLifecycleEvent(current, event);
    snapshots.set(key, next as JobSnapshot<unknown, unknown>);
    notify(key, next as JobSnapshot<unknown, unknown>);
    return next;
  };

  return {
    watch(queueBinding, jobId) {
      if (subscriptions.has(queueBinding.publishPrefix)) return;

      const subscription = nc.subscribe(
        `${queueBinding.publishPrefix}.${jobId ?? "*"}.*`,
      );
      subscriptions.set(queueBinding.publishPrefix, subscription);
      void (async () => {
        try {
          for await (const msg of subscription) {
            const event = parseJobLifecycleEvent(msg.data);
            if (
              !event || !subjectMatchesLifecycleEvent(
                msg.subject,
                queueBinding,
                event,
              )
            ) {
              continue;
            }
            apply(event);
          }
        } finally {
          // A per-call observer cannot remain pending after its socket closes.
          if (jobId !== undefined) this.stop();
        }
      })().catch(() => {
        if (jobId !== undefined) this.stop();
      });
    },
    seed<TPayload, TResult>(snapshot: JobSnapshot<TPayload, TResult>) {
      const key = jobLifecycleKey(snapshot.service, snapshot.type, snapshot.id);
      attempts.set(key, Math.max(attempts.get(key) ?? 0, snapshot.tries));
      const current = snapshots.get(key) as
        | JobSnapshot<TPayload, TResult>
        | undefined;
      if (current && isTerminalJobState(current.state)) return;
      snapshots.set(key, snapshot as JobSnapshot<unknown, unknown>);
      notify(key, snapshot as JobSnapshot<unknown, unknown>);
    },
    get<TPayload, TResult>(args: {
      service: string;
      jobType: string;
      id: string;
    }) {
      const current = snapshots.get(
        jobLifecycleKey(args.service, args.jobType, args.id),
      );
      return current as JobSnapshot<TPayload, TResult> | undefined;
    },
    attempt(args) {
      return attempts.get(jobLifecycleKey(args.service, args.jobType, args.id));
    },
    apply,
    wait<TPayload, TResult>(snapshot: JobSnapshot<TPayload, TResult>) {
      this.seed(snapshot);
      const key = jobLifecycleKey(snapshot.service, snapshot.type, snapshot.id);
      const current = snapshots.get(key) as
        | JobSnapshot<TPayload, TResult>
        | undefined;
      if (current && isTerminalJobSnapshot(current)) {
        return Promise.resolve(current);
      }
      if (stopped) {
        return Promise.reject(toUnexpectedError(
          new Error("job lifecycle tracker stopped"),
        ));
      }

      return new Promise((resolve, reject) => {
        const pending = waiters.get(key) ?? [];
        pending.push({
          resolve: resolve as (snapshot: TerminalJob<unknown, unknown>) => void,
          reject,
        });
        waiters.set(key, pending);
      });
    },
    stop() {
      stopped = true;
      for (const subscription of subscriptions.values()) {
        subscription.unsubscribe();
      }
      subscriptions.clear();
      const error = toUnexpectedError(
        new Error("job lifecycle tracker stopped"),
      );
      for (const pending of waiters.values()) {
        for (const waiter of pending) waiter.reject(error);
      }
      waiters.clear();
      attempts.clear();
    },
  };
}

function createJobRef<TPayload, TResult, TUpdate = unknown>(args: {
  nc: NatsConnection;
  queueType: string;
  jobsBinding: JobsBinding;
  queueBinding: JobsQueueBinding;
  seed: JobSnapshot<TPayload, TResult>;
  lifecycle: JobLifecycleTracker;
  acquireTransport?: () => Promise<TransportLease | undefined>;
  updates?: (
    options?: JobUpdatesOptions,
  ) => AsyncResult<JobUpdateSubscription<TUpdate>, BaseError>;
}): JobRef<TPayload, TResult, TUpdate> {
  args.lifecycle.seed(args.seed);

  const exchange = <T>(
    observe: boolean,
    run: (
      nc: NatsConnection,
      lifecycle: JobLifecycleTracker,
      current: JobSnapshot<TPayload, TResult>,
    ) => Promise<T>,
  ): AsyncResult<T, BaseError> =>
    AsyncResult.from((async () => {
      let lease: TransportLease | undefined;
      let observer: JobLifecycleTracker | undefined;
      try {
        lease = await args.acquireTransport?.();
        const nc = lease?.nc ?? args.nc;
        let current = args.seed;
        if (lease) {
          if (observe) {
            observer = createJobLifecycleTracker(nc);
            observer.seed(args.seed);
            observer.watch(args.queueBinding, args.seed.id);
            await nc.flush();
          }
          const jsm = await jetstreamManager(nc);
          const event = await getLatestLifecycleEvent(
            jsm.direct,
            "JOBS",
            args.queueBinding.publishPrefix,
            args.seed,
          );
          if (event) {
            current = snapshotFromLifecycleEvent(
              args.seed,
              event as InternalJobEvent<TPayload, TResult>,
            );
          }
        } else {
          current = args.lifecycle.get<TPayload, TResult>({
            service: args.seed.service,
            jobType: args.queueType,
            id: args.seed.id,
          }) ?? args.seed;
        }
        return Result.ok(await run(nc, observer ?? args.lifecycle, current));
      } catch (cause) {
        return Result.err(
          cause instanceof BaseError ? cause : toUnexpectedError(cause),
        );
      } finally {
        observer?.stop();
        lease?.release();
      }
    })());

  return new JobRef<TPayload, TResult, TUpdate>(
    {
      id: args.seed.id,
      service: args.seed.service,
      jobType: args.queueType,
    },
    {
      get: () =>
        exchange(false, (_nc, _lifecycle, current) => Promise.resolve(current)),
      wait: () =>
        exchange(true, (_nc, lifecycle, current) => lifecycle.wait(current)),
      cancel: () =>
        exchange(false, async (nc, lifecycle, current) => {
          if (isTerminalJobState(current.state)) {
            return current;
          }

          const event: InternalJobEvent<TPayload, TResult> = {
            jobId: args.seed.id,
            service: current.service,
            jobType: args.queueType,
            eventType: "cancelled",
            state: "cancelled",
            previousState: current.state,
            context: current.context,
            tries: current.tries,
            error: "cancelled",
            timestamp: new Date().toISOString(),
          };

          nc.publish(
            `${args.queueBinding.publishPrefix}.${args.seed.id}.cancelled`,
            new TextEncoder().encode(JSON.stringify(event)),
            { headers: headersFromJobContext(event.context) },
          );
          if (args.acquireTransport) await nc.flush();
          return args.acquireTransport
            ? snapshotFromLifecycleEvent(current, event)
            : lifecycle.apply(event) ?? current;
        }),
      updates: args.updates,
    },
  );
}

function subscribeToJobUpdates(args: {
  nc: NatsConnection;
  active: Set<Subscription>;
  lifecycle: JobLifecycleTracker;
  service: string;
  queue: ResourceBindingJobsQueue;
  updateSchema: NonNullable<ParticipantJobsMetadata[string]["updateSchema"]>;
  jobId: string;
  options?: JobUpdatesOptions;
}): AsyncResult<JobUpdateSubscription<unknown>, BaseError> {
  return AsyncResult.from(
    Promise.resolve().then(() => {
      if (!args.queue.update || !args.queue.updatesPrefix) {
        return Result.err(toUnexpectedError(
          new Error("Job updates are not configured for this queue"),
        ));
      }
      if (!/^[^.>*\s]+$/u.test(args.jobId)) {
        return Result.err(
          new ValidationError({
            errors: [{
              path: "/jobId",
              message: "Job id must be one NATS token",
            }],
          }),
        );
      }

      const subscription = args.nc.subscribe(
        `${args.queue.updatesPrefix}.${args.jobId}`,
      );
      args.active.add(subscription);
      const close = () => {
        subscription.unsubscribe();
        args.active.delete(subscription);
      };
      const abort = () => close();
      args.options?.signal?.addEventListener("abort", abort, { once: true });
      if (args.options?.signal?.aborted) close();

      const updates: JobUpdateSubscription<unknown> = {
        unsubscribe: close,
        async *[Symbol.asyncIterator]() {
          let attempt = 0;
          let sequence = 0;
          try {
            for await (const message of subscription) {
              const envelope = decodeJobUpdateEnvelope(message.data);
              if (!envelope || envelope.jobId !== args.jobId) continue;
              if (!isJsonValue(envelope.update)) continue;
              let parsed: unknown;
              if (
                typeof Reflect.get(args.updateSchema, "decode") === "function"
              ) {
                try {
                  parsed = (args.updateSchema as Codec<unknown>).decode(
                    envelope.update,
                  );
                } catch {
                  continue;
                }
              } else {
                parsed = parseSchema(args.updateSchema, envelope.update).take();
              }
              if (isErr(parsed)) continue;
              const lifecycleAttempt = args.lifecycle.attempt({
                service: args.service,
                jobType: args.queue.queueType,
                id: args.jobId,
              }) ?? 0;
              if (lifecycleAttempt > attempt) {
                attempt = lifecycleAttempt;
                sequence = 0;
              }
              if (envelope.attempt < attempt) continue;
              if (envelope.attempt > attempt) {
                attempt = envelope.attempt;
                sequence = 0;
              }
              if (envelope.sequence <= sequence) continue;
              sequence = envelope.sequence;
              yield parsed;
            }
          } finally {
            close();
            args.options?.signal?.removeEventListener("abort", abort);
          }
        },
      };
      return Result.ok(updates);
    }),
  );
}

function createNoopJobWorkerHost(): JobWorkerHostAdapter {
  return new JobWorkerHostAdapter({
    stop: () => AsyncResult.ok(undefined),
    join: () => AsyncResult.ok(undefined),
  });
}

function createJobsFacade<
  TJobs extends ParticipantJobsMetadata,
  TClient,
>(args: {
  serviceName: string;
  contractId?: string;
  contractDigest?: string;
  nc: NatsConnection;
  contractJobs: TJobs;
  transport: TrellisTransportProvider;
  acquireTimeoutMs: number;
  intakeOwner: Pick<
    TrellisServiceRuntimeFor<RuntimeApi>,
    "declareFrameworkIntake" | "retractFrameworkIntake"
  >;
  client: TClient;
  jobsBinding?: ResourceBindingJobs;
  workStream?: string;
}): ManagedJobsFacade<TJobs, TClient> {
  const handlers = new Map<string, {
    handler: RegisteredJobHandler<unknown, unknown>;
    concurrency: number;
  }>();
  const jobsFacade: Record<string, unknown> = {};
  const lifecycle = createJobLifecycleTracker(args.nc);
  const updateSubscriptions = new Set<Subscription>();
  const keyCoordinator = createNatsJobKeyCoordinator(args.nc);
  const jobsBinding = args.jobsBinding
    ? normalizeResourceJobsBinding(args.jobsBinding)
    : undefined;
  const manager = new InternalJobManager<unknown, unknown>({
    nc: jetstream(args.nc),
    jobs: jobsBinding,
    keyCoordinator,
  });
  const acquireSubmissionTransport = async () => {
    if (!(args.transport instanceof TransportGenerationManager)) {
      return undefined;
    }
    return await args.transport.acquireCurrent({
      deadlineMs: Date.now() + args.acquireTimeoutMs,
    });
  };
  let activeHost: JobWorkerHostAdapter | undefined;
  let startupPromise:
    | Promise<Result<JobWorkerHostAdapter, BaseError>>
    | undefined;
  let stopPromise: Promise<Result<void, BaseError>> | undefined;
  const startupCancellation = new AbortController();
  const startingHosts = new Set<{ stop(reason?: unknown): Promise<void> }>();
  const cancellationRegistry = new ActiveJobCancellationRegistry();

  for (const queueType of Object.keys(args.contractJobs ?? {})) {
    const queueBinding = jobsBinding?.queues[queueType];
    const updateSchema = args.contractJobs[queueType]?.updateSchema;
    if (queueBinding) lifecycle.watch(queueBinding);

    const updates = (jobId: string, options?: JobUpdatesOptions) => {
      if (!queueBinding || !updateSchema) {
        return AsyncResult.err(toUnexpectedError(
          new Error(`Job updates are unavailable for queue '${queueType}'`),
        ));
      }
      return subscribeToJobUpdates({
        nc: args.nc,
        active: updateSubscriptions,
        lifecycle,
        service: args.serviceName,
        queue: queueBinding,
        updateSchema,
        jobId,
        options,
      });
    };

    jobsFacade[queueType] = {
      updates,
      create: (payload) =>
        AsyncResult.from((async () => {
          let lease: TransportLease | undefined;
          try {
            if (!jobsBinding) {
              return Result.err(
                toUnexpectedError(new Error("Jobs bindings are unavailable")),
              );
            }
            const queueBinding = jobsBinding.queues[queueType];
            if (!queueBinding) {
              return Result.err(toUnexpectedError(
                new Error(
                  `Jobs binding for queue '${queueType}' is unavailable`,
                ),
              ));
            }

            lease = await acquireSubmissionTransport();
            const submissionManager = lease
              ? manager.withTransport(
                jetstream(lease.nc),
                createNatsJobKeyCoordinator(lease.nc),
              )
              : manager;
            const created = await submissionManager.create(queueType, payload);
            return Result.ok(createJobRef({
              nc: args.nc,
              acquireTransport:
                args.transport instanceof TransportGenerationManager
                  ? acquireSubmissionTransport
                  : undefined,
              queueType,
              jobsBinding,
              queueBinding,
              seed: created as JobSnapshot<unknown, unknown>,
              lifecycle,
              updates: (options) => updates(created.id, options),
            }));
          } catch (cause) {
            if (
              cause instanceof JobNotEnqueuedError ||
              cause instanceof TransportError
            ) {
              return Result.err(cause);
            }
            return Result.err(toUnexpectedError(cause));
          } finally {
            lease?.release();
          }
        })()),
      submit: (payload) =>
        AsyncResult.from((async () => {
          let lease: TransportLease | undefined;
          try {
            if (!jobsBinding) {
              return Result.err(
                toUnexpectedError(new Error("Jobs bindings are unavailable")),
              );
            }
            const queueBinding = jobsBinding.queues[queueType];
            if (!queueBinding) {
              return Result.err(toUnexpectedError(
                new Error(
                  `Jobs binding for queue '${queueType}' is unavailable`,
                ),
              ));
            }

            lease = await acquireSubmissionTransport();
            const submissionManager = lease
              ? manager.withTransport(
                jetstream(lease.nc),
                createNatsJobKeyCoordinator(lease.nc),
              )
              : manager;
            const outcome = await submissionManager.submit(queueType, payload);
            if (outcome.kind === "accepted") {
              return Result.ok({
                kind: "accepted",
                key: outcome.key,
                ref: createJobRef({
                  nc: args.nc,
                  acquireTransport:
                    args.transport instanceof TransportGenerationManager
                      ? acquireSubmissionTransport
                      : undefined,
                  queueType,
                  jobsBinding,
                  queueBinding,
                  seed: outcome.job as JobSnapshot<unknown, unknown>,
                  lifecycle,
                  updates: (options) => updates(outcome.job.id, options),
                }),
              });
            }
            if (outcome.kind === "replaced") {
              return Result.ok({
                kind: "replaced",
                key: outcome.key,
                replaced: outcome.replaced,
                ref: createJobRef({
                  nc: args.nc,
                  acquireTransport:
                    args.transport instanceof TransportGenerationManager
                      ? acquireSubmissionTransport
                      : undefined,
                  queueType,
                  jobsBinding,
                  queueBinding,
                  seed: outcome.job as JobSnapshot<unknown, unknown>,
                  lifecycle,
                  updates: (options) => updates(outcome.job.id, options),
                }),
              });
            }
            return Result.ok(outcome);
          } catch (cause) {
            if (cause instanceof TransportError) return Result.err(cause);
            return Result.err(toUnexpectedError(cause));
          } finally {
            lease?.release();
          }
        })()),
      handle: (handler, options) => {
        if (handlers.has(queueType)) {
          throw new Error(
            `Job handler for queue '${queueType}' is already registered`,
          );
        }
        if (activeHost || startupPromise) {
          throw new Error(
            `Job handler for queue '${queueType}' cannot be registered after worker startup has begun`,
          );
        }
        const concurrency = options?.concurrency ?? 1;
        if (!Number.isInteger(concurrency) || concurrency < 1) {
          throw new Error(
            `Job handler for queue '${queueType}' has invalid concurrency ${concurrency}; expected a positive integer`,
          );
        }
        handlers.set(queueType, {
          concurrency,
          handler: async (job) =>
            await handler({
              job,
              client: args.client,
            }),
        });
      },
    } satisfies ServiceJobQueue<unknown, unknown, unknown, TClient>;
  }

  const managedWorkers: ManagedJobWorkers = {
    start: () => {
      if (activeHost) {
        return AsyncResult.ok(activeHost);
      }
      if (startupPromise) {
        return AsyncResult.from(startupPromise);
      }

      startupPromise = (async () => {
        startupCancellation.signal.throwIfAborted();
        const selectedQueues = [...handlers.keys()];
        if (selectedQueues.length === 0) {
          const host = createNoopJobWorkerHost();
          activeHost = host;
          return Result.ok(host);
        }

        if (!jobsBinding || !args.workStream) {
          return Result.err(toUnexpectedError(
            new Error(
              "Jobs infrastructure bindings are unavailable for this service",
            ),
          ));
        }

        const workStream = args.workStream;

        const hosts = [] as Array<{ stop(): Promise<void> }>;
        try {
          for (const queueType of selectedQueues) {
            const queueBinding = jobsBinding.queues[queueType];
            if (!queueBinding) {
              throw new Error(`Unknown jobs queue '${queueType}'`);
            }
            const updateSchema = args.contractJobs[queueType]?.updateSchema;
            const workerUpdates = (
              jobId: string,
              nc: NatsConnection,
              lifecycle: JobLifecycleTracker,
              options?: JobUpdatesOptions,
            ) => {
              if (!updateSchema) {
                return AsyncResult.err(toUnexpectedError(
                  new Error(
                    `Job updates are unavailable for queue '${queueType}'`,
                  ),
                ));
              }
              return subscribeToJobUpdates({
                nc,
                active: updateSubscriptions,
                lifecycle,
                service: args.serviceName,
                queue: queueBinding,
                updateSchema,
                jobId,
                options,
              });
            };
            const registration = handlers.get(queueType);
            if (!registration) {
              throw new Error(
                `No job handler registered for queue '${queueType}'`,
              );
            }

            const workerOptions: StartNatsWorkerHostOptions<unknown> = {
              nats: args.nc,
              instanceId: `${args.serviceName}-worker`,
              queueTypes: [queueType],
              queueConcurrency: {
                [queueType]: registration.concurrency,
              },
              manager,
              heartbeatPublisher: args.nc,
              getProjectedJob: (job) =>
                Promise.resolve(lifecycle.get({
                  service: job.service,
                  jobType: job.type,
                  id: job.id,
                })),
              handler: async (
                job: InternalActiveJob<unknown, unknown>,
                session: JobReceivingSession<unknown>,
              ) => {
                const receivingNc = session.nc ?? args.nc;
                const receivingLifecycle =
                  args.transport instanceof TransportGenerationManager
                    ? createJobLifecycleTracker(receivingNc)
                    : lifecycle;
                if (receivingLifecycle !== lifecycle) {
                  receivingLifecycle.watch(queueBinding);
                }
                let updateSequence = 0;
                let attemptActive = true;
                const publicJob = new PublicActiveJob(
                  createJobRef({
                    nc: receivingNc,
                    queueType,
                    jobsBinding,
                    queueBinding,
                    seed: job.job() as JobSnapshot<unknown, unknown>,
                    lifecycle: receivingLifecycle,
                    updates: (options) =>
                      workerUpdates(
                        job.job().id,
                        receivingNc,
                        receivingLifecycle,
                        options,
                      ),
                  }),
                  job.job().payload,
                  job.context(),
                  () => job.isCancelled(),
                  {
                    heartbeat: () => wrapVoidTask(() => job.heartbeat()),
                    progress: (value: JobProgress) =>
                      wrapVoidTask(() => job.updateProgress(value)),
                    log: (entry: JobLogEntry) =>
                      wrapVoidTask(() => job.log(entry.level, entry.message)),
                    emitUpdate: (value: unknown) =>
                      AsyncResult.from(
                        Promise.resolve().then(() => {
                          if (!attemptActive) {
                            return Result.err(toUnexpectedError(
                              new Error("Job attempt is no longer active"),
                            ));
                          }
                          if (
                            !queueBinding.update ||
                            !queueBinding.updatesPrefix ||
                            !updateSchema
                          ) {
                            return Result.err(toUnexpectedError(
                              new Error(
                                `Job updates are unavailable for queue '${queueType}'`,
                              ),
                            ));
                          }
                          let encoded = value;
                          if (
                            typeof Reflect.get(updateSchema, "encode") ===
                              "function"
                          ) {
                            try {
                              encoded = (updateSchema as Codec<unknown>).encode(
                                value,
                              );
                            } catch (cause) {
                              return Result.err(toUnexpectedError(cause));
                            }
                          }
                          if (!isJsonValue(encoded)) {
                            return Result.err(
                              new ValidationError({
                                errors: [{
                                  path: "/",
                                  message:
                                    "Job update must be JSON-serializable",
                                }],
                              }),
                            );
                          }
                          const parsed =
                            typeof Reflect.get(updateSchema, "encode") ===
                                "function"
                              ? encoded
                              : parseSchema(updateSchema, encoded).take();
                          if (isErr(parsed)) return parsed;
                          updateSequence += 1;
                          try {
                            receivingNc.publish(
                              `${queueBinding.updatesPrefix}.${job.job().id}`,
                              new TextEncoder().encode(JSON.stringify({
                                jobId: job.job().id,
                                attempt: job.job().tries + 1,
                                sequence: updateSequence,
                                timestamp: new Date().toISOString(),
                                update: parsed,
                              })),
                            );
                            return Result.ok(undefined);
                          } catch (cause) {
                            return Result.err(toUnexpectedError(cause));
                          }
                        }),
                      ),
                    waitFor: (target, fn) => job.waitFor(target, fn),
                    redeliveryCount: job.redeliveryCount(),
                    signal: job.signal,
                    cancellationReason: () => job.cancellationToken().reason(),
                  },
                );

                const jobErrorContext = {
                  jobType: queueType,
                  requestId: job.context().requestId,
                  service: args.serviceName,
                  contractId: args.contractId,
                  contractDigest: args.contractDigest,
                  traceId: job.context().traceId,
                };

                let handled: unknown | Result<never, BaseError>;
                try {
                  handled = (await registration.handler(publicJob)).take();
                } catch (cause) {
                  const annotatedError = annotateHandlerBoundaryError(
                    cause,
                    jobErrorContext,
                  );
                  const signal = annotatedError instanceof RetryJobError
                    ? InternalJobProcessError.retryable
                    : InternalJobProcessError.failed;
                  throw signal(
                    serializeJobHandlerError(annotatedError),
                  );
                } finally {
                  attemptActive = false;
                  if (receivingLifecycle !== lifecycle) {
                    receivingLifecycle.stop();
                  }
                }
                if (isErr(handled)) {
                  const annotatedError = annotateHandlerBoundaryError(
                    handled.error,
                    jobErrorContext,
                  );
                  const signal = annotatedError instanceof RetryJobError
                    ? InternalJobProcessError.retryable
                    : InternalJobProcessError.failed;
                  throw signal(
                    serializeJobHandlerError(annotatedError),
                  );
                }
                return handled;
              },
            };
            startupCancellation.signal.throwIfAborted();
            if (args.transport instanceof TransportGenerationManager) {
              const host = startManagedJobWorkerHost({
                jobs: jobsBinding,
                workStream,
              }, {
                ...workerOptions,
                transport: args.transport,
                intakeOwner: args.intakeOwner,
                queueType,
                cancellationRegistry,
              });
              hosts.push(host);
              startingHosts.add(host);
              await host.ready;
            } else {
              const host = await startNatsWorkerHostFromBinding({
                jobs: jobsBinding,
                workStream,
              }, workerOptions);
              hosts.push(host);
              startingHosts.add(host);
            }
          }
        } catch (cause) {
          const stopResults = await Promise.allSettled(
            hosts.map((host) => host.stop()),
          );
          const stopErrors = stopResults
            .filter((result): result is PromiseRejectedResult =>
              result.status === "rejected"
            )
            .map((result) => result.reason);
          if (stopErrors.length > 0) {
            return Result.err(
              toUnexpectedError(new AggregateError([cause, ...stopErrors])),
            );
          }
          if (
            startupCancellation.signal.aborted &&
            cause === startupCancellation.signal.reason
          ) {
            return Result.ok(createNoopJobWorkerHost());
          }
          return Result.err(toUnexpectedError(cause));
        }

        activeHost = new JobWorkerHostAdapter({
          stop: () =>
            wrapVoidTask(async () => {
              for (const host of hosts) {
                await host.stop();
              }
            }),
          join: () => AsyncResult.ok(undefined),
        });
        return Result.ok(activeHost);
      })().finally(() => {
        startupPromise = undefined;
        startingHosts.clear();
      });

      return AsyncResult.from(startupPromise);
    },
    stop: () => {
      if (stopPromise) {
        return AsyncResult.from(stopPromise);
      }
      stopPromise = (async () => {
        startupCancellation.abort();
        const stoppingStartupHosts = Promise.allSettled(
          [...startingHosts].map((host) =>
            host.stop(startupCancellation.signal.reason)
          ),
        );
        const startup = startupPromise;
        if (startup) {
          const started = await startup;
          await stoppingStartupHosts;
          if (isErr(started)) {
            return Result.ok(undefined);
          }
        }
        if (!activeHost) {
          for (const subscription of updateSubscriptions) {
            subscription.unsubscribe();
          }
          updateSubscriptions.clear();
          lifecycle.stop();
          return Result.ok(undefined);
        }

        const host = activeHost;
        try {
          return await host.stop();
        } finally {
          for (const subscription of updateSubscriptions) {
            subscription.unsubscribe();
          }
          updateSubscriptions.clear();
          lifecycle.stop();
          if (activeHost === host) {
            activeHost = undefined;
          }
          stopPromise = undefined;
        }
      })();

      return AsyncResult.from(stopPromise);
    },
  };

  Object.defineProperty(jobsFacade, MANAGED_JOB_WORKERS, {
    value: managedWorkers,
    enumerable: false,
  });

  return jobsFacade as ManagedJobsFacade<TJobs, TClient>;
}

/**
 * Connects a service with caller-supplied runtime dependencies for tests and
 * Trellis-owned internals. This helper is intentionally not re-exported from
 * public package subpaths.
 *
 * @internal
 */
export function connectTrellisServiceWithRuntimeDeps<
  const TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
>(
  args: TrellisServiceConnectArgs<TContract>,
  deps: Partial<TrellisServiceRuntimeDeps>,
): AsyncResult<
  TrellisServiceSession<
    ParticipantOwnedApi<TContract>,
    ParticipantTrellisApi<TContract>,
    ParticipantJobsOf<TContract>,
    ParticipantKvOf<TContract>
  >,
  TransportError | UnexpectedError
> {
  return AsyncResult.from((async () => {
    const totalStartedAt = performance.now();
    try {
      type TOwnedApi = ParticipantOwnedApi<TContract>;
      type TTrellisApi = ParticipantTrellisApi<TContract>;

      const runtimeDeps = {
        ...(await loadDefaultServiceRuntimeDeps()),
        ...deps,
      } satisfies TrellisServiceRuntimeDeps;
      const serviceName = args.name ?? args.participant.identity;
      if (args.telemetry !== false && args.telemetry?.enabled !== false) {
        // Await the single owner before the first instrument starts.
        await runtimeDeps.initTelemetry?.(serviceName);
      }
      const identityAuth = await createAuth({
        sessionKeySeed: args.seed,
      });
      const sessionAuth = await createAuth({
        sessionKeySeed: base64urlEncode(
          crypto.getRandomValues(new Uint8Array(32)),
        ),
      });
      const bootstrapLog = resolveServiceLogger(args.runtime?.log);
      const bootstrapStartedAt = performance.now();
      const bootstrap = await fetchServiceBootstrapInfo({
        trellisUrl: args.trellisUrl,
        serviceName,
        name: args.name,
        contractId: args.participant.identity,
        contractDigest: participantEvidence(args.participant).packageDigest,
        contract: args.participant,
        identityAuth,
        sessionAuth,
        log: bootstrapLog,
      });
      recordTrellisDuration(
        "trellis.connect.duration",
        performance.now() - bootstrapStartedAt,
        {
          phase: "bootstrap",
          participantKind: "service",
          outcome: "ok",
        },
      );
      const authorizationContexts = new AuthorizationContextCache(
        args.trellisUrl,
      );
      authorizationContexts.setServerClockOffsetMs(
        bootstrap.serverClockOffsetMs,
      );
      const inboxPrefix = `_INBOX.${bootstrap.connectInfo.connectionId}`;
      await authorizationContexts.install(
        bootstrap.connectInfo.authorizationContext,
        {
          bootstrapJwt: bootstrap.connectInfo.jwt,
          bootstrapJwtExpiresAt: bootstrap.connectInfo.jwtExpiresAt,
        },
        authorizationContexts.correctedNowSeconds(),
        () => true,
        {
          connectionId: bootstrap.connectInfo.connectionId,
          loginSessionId: null,
          participantId: bootstrap.connectInfo.participantId,
          inboxPrefix,
          transports: bootstrap.connectInfo.transports,
        },
      );
      const verifiedContext = authorizationContexts.current();
      if (verifiedContext.context.participantId !== args.participant.identity) {
        throw new Error(
          "service authorization context belongs to another participant",
        );
      }
      if (
        !verifiedContext.context.deploymentId ||
        !verifiedContext.context.instanceId
      ) {
        throw new Error(
          "service authorization context is missing its deployment assignment",
        );
      }
      let nc: NatsConnection | undefined;
      let initialPrepared: TransportGenerationPrepared | undefined;
      let authorizationProviderCache: AuthorizationProviderCache | undefined;
      let stopContextRefresh: (() => void) | undefined;
      const connectionTelemetry = startConnectionTelemetry("service");
      /**
       * Logical provider core handoff: installed on each admitted generation by
       * the generation manager, never re-registered by application code.
       */
      let serviceFacade: ServiceProviderIngress | undefined;
      /**
       * Capture immutable CONNECT material for the newest desired service
       * context. Generation 1 and every automatic generation use this one path
       * so a candidate can only CONNECT with the exact context it reports.
       */
      const prepareServiceConnect = async (
        verified: AuthorizationCandidateSnapshot["verified"],
        runtime: AuthorizationCandidateSnapshot["runtime"],
        routingJwt: string,
      ): Promise<TransportGenerationPrepared> => {
        if (!runtime) {
          throw new Error("authorization runtime metadata is unavailable");
        }
        const policy = verified.context.transportAuthorization;
        const servers = selectRuntimeTransportServers(runtime.transports);
        const policyDigest = await transportAuthorizationDigestWasm(policy);
        const {
          authenticator: preparedAuthenticator,
          inboxPrefix: preparedInbox,
        } = await sessionAuth.natsConnectOptions({
          inboxPrefix: `_INBOX.${bootstrap.connectInfo.connectionId}`,
          contextDigest: () => verified.contextDigest,
          jwt: () => routingJwt,
          authorizationUsable: () =>
            authorizationProviderCache?.transportUsable() ?? true,
        });
        return {
          contextDigest: verified.contextDigest,
          policy,
          policyDigest,
          connect: {
            servers,
            authenticators: Array.isArray(preparedAuthenticator)
              ? preparedAuthenticator
              : [preparedAuthenticator],
            inboxPrefix: preparedInbox,
            maxReconnectAttempts: GENERATION_MAX_RECONNECT_ATTEMPTS,
            waitOnFirstConnect: DEFAULT_SERVICE_RUNTIME_WAIT_ON_FIRST_CONNECT,
          },
        };
      };
      const prepareServiceGeneration = (): Promise<
        TransportGenerationPrepared | undefined
      > => {
        try {
          return prepareServiceConnect(
            authorizationContexts.current(),
            authorizationContexts.runtimeBinding(),
            authorizationContexts.routingJwt(),
          );
        } catch {
          authorizationContexts.requestRefresh();
          return Promise.resolve(undefined);
        }
      };
      const generationManager = new TransportGenerationManager({
        kind: "service",
        nowSeconds: () => authorizationContexts.correctedNowSeconds(),
        desiredPolicy: () => {
          // Installed own authority the provider cache has withdrawn as unusable
          // (revoked or suspended) must not drive generation reconciliation or a
          // normal default connection: the retained signed bytes are
          // known-unusable. The existing healthy carrier and accepted work stay
          // intact while a private refresh candidate is verified and promoted;
          // promotion schedules adoption through `authorizationPromoted`.
          if (!authorizationProviderCache?.ownUsable()) return undefined;
          try {
            return authorizationContexts.current().context
              .transportAuthorization;
          } catch {
            return undefined;
          }
        },
        prepare: prepareServiceGeneration,
        open: (connect) =>
          runtimeDeps.connect({
            servers: connect.servers,
            maxReconnectAttempts: connect.maxReconnectAttempts ??
              GENERATION_MAX_RECONNECT_ATTEMPTS,
            ignoreAuthErrorAbort: true,
            waitOnFirstConnect: connect.waitOnFirstConnect ??
              DEFAULT_SERVICE_RUNTIME_WAIT_ON_FIRST_CONNECT,
            inboxPrefix: connect.inboxPrefix,
            authenticator: connect.authenticators,
          }),
        onPreActivate: async (generation) => {
          await serviceFacade?.install({
            id: generation.id,
            nc: generation.nc,
            contextDigest: generation.contextDigest,
            // Lease the exact admitted candidate directly; a lookup-by-id would
            // miss it because it is not yet published as the default.
            lease: () => generation.lease(),
            // Push each owning ingress's cleanup scope into the manager before
            // any install async work.
            registerDisposal: (handle) => generation.registerDisposal(handle),
          });
        },
        onDrain: (generation) => {
          serviceFacade?.retireIntake(generation.id);
        },
        onActivate: () => {
          // Recompute the admitted-transport gate from the new default.
          void applyServiceAdmission().catch(() => undefined);
        },
        log: bootstrapLog,
      });
      try {
        const natsStartedAt = performance.now();
        initialPrepared = await prepareServiceGeneration();
        if (!initialPrepared) {
          throw new Error("no current authorization context to connect with");
        }
        nc = await runtimeDeps.connect({
          servers: initialPrepared.connect.servers,
          maxReconnectAttempts: initialPrepared.connect.maxReconnectAttempts ??
            GENERATION_MAX_RECONNECT_ATTEMPTS,
          ignoreAuthErrorAbort: true,
          waitOnFirstConnect: initialPrepared.connect.waitOnFirstConnect ??
            DEFAULT_SERVICE_RUNTIME_WAIT_ON_FIRST_CONNECT,
          inboxPrefix: initialPrepared.connect.inboxPrefix,
          authenticator: initialPrepared.connect.authenticators,
        });
        const connectedNats = nc;
        authorizationProviderCache = await AuthorizationProviderCache.attach(
          connectedNats,
          authorizationContexts.bundle().authorizationRegistry,
          inboxPrefix,
          authorizationContexts,
        );
        await authorizationProviderCache.enableVerificationWorkers(
          args.runtime?.timeout ?? 30_000,
          args.runtime?.requestLimits,
          args.runtime?.maxVerificationWorkers,
          args.runtime?.verificationWorkerQueueAgeMs,
          args.runtime?.verificationWorkerBacklogDurationMs,
        );
        authorizationProviderCache.start();
        await authorizationProviderCache.waitReady();
        await authorizationProviderCache.retainOwnContext();
        // The provider cache is a logical-connection resource, not a property of
        // the first physical generation. The initial attachment may be reaped on
        // a rollover; stopping the cache then would block every later
        // authenticator, so bind its lifetime to the logical transport.
        void generationManager.closed().then(
          () => {
            stopContextRefresh?.();
            authorizationProviderCache?.stop();
          },
          () => {
            stopContextRefresh?.();
            authorizationProviderCache?.stop();
          },
        );
        recordTrellisDuration(
          "trellis.connect.duration",
          performance.now() - natsStartedAt,
          {
            phase: "nats_connect",
            participantKind: "service",
            outcome: "ok",
          },
        );
      } catch (cause) {
        connectionTelemetry.dispose();
        authorizationProviderCache?.stop();
        stopContextRefresh?.();
        if (nc && !nc.isClosed()) await nc.close();
        throw new TransportError({
          code: "trellis.runtime.connect_failed",
          message: "Trellis could not open the service runtime connection.",
          hint:
            "Retry the connection. If it keeps failing, check Trellis transport availability.",
          cause,
          context: {
            trellisUrl: args.trellisUrl,
            contractId: args.participant.identity,
            contractDigest: bootstrap.connectInfo.participantDigest,
          },
        });
      }

      if (!nc || !authorizationProviderCache) {
        throw new Error(
          "Trellis service runtime connection was not established",
        );
      }
      const serviceAuth: SessionAuth = {
        ...sessionAuth,
        authorizationProviderCache,
      };
      const transportState = new TransportAuthorizationState();
      let liveResourceBindings: ResourceBindings = bootstrap.binding.resources;
      const applyServiceAdmission = async (): Promise<void> => {
        if (authorizationContexts.storedContextDigest() === undefined) return;
        const generation = generationManager.currentGeneration();
        if (!generation || !generation.ready) {
          transportState.markDisconnected();
          return;
        }
        // The admitted-transport projection tracks the current generation's exact
        // verified admission rather than a fresh broker read of one socket.
        await transportState.recordAdmission({
          contextDigest: generation.contextDigest,
          policy: generation.admittedPolicy,
          allowed:
            authorizationContexts.current().context.transportAuthorization,
          nowUnixSeconds: authorizationContexts.correctedNowSeconds(),
        });
      };

      try {
        const contractRuntime = getParticipantRuntime(args.participant);
        const runtime = {
          ...(args.runtime ?? {}),
          api: bindApiRoutes(
            contractRuntime.ownedApi,
            bootstrap.binding.apiBindings,
          ) as TOwnedApi,
          trellisApi: bindApiRoutes(
            contractRuntime.api,
            bootstrap.binding.apiBindings,
          ) as TTrellisApi,
          operationDeploymentId: verifiedContext.context.deploymentId,
          operationConnectionId: verifiedContext.context.connectionId,
        };

        const service = await createConnectedService<
          TOwnedApi,
          TTrellisApi,
          ParticipantJobsOf<TContract>,
          ParticipantKvOf<TContract>
        >({
          name: serviceName,
          auth: serviceAuth,
          nc,
          telemetry: connectionTelemetry,
          inboxPrefix,
          contextDigest: () => authorizationContexts.current().contextDigest,
          operationConnectionId: verifiedContext.context.connectionId,
          contractId: args.participant.identity,
          contractDigest: bootstrap.connectInfo.participantDigest,
          participantDigest: bootstrap.connectInfo.participantDigest,
          contractJobs: contractRuntime.jobs as ParticipantJobsOf<
            TContract
          >,
          contractKv: contractRuntime.kv as ParticipantKvOf<
            TContract
          >,
          contractEventConsumers: contractRuntime.eventConsumers,
          apiBindings: bootstrap.binding.apiBindings,
          ephemeralEventNeeds: participantEphemeralEventNeeds(args.participant),
          onTransportEvent: (event) => {
            const type = (event as { type?: unknown } | null)?.type;
            if (
              type === "disconnect" || type === "disconnected" ||
              type === "reconnecting" || type === "forceReconnect"
            ) {
              transportState.markDisconnected();
            } else if (type === "reconnect") {
              void applyServiceAdmission().catch(() => undefined);
            }
          },
          runtime,
          bindings: bootstrap.binding.resources,
          liveBindings: () => liveResourceBindings,
          availability: participantAvailability(
            args.participant,
            bootstrap.binding.apiBindings,
            bootstrap.binding.resources,
            verifiedContext.context.grants.permissions,
          ),
          healthIdentity: {
            instanceId: verifiedContext.context.instanceId,
            deploymentId: verifiedContext.context.deploymentId,
          },
          authorizationProviderCache,
          transport: generationManager,
          // The initial heartbeat may run before generation 1 is activated, so
          // fall back to the connection that was just admitted.
          activeNats: () => {
            try {
              return generationManager.currentNats();
            } catch {
              return nc;
            }
          },
        });
        serviceFacade = service[SERVICE_PROVIDER_INGRESS];
        if (!initialPrepared) {
          throw new Error("missing prepared service generation");
        }
        await generationManager.initialize(nc, initialPrepared);
        // Bind the logical cache to the manager's published current transport
        // only after the initial generation is adopted: its first retained
        // publication pins existing fixed-mode coverage to that exact
        // generation, even though the socket identity is unchanged.
        authorizationProviderCache.followTransport(generationManager);
        service[SERVICE_TRANSPORT_LIFETIME] = generationManager;
        let installedAvailability = participantAvailability(
          args.participant,
          bootstrap.binding.apiBindings,
          bootstrap.binding.resources,
          verifiedContext.context.grants.permissions,
        );
        await applyServiceAdmission();
        authorizationProviderCache.onOwnInvalidated((reason) => {
          transitionConnectionAvailability(
            service.connection,
            false,
            reason,
          );
          installConnectionAvailability(
            service.connection,
            participantAvailability(args.participant, {}, {}, []),
          );
        });
        authorizationProviderCache.onOwnResumed(() => {
          if (service.connection.status.phase === "connected") {
            transitionConnectionAvailability(
              service.connection,
              true,
              "resumed",
            );
          }
          installConnectionAvailability(
            service.connection,
            installedAvailability,
          );
        });
        if (
          authorizationProviderCache.ownUsable() &&
          service.connection.status.phase === "connected"
        ) {
          transitionConnectionAvailability(
            service.connection,
            true,
            "connected",
          );
        }
        stopContextRefresh = startAuthorizationContextRefresh({
          trellisUrl: args.trellisUrl,
          credential: {
            loginSessionId: bootstrap.connectInfo.connectionId,
            proofAuth: sessionAuth,
          },
          runtime: { auth: sessionAuth },
          cache: authorizationContexts,
          refresh: async (shouldInstall) => {
            try {
              const next = await fetchServiceBootstrapInfo({
                trellisUrl: args.trellisUrl,
                serviceName,
                name: args.name,
                contractId: args.participant.identity,
                contractDigest: bootstrap.connectInfo.participantDigest,
                contract: args.participant,
                identityAuth,
                sessionAuth,
                log: bootstrapLog,
                connectionId: bootstrap.connectInfo.connectionId,
              });
              authorizationContexts.setServerClockOffsetMs(
                next.serverClockOffsetMs,
              );
              const context = await authorizationContexts.prepare(
                next.connectInfo.authorizationContext,
                {
                  bootstrapJwt: next.connectInfo.jwt,
                  bootstrapJwtExpiresAt: next.connectInfo.jwtExpiresAt,
                },
                next.serverNow,
                shouldInstall,
                {
                  connectionId: next.connectInfo.connectionId,
                  loginSessionId: null,
                  participantId: next.connectInfo.participantId,
                  inboxPrefix,
                  transports: next.connectInfo.transports,
                },
                (verified) => () => {
                  liveResourceBindings = next.binding.resources;
                  installedAvailability = participantAvailability(
                    args.participant,
                    next.binding.apiBindings,
                    next.binding.resources,
                    verified.context.grants.permissions,
                  );
                  installConnectionAvailability(
                    service.connection,
                    installedAvailability,
                  );
                  refreshApiRoutes(runtime.api, next.binding.apiBindings);
                  refreshApiRoutes(
                    runtime.trellisApi,
                    next.binding.apiBindings,
                  );
                },
              );
              return context;
            } catch (error) {
              if (error instanceof TrellisHttpError) {
                throw new AuthorizationContextRefreshError(
                  error.status,
                  error.code,
                );
              }
              throw error;
            }
          },
          onRefresh: async (context) => {
            await installAuthorizationRefresh({
              provider: authorizationProviderCache,
              origin: context,
              prepareConnect: (snapshot) =>
                prepareServiceConnect(
                  snapshot.verified,
                  snapshot.runtime,
                  snapshot.routing.bootstrapJwt,
                ),
              onPromoted: () => generationManager.authorizationPromoted(),
            });
            await transportState.recompute(
              context.context.transportAuthorization,
              authorizationContexts.correctedNowSeconds(),
            );
            // The generation manager opens a new generation when the promoted
            // policy needs it; routine renewal leaves the attachment untouched.
          },
          onTerminalFailure: async (error) => {
            // The authorization controller only reports an authoritative
            // terminal cause; carry the typed error into the logical lifetime so
            // `closed()` preserves it instead of a bare close.
            await generationManager.terminate(toUnexpectedError(error)).catch(
              () => undefined,
            );
            if (!nc.isClosed()) await nc.close();
          },
        });
        recordTrellisDuration(
          "trellis.connect.duration",
          performance.now() - totalStartedAt,
          {
            phase: "total",
            participantKind: "service",
            outcome: "ok",
          },
        );
        return Result.ok(service);
      } catch (cause) {
        await closeFailedServiceBootstrapConnection(nc);
        throw cause;
      }
    } catch (cause) {
      return Result.err(
        cause instanceof TransportError ? cause : toUnexpectedError(cause),
      );
    }
  })());
}

/**
 * Collect the `event:<Name>` descriptor names this participant explicitly
 * declares as subscribe needs. A declared durable consumer is not included:
 * `Consume` and `Subscribe` are independent authorities.
 */
function participantEphemeralEventNeeds(
  participant: unknown,
): ReadonlySet<string> {
  const needs = new Set<string>();
  const uses = Reflect.get(participant as object, "uses");
  if (!Array.isArray(uses)) return needs;
  for (const entry of uses) {
    const actions = Reflect.get(entry as object, "actions");
    if (!Array.isArray(actions)) continue;
    for (const action of actions) {
      const descriptorName = Reflect.get(action as object, "descriptorName");
      const direction = Reflect.get(action as object, "direction");
      if (
        direction === "subscribe" && typeof descriptorName === "string" &&
        descriptorName.startsWith("event:")
      ) {
        needs.add(descriptorName);
      }
    }
  }
  return needs;
}

/** Connected session implementation backing the public service type. */
export class TrellisServiceSession<
  TOwnedApi extends RuntimeApi = RuntimeApi,
  TTrellisApi extends RuntimeApi = TOwnedApi,
  TJobs extends ParticipantJobsMetadata = ParticipantJobsMetadata,
  TKv extends ParticipantKvMetadata = ParticipantKvMetadata,
> {
  readonly name: string;
  readonly auth: SessionAuth;
  readonly #runtime: TrellisServiceRuntimeFor<TOwnedApi & TTrellisApi>;
  readonly #nc: NatsConnection;
  readonly #handlerTrellis: Trellis<TTrellisApi, TKv, TJobs>;
  declare readonly [PROVIDER_CALLER]: ProviderCaller;
  declare readonly [SERVICE_PROVIDER_INGRESS]: ServiceProviderIngress;
  declare [SERVICE_TRANSPORT_LIFETIME]: ServiceTransportLifetime;
  /** Event lifecycle surface for service startup listeners and publishers. */
  readonly event: ActiveEventFacade<TTrellisApi>;
  readonly kv: ServiceKvFacade<TKv>;
  readonly store: Record<string, StoreHandle>;
  readonly jobs: ServiceJobsFacadeOf<
    TJobs,
    Trellis<TTrellisApi, TKv, TJobs>
  >;
  readonly health: ServiceHealth;
  readonly handle: TypedServiceHandleFacade<TOwnedApi, TTrellisApi, TKv, TJobs>;
  /** Framework-neutral lifecycle handle for the service runtime connection. */
  readonly connection: TrellisConnection;
  readonly #operationTransfer: ServiceTransfer;
  readonly #stopHealthPublishing: () => Promise<void>;
  readonly #managedJobWorkers: ManagedJobWorkers;
  readonly #log: LoggerLike;
  readonly #contractJobs: TJobs;
  readonly #jobsBinding?: ResourceBindingJobs;
  readonly #ownedOutboxDispatchers = new Set<OutboxDispatcher>();
  #waitPromise?: Promise<void>;
  #stopPromise?: Promise<void>;

  private constructor(
    name: string,
    auth: SessionAuth,
    nc: NatsConnection,
    runtime: TrellisServiceRuntimeFor<TOwnedApi & TTrellisApi>,
    event: ActiveEventFacade<TTrellisApi>,
    providerCaller: ProviderCaller,
    handlerTrellis: Trellis<TTrellisApi, TKv, TJobs>,
    kv: ServiceKvFacade<TKv>,
    contractJobs: TJobs,
    bindings: ResourceBindings,
    operationTransfer: ServiceTransfer,
    health: ServiceHealthRuntime,
    stopHealthPublishing: () => Promise<void>,
    connection: TrellisConnection,
    transport: TrellisTransportProvider,
    acquireTimeoutMs: number,
    log: LoggerLike,
    token: typeof trellisServiceConstructorToken,
  ) {
    if (token !== trellisServiceConstructorToken) {
      throw new TypeError("TrellisService instances are created by connect()");
    }
    const storeBindings = bindings.store ?? {};
    this.name = name;
    this.auth = auth;
    this.#nc = nc;
    this.#runtime = runtime;
    this.#handlerTrellis = handlerTrellis;
    Object.defineProperty(this, PROVIDER_CALLER, {
      value: providerCaller,
    });
    Object.defineProperty(this, SERVICE_PROVIDER_INGRESS, {
      value: {
        // Both the outbound framework surfaces and the operation runtime own
        // per-generation intake on the same generation.
        install: async (target) => {
          const outbound = providerCaller as ProviderIngressOwner;
          await outbound.installProviderIngress(target);
          await this.#runtime.installProviderIngress(target);
        },
        retireIntake: (id) => {
          (providerCaller as ProviderIngressOwner).retireProviderIngress(id);
          this.#runtime.retireProviderIngress(id);
        },
      } satisfies ServiceProviderIngress,
    });
    this.event = event;
    this.kv = kv;
    this.store = Object.fromEntries(
      Object.entries(storeBindings).map((
        [alias, binding],
      ) => [
        alias,
        new InternalStoreHandle(
          transport,
          acquireTimeoutMs,
          binding,
          storeHandleConstructorToken,
        ),
      ]),
    );
    this.#operationTransfer = operationTransfer;
    const jobs = createJobsFacade<
      TJobs,
      Trellis<TTrellisApi, TKv, TJobs>
    >({
      serviceName: name,
      contractId: health.contractId,
      contractDigest: health.contractDigest,
      nc,
      contractJobs,
      transport,
      acquireTimeoutMs,
      intakeOwner: this.#runtime,
      client: handlerTrellis,
      jobsBinding: bindings.jobs,
      workStream: bindings.jobs?.workStream,
    });
    this.jobs = jobs;
    this.#managedJobWorkers = jobs[MANAGED_JOB_WORKERS];
    this.#contractJobs = contractJobs;
    this.#jobsBinding = bindings.jobs;
    this.health = health;
    this.handle = this.#createHandleFacade() as TypedServiceHandleFacade<
      TOwnedApi,
      TTrellisApi,
      TKv,
      TJobs
    >;
    this.connection = connection;
    this.#stopHealthPublishing = stopHealthPublishing;
    this.#log = log.child({ service: name });
  }

  /**
   * Creates an explicit SQL outbox helper for service-owned transactions.
   * Services should create this at startup and close over it in handlers.
   */
  createSqlOutbox<TTx>(
    options: TrellisServiceSqlOutboxExecutorOptions<TTx>,
  ): SqlOutbox<TTx, TTrellisApi, TJobs> {
    const binding = this.#createSqlOutboxBinding(options);
    return this.#createSqlOutbox(binding);
  }

  /** Publishes a prepared event through the service runtime connection. */
  publishPrepared(
    event: PreparedTrellisEvent,
  ): AsyncResult<void, TransportError | UnexpectedError> {
    return this.#handlerTrellis.publishPrepared(event);
  }

  #createHandleFacade(): ServiceHandleFacade {
    const rpc: ServiceHandleFacade["rpc"] = {};
    for (const method of Object.keys(this.#runtime.api.rpc ?? {})) {
      addSurfaceLeaf(rpc, method, (handler) =>
        this.#runtime.mountRuntime(
          method,
          async ({ input, context }) =>
            await Promise.resolve(
              (handler as (
                args: unknown,
              ) =>
                | Promise<Result<unknown, BaseError>>
                | Result<unknown, BaseError>)({
                  input,
                  context,
                  client: this.#handlerTrellis,
                }),
            ),
        ));
    }

    const live: ServiceHandleFacade["live"] = {};
    for (const liveName of Object.keys(this.#runtime.api.lives ?? {})) {
      addSurfaceLeaf(
        live,
        liveName,
        (handler) =>
          this.#runtime.liveHandle(liveName).handle((context) =>
            (handler as (args: unknown) => unknown | Promise<unknown>)({
              ...context,
              client: this.#handlerTrellis,
            })
          ),
      );
    }

    const operation: Record<
      string,
      Record<string, ServiceHandleOperationLeaf>
    > = {};
    for (
      const operationName of Object.keys(this.#runtime.api.operations ?? {})
    ) {
      const registration = this.#operation(
        operationName as keyof TOwnedApi["operations"] & string,
      );
      const leaf: ServiceHandleOperationLeaf = Object.assign(
        (handler: (context: unknown) => unknown) =>
          registration.handle((context) =>
            handler({
              ...context,
              client: this.#handlerTrellis,
            })
          ),
        { control: registration.control, reconcile: registration.reconcile },
      );
      addSurfaceLeaf(operation, operationName, leaf);
    }

    return { rpc, live, operation };
  }

  #createSqlOutboxBinding<TTx>(
    options: TrellisServiceSqlOutboxOptions<TTx>,
  ): {
    readonly dialect: SqlDialect;
    readonly tables: SqlOutboxTables;
    readonly transaction: SqlOutboxTransactionRunner<TTx>;
    readonly dispatcher: OutboxDispatcher;
    readonly repository: SqlOutboxRepository;
  } {
    const tables = resolveSqlOutboxTables(options.tables);
    const executor = createSqlOutboxBaseExecutor(options);
    const repository = new SqlOutboxRepository(
      executor,
      options.dialect,
      tables,
    );
    const dispatchRuntime: OutboxDispatchRuntime = {
      publishPreparedEvent: (event) =>
        this.#handlerTrellis.publishPrepared(event),
      dispatchJobSubmission: (message) =>
        AsyncResult.from((async () => {
          try {
            if (!this.#jobsBinding) {
              return Result.ok({
                kind: "invalid",
                error: "Jobs bindings are unavailable",
              });
            }
            const jobsBinding = normalizeResourceJobsBinding(this.#jobsBinding);
            const queueBinding = jobsBinding.queues[message.name];
            if (!queueBinding) {
              return Result.ok({
                kind: "invalid",
                error:
                  `Jobs binding for queue '${message.name}' is unavailable`,
              });
            }

            let submission: InternalPreparedJobSubmission;
            try {
              submission = Value.Parse(
                PreparedJobSubmissionSchema,
                JSON.parse(message.payload),
              );
            } catch (cause) {
              return Result.ok({
                kind: "invalid",
                error: cause instanceof Error ? cause.message : String(cause),
              });
            }
            const expectedMode = message.kind === "job.create"
              ? "create"
              : "submit";
            const expectedSubject =
              `${queueBinding.publishPrefix}.${submission.jobId}.created`;
            if (
              submission.submissionId !== message.id ||
              submission.mode !== expectedMode ||
              submission.service !== jobsBinding.serviceName ||
              submission.queue !== message.name ||
              message.subject !== expectedSubject ||
              message.headers["request-id"] !==
                submission.context.requestId ||
              message.headers.traceparent !== submission.context.traceparent ||
              message.headers.tracestate !== submission.context.tracestate
            ) {
              return Result.ok({
                kind: "invalid",
                error: "Outbox job submission metadata is inconsistent",
              });
            }

            const manager = new InternalJobManager<unknown, unknown>({
              nc: jetstream(this.#nc),
              jobs: jobsBinding,
              keyCoordinator: createNatsJobKeyCoordinator(this.#nc),
            });
            if (submission.mode === "create") {
              const job = await manager.createPrepared(submission);
              return Result.ok({ kind: "accepted", jobId: job.id });
            }

            const outcome = await manager.submitPrepared(submission);
            if (outcome.kind === "accepted") {
              return Result.ok({
                kind: outcome.kind,
                jobId: outcome.job.id,
                ...(outcome.key ? { key: outcome.key } : {}),
              });
            }
            if (outcome.kind === "replaced") {
              return Result.ok({
                kind: outcome.kind,
                jobId: outcome.job.id,
                key: outcome.key,
                replaced: outcome.replaced,
              });
            }
            return Result.ok(outcome);
          } catch (cause) {
            if (cause instanceof JobNotEnqueuedError) {
              return Result.ok({
                kind: "rejected",
                reason: cause.reason,
                key: cause.key,
                active: cause.active,
                queued: cause.queued,
                limit: cause.limit,
                ...(cause.existingJobId
                  ? { existingJobId: cause.existingJobId }
                  : {}),
              });
            }
            if (cause instanceof ValidationError) {
              return Result.ok({ kind: "invalid", error: cause.message });
            }
            if (cause instanceof UnexpectedError) {
              return Result.err(cause);
            }
            return Result.err(toUnexpectedError(cause));
          }
        })()),
    };
    const dispatcher = new OutboxDispatcher(
      repository,
      dispatchRuntime,
      options.dispatcher,
    );
    this.#ownedOutboxDispatchers.add(dispatcher);
    return {
      dialect: options.dialect,
      tables,
      transaction: createSqlOutboxTransactionRunner(options),
      dispatcher,
      repository,
    };
  }

  #createSqlOutbox<TTx>(binding: {
    readonly dialect: SqlDialect;
    readonly tables: SqlOutboxTables;
    readonly transaction: SqlOutboxTransactionRunner<TTx>;
    readonly dispatcher: OutboxDispatcher;
    readonly repository: SqlOutboxRepository;
  }): SqlOutbox<TTx, TTrellisApi, TJobs> {
    return {
      jobSubmissionOutcome: (submissionId: string) =>
        AsyncResult.from((async () => {
          try {
            const message = await binding.repository.get(submissionId);
            if (message?.state !== "dispatched") {
              return Result.ok(undefined);
            }
            const outcome = message.outcome;
            if (
              !outcome || typeof outcome !== "object" ||
              !("kind" in outcome) || typeof outcome.kind !== "string"
            ) {
              return Result.err(toUnexpectedError(
                new Error(
                  `Outbox job submission '${submissionId}' has no valid outcome`,
                ),
              ));
            }
            return Result.ok({ ...outcome, kind: outcome.kind });
          } catch (cause) {
            return Result.err(toUnexpectedError(cause));
          }
        })()),
      transaction: <TResult>(
        work: (
          context: SqlOutboxTransactionContext<TTx, TTrellisApi, TJobs>,
        ) => Promise<TResult> | TResult,
      ) =>
        AsyncResult.from((() => {
          let enqueued = 0;
          const toResultError = (
            cause: unknown,
          ): Result<TResult, ValidationError | UnexpectedError> => {
            if (
              cause instanceof ValidationError ||
              cause instanceof UnexpectedError
            ) {
              return Result.err(cause);
            }
            return Result.err(toUnexpectedError(cause));
          };
          let transaction: Promise<TResult>;
          try {
            transaction = binding.transaction<TResult>(({ tx, executor }) => {
              const repository = new SqlOutboxRepository(
                executor,
                binding.dialect,
                binding.tables,
              );
              const event = createSqlOutboxEventEnqueueFacade({
                event: this.event,
                repository,
                onEnqueued: () => {
                  enqueued += 1;
                },
              });
              const job = createSqlOutboxJobEnqueueFacade({
                contractJobs: this.#contractJobs,
                jobsBinding: this.#jobsBinding,
                repository,
                onEnqueued: () => {
                  enqueued += 1;
                },
              });
              return work({ tx, event, job });
            });
          } catch (cause) {
            return Promise.resolve(toResultError(cause));
          }
          return transaction.then((result) => {
            if (enqueued > 0) binding.dispatcher.notify();
            return Result.ok(result);
          }, toResultError);
        })()),
    };
  }

  /**
   * Creates a short-lived receive transfer grant for a caller session.
   */
  createTransfer(args: {
    direction: "receive";
    store: string;
    key: string;
    sessionKey: string;
    connectionId: string;
    contextDigest: string;
    permission: PermissionAtom;
    requiredCapabilities?: readonly string[];
    inboxPrefix: string;
    expiresInMs?: number;
  }): AsyncResult<ReceiveTransferGrant, TransferError> {
    return AsyncResult.from(
      this.#operationTransfer.initiateDownload({
        store: args.store,
        key: args.key,
        sessionKey: args.sessionKey,
        connectionId: args.connectionId,
        contextDigest: args.contextDigest,
        permission: args.permission,
        requiredCapabilities: args.requiredCapabilities,
        inboxPrefix: args.inboxPrefix,
        expiresInMs: args.expiresInMs ?? 60_000,
      }),
    );
  }

  /**
   * Completes an operation from Trellis-owned runtime code that resolves
   * an operation from a separate RPC handler.
   *
   * @internal
   */
  completeOperation(
    operationId: string,
    output: unknown,
  ): AsyncResult<unknown, BaseError> {
    return AsyncResult.from((async () => {
      const completed = await this.#runtime.operations.complete(
        operationId,
        output,
      ).take();
      if (!isErr(completed)) return Result.ok(completed);

      const current = await this.#runtime.operations.get(operationId).take();
      if (!isErr(current) && current.state === "completed") {
        if (!operationOutputsEqual(current.output, output)) {
          return Result.err(
            new UnexpectedError({
              cause: new Error(
                "operation already completed with different output",
              ),
            }),
          );
        }
        return Result.ok(current);
      }

      return Result.err(completed.error);
    })());
  }

  static connect<
    const TContract extends GeneratedServiceParticipant<
      RuntimeApi,
      RuntimeApi | undefined,
      ParticipantJobsMetadata,
      ParticipantKvMetadata
    >,
  >(
    args: TrellisServiceConnectArgs<TContract>,
  ): AsyncResult<
    ConnectedTrellisService<TContract>,
    TransportError | UnexpectedError
  > {
    return AsyncResult.from((async () => {
      const connected = await connectTrellisServiceWithRuntimeDeps(args, {});
      if (isErr(connected)) {
        return connected;
      }
      const service = connected.unwrapOrElse(() => {
        throw new Error("Connected service result narrowed incorrectly");
      });
      return Result.ok(createProviderRuntime(service, args.participant));
    })());
  }

  async wait(): Promise<void> {
    this.#waitPromise ??= (async () => {
      try {
        if (!this.#stopPromise) {
          await this.#managedJobWorkers.start().orThrow();
        }
        // Supervise the logical connection: an authoritative terminal failure of
        // the logical transport must stop the service. A single physical
        // generation being reaped is not a stop.
        const closed = await (this[SERVICE_TRANSPORT_LIFETIME]?.closed() ??
          this.#nc.closed());
        if (closed instanceof Error) {
          throw closed;
        }
        if (!this.#stopPromise) {
          throw new Error("Trellis service connection closed unexpectedly");
        }
      } finally {
        await this.stop();
      }
    })();

    await this.#waitPromise;
  }

  async stop(): Promise<void> {
    this.#stopPromise ??= (async () => {
      const requests = this.#handlerTrellis.stopRequestListeners();
      this.connection.stopObserving();
      this.#handlerTrellis.stopEventListeners();
      for (const dispatcher of this.#ownedOutboxDispatchers) {
        dispatcher.stop();
      }
      this.#ownedOutboxDispatchers.clear();

      try {
        this.#log.info({ phase: "health" }, "Stopping service");
        await this.#stopHealthPublishing();
      } finally {
        try {
          this.#log.info({ phase: "jobs" }, "Stopping service");
          await this.#managedJobWorkers.stop().orThrow();
        } finally {
          try {
            this.#log.info({ phase: "transfers" }, "Stopping service");
            await this.#operationTransfer.stop();
          } finally {
            try {
              this.#log.info({ phase: "runtime" }, "Stopping service");
              await this.#runtime.stop(requests);
            } finally {
              this.#log.info({ phase: "connection" }, "Stopping service");
              await this.connection.close();
              this.#log.info({ phase: "closed" }, "Service stopped");
            }
          }
        }
      }
    })();

    await this.#stopPromise;
  }

  #operation<O extends keyof TOwnedApi["operations"] & string>(
    operation: O,
  ): ServiceOperationRegistration<TOwnedApi, TTrellisApi, O, TKv, TJobs> {
    const registration = this.#runtime.operationHandle(
      operation,
    ) as RootOperationRegistration<
      InferSchemaType<TOwnedApi["operations"][O]["input"]>,
      OperationProgressOf<TOwnedApi, O>,
      OperationOutputOf<TOwnedApi, O>,
      OperationTransferContextOf<TOwnedApi, O>,
      BaseError
    >;

    return {
      control: (operationId) =>
        registration.control(operationId) as ReturnType<
          OperationControlRegistration<TOwnedApi, O>["control"]
        >,
      reconcile: (operationId) => registration.reconcile(operationId),
      handle: (
        handler: (
          args:
            & OperationHandlerContext<
              InferSchemaType<TOwnedApi["operations"][O]["input"]>,
              OperationProgressOf<TOwnedApi, O>,
              OperationOutputOf<TOwnedApi, O>,
              OperationTransferContextOf<TOwnedApi, O>,
              BaseError
            >
            & { client: Trellis<TTrellisApi, TKv, TJobs> },
        ) => unknown | Promise<unknown>,
      ) =>
        registration.handle((context) =>
          handler({
            ...context,
            client: this.#handlerTrellis,
          })
        ),
    };
  }
}

/** Public factory for connecting a descriptor-defined Trellis service. */
export class TrellisService {
  private constructor() {}

  /** Connects a service and returns its provider-only runtime facade. */
  static connect<
    const TContract extends GeneratedServiceParticipant<
      RuntimeApi,
      RuntimeApi | undefined,
      ParticipantJobsMetadata,
      ParticipantKvMetadata
    >,
  >(
    args: TrellisServiceConnectArgs<TContract>,
  ): AsyncResult<
    ConnectedTrellisService<TContract>,
    TransportError | UnexpectedError
  > {
    return TrellisServiceSession.connect(args);
  }
}
