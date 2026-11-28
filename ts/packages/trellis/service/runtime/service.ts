// Every service serves requests: select the provider protocol build before
// anything uses the protocol.
import "../../auth/protocol_wasm_provider.ts";
import { jetstream } from "@nats-io/jetstream";
import { renderUnicodeCompact } from "uqr";
import {
  headers as natsHeaders,
  type MsgHdrs,
  type NatsConnection,
  type Subscription,
} from "@nats-io/nats-core";
import type { VerifiedAuthority } from "../../auth/authority.ts";
import type { StoreError } from "../../errors/index.ts";
import {
  observeTrellisConnection,
  type TrellisAvailability,
} from "../../connection.ts";
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
import type { TrellisAuth as SessionAuth } from "../../auth/session_auth.ts";
import { connectProvisioned } from "../../auth/connection.ts";
import { acceptServerCertificateFor } from "../../native/certificate_acceptance.ts";
import { discoverNativeOAuth } from "../../native/discovery.ts";
import { Type } from "typebox";
import { ContractResourceBindingsSchema } from "../../participant_runtime/schemas.ts";
import {
  fixedTransportProvider,
  TransportGenerationManager,
  type TransportLease,
  type TrellisTransportProvider,
} from "../../transport/generations.ts";
import type { InferSchemaType } from "../../participant.ts";
import { newId } from "../../values.ts";
import type { RuntimeApi } from "../../participant_runtime/api.ts";
import type {
  ParticipantJobsMetadata,
  ParticipantKvMetadata,
} from "../../participant_runtime/metadata.ts";
import type { ContractEventConsumers } from "../../participant_runtime/schemas.ts";
import {
  type GeneratedParticipant,
  getParticipantRuntime,
  participantAvailability,
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
import {
  ServiceLifecycle,
  type ServicePhase,
  type ServiceReadinessOpts,
} from "./lifecycle.ts";
import { createHealthHeartbeatPublisher } from "../../health_transport.ts";
import { API as WEBHOOKS_API } from "../../internal_sdk/generated/apis/webhooks/mod.js";
import { webhookHealthCheck } from "../../webhooks.ts";
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
} from "../../session.ts";
import {
  annotateHandlerBoundaryError,
  createTrellisInternal,
  logFailure,
} from "../../session.ts";
import { type DownloadSource, ServiceTransfer } from "./transfer.ts";
import { logger as noopLogger, type LoggerLike } from "../../globals.ts";
import {
  createProviderRuntime,
  PROVIDER_CALLER,
  SERVICE_DEPLOYMENT_ID,
  type ProviderCaller,
  type ProviderHandlerClient,
  type ProviderRuntime,
} from "../../provider.ts";
import { serviceRuntimeLogger } from "./logger.ts";
import {
  ConfigHandle,
  loadConfig,
  ownReading,
  type ParticipantConfig,
} from "../config.ts";
import { API as AUTH_API } from "../../internal_sdk/generated/apis/auth/mod.js";
import type { AuthOwnConfigResponse } from "../../internal_sdk/generated/types/index.js";
import {
  AuthError,
  TransferError,
  TransportError,
  UnexpectedError,
  ValidationError,
} from "../../errors/index.ts";
import {
  type ResourceExhaustedError,
  resourceExhausted,
} from "../../errors/ResourceExhaustedError.ts";
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
import { readJobHistory } from "./internal_jobs/history.ts";
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
import { initTelemetry } from "../../telemetry/init.ts";
import {
  connectWithServiceIdentity,
  serviceIdentityStatus,
  type TrellisServiceEnrollmentOpts,
} from "./enrollment.ts";
import { Backoff } from "../../backoff.ts";

const ServiceSignedAuthorizationSchema = Type.Object({
  participantId: Type.String(),
  participantDigest: Type.String(),
  resourceRuntime: ContractResourceBindingsSchema,
});

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
type RpcMethodInput<
  TA extends RuntimeApi,
  M extends RpcMethodName<TA>,
> = InferSchemaType<TA["rpc"][M]["input"]>;
type RpcMethodOutput<
  TA extends RuntimeApi,
  M extends RpcMethodName<TA>,
> = InferSchemaType<TA["rpc"][M]["output"]>;
type TrellisServiceRuntimeCreateOpts<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi | undefined = TOwnedApi,
> = {
  log?: LoggerLike | false;
  timeout?: number;
  stream?: string;
  api: TOwnedApi;
  trellisApi?: TTrellisApi;
  version?: string;
  health?: TrellisServiceHealthOpts;
  readiness?: ServiceReadinessOpts;
  stopTimeoutMs?: number;
};

export type TrellisServiceHealthOpts = {
  publishIntervalMs?: number;
};

export type TrellisServiceRuntimeOpts = {
  log?: LoggerLike | false;
  timeout?: number;
  stream?: string;
  version?: string;
  health?: TrellisServiceHealthOpts;
  /** A readiness probe: a file and/or an HTTP endpoint that report serving. */
  readiness?: ServiceReadinessOpts;
  /**
   * How long a stop waits for requests, event deliveries and jobs in flight
   * before it hands the rest back to other instances. 25 seconds by default,
   * inside Kubernetes' default 30-second grace period.
   */
  stopTimeoutMs?: number;
};

function resolveServiceLogger(log?: LoggerLike | false): LoggerLike {
  return log === false ? noopLogger : (log ?? serviceRuntimeLogger);
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
/** @internal The live settings handle, when the participant declares settings. */
export const SERVICE_CONFIG: unique symbol = Symbol("trellis.service.config");
/** Retires an enrolled instance with a memory-only key at a graceful stop. @internal */
export const SERVICE_RETIRE_OWN: unique symbol = Symbol(
  "trellis.service.retireOwn",
);

export const SERVICE_TRANSPORT_LIFETIME: unique symbol = Symbol(
  "trellis.service.transportLifetime",
);

/** Logical transport lifetime surface used by the service wait loop. @internal */
type ServiceTransportLifetime = {
  closed(): Promise<void | Error>;
  isClosed(): boolean;
  retireAllIntake(): Promise<void>;
};

/** @internal */
export type ServiceProviderIngress = {
  install(target: {
    id: number;
    nc: NatsConnection;
    contextDigest: string;
    retainAuthority?: () => VerifiedAuthority;
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
  /** Stops taking durable deliveries; resolves once those in hand are acked. */
  drainEventListeners(): Promise<void>;
  /** Hands the durable deliveries still in hand back at once. */
  abandonEventDeliveries(): void;
  /** Resolves once every started durable consumer exists at the broker. */
  eventListenersAttached(): Promise<void>;
  /** Takes no durable delivery until `serveEventDeliveries()`. */
  holdEventDeliveries(): void;
  /** Takes durable deliveries from here on. */
  serveEventDeliveries(): void;
  /** Ends every owned Live session as `local_shutdown`. */
  endLiveSessions(): Promise<void>;
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
  readonly #alias: string;

  constructor(
    transport: TrellisTransportProvider,
    acquireTimeoutMs: number,
    alias: string,
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
    this.#alias = alias;
    this.binding = binding;
  }

  open(): AsyncResult<TypedStore, StoreError> {
    return TypedStore.open(this.#transport, this.binding.name, {
      ttlMs: this.binding.ttlMs,
      maxObjectBytes: this.binding.maxObjectBytes,
      maxTotalBytes: this.binding.maxTotalBytes,
      bindOnly: true,
      acquireTimeoutMs: this.#acquireTimeoutMs,
      resourceName: this.#alias,
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
            resourceName: alias,
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
  /** Trellis HTTPS origin, or literal loopback HTTP origin for local development. */
  trellisUrl: string;
  participant: GeneratedServiceParticipant<TOwnedApi, TTrellisApi>;
  name?: string;
  /**
   * Provisioned service identity. Without it the service enrolls itself with
   * `enrollment` or `TRELLIS_ENROLLMENT_SECRET`; giving both is an error.
   */
  seed?: string;
  /** Enroll as a new instance with the deployment's enrollment secret. */
  enrollment?: TrellisServiceEnrollmentOpts;
  /** Discovery and connection timeout in milliseconds. */
  timeout?: number;
  /** Cancels discovery and connection establishment. */
  signal?: AbortSignal;
  /** Controls automatic telemetry initialization. Enabled by default. */
  telemetry?: TrellisServiceConnectTelemetryOpts;
  /** Configures the connected service runtime. */
  runtime?: TrellisServiceRuntimeOpts;
};

/** Controls automatic telemetry initialization for `TrellisService.connect()`. */
export type TrellisServiceConnectTelemetryOpts =
  | false
  | {
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
> = HandlerTrellis<TTrellisApi> &
  ServiceHandlerResources<TKv, TJobs, TTrellisApi>;

export type GeneratedServiceParticipant<
  TOwnedApi extends RuntimeApi,
  TTrellisApi extends RuntimeApi | undefined,
  TJobs extends ParticipantJobsMetadata = ParticipantJobsMetadata,
  TKv extends ParticipantKvMetadata = ParticipantKvMetadata,
> = Omit<
  GeneratedParticipant,
  "companion" | "kind" | "requiredCapabilities"
> & {
  readonly kind: "service";
  readonly requiredCapabilities: readonly string[];
  readonly optionalCapabilities: readonly string[];
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
}
  ? Extract<TOwnedApi, RuntimeApi>
  : RuntimeApi;

type ParticipantTrellisApi<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata
  >,
> = TContract extends {
  readonly __runtimeTypes?: { api: infer TApi };
}
  ? Extract<TApi, RuntimeApi>
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
/**
 * Names of the RPCs a generated service participant owns, for example
 * `"Orders.Create"`; the `M` of {@link RpcHandler}.
 */
export type RpcName<
  TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
> = RpcMethodName<ParticipantOwnedApi<TContract>>;

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
> = (args: {
  event: ServiceEventOf<ParticipantTrellisApi<TContract>, E>;
  context: EventListenerContext;
  client: ServiceHandlerClient<TContract>;
}) => MaybeAsync<void, BaseError>;

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
  args: OperationHandlerContext<
    InferSchemaType<ParticipantOwnedApi<TContract>["operations"][O]["input"]>,
    OperationProgressOf<ParticipantOwnedApi<TContract>, O>,
    OperationOutputOf<ParticipantOwnedApi<TContract>, O>,
    OperationTransferContextOf<ParticipantOwnedApi<TContract>, O>,
    OperationHandlerErrorOf<ParticipantOwnedApi<TContract>, O>,
    OperationUpdateOf<ParticipantOwnedApi<TContract>, O>
  > & {
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
  context: LiveHandlerContext<
    LiveInputOf<ParticipantOwnedApi<TContract>, F>,
    LiveEventOf<ParticipantOwnedApi<TContract>, F>
  > & { client: ServiceHandlerClient<TContract> },
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
type ServiceEventOf<TA extends RuntimeApi, E extends ServiceEventName<TA>> =
  TA["events"][E] extends EventDesc<infer TEvent>
    ? InferSchemaType<TEvent>
    : never;
type ServiceEventPayloadOf<
  TA extends RuntimeApi,
  E extends ServiceEventName<TA>,
> = ServiceEventOf<TA, E> & Record<string, unknown>;

/** Runs SQL outbox work inside a caller-owned service database transaction. */
export type SqlOutboxTransactionRunner<TTx> = <TResult>(
  work: (context: {
    tx: TTx;
    executor: SqlExecutor;
  }) => Promise<TResult> | TResult,
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
  TrellisServiceSqlOutboxCommonOptions & {
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
    work: (
      context: SqlOutboxTransactionContext<TTx, TEventApi, TJobs>,
    ) => Promise<TResult> | TResult,
  ): AsyncResult<TResult, ValidationError | UnexpectedError>;
  /** Returns the durable queue-admission outcome once a job submission dispatches. */
  jobSubmissionOutcome(
    submissionId: string,
  ): AsyncResult<OutboxJobDispatchOutcome | undefined, UnexpectedError>;
};

const MANAGED_JOB_WORKERS = Symbol("trellis.managedJobWorkers");

type ManagedJobWorkers = {
  start(): AsyncResult<JobWorkerHostAdapter, BaseError>;
  /** Stop taking jobs; resolves once every running job has settled. */
  drain(): Promise<void>;
  stop(): AsyncResult<void, BaseError>;
};

type ManagedJobsFacade<
  TJobs extends ParticipantJobsMetadata,
  TClient,
> = ServiceJobsFacadeOf<TJobs, TClient> & {
  [MANAGED_JOB_WORKERS]: ManagedJobWorkers;
};

type ServiceHandleOperationLeaf = ((
  handler: (context: unknown) => unknown,
) => Promise<void>) &
  Pick<
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
type SurfaceKeysForGroup<
  TKeys extends string,
  TGroup extends string,
> = TKeys extends string
  ? SurfaceGroupName<TKeys> extends TGroup
    ? TKeys
    : never
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
      ]: OperationHandleFn<TOwnedApi, TTrellisApi, O, TKv, TJobs>;
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
> = OperationControlRegistration<TOwnedApi, O> &
  ((
    handler: (
      context: OperationHandlerContext<
        InferSchemaType<TOwnedApi["operations"][O]["input"]>,
        OperationProgressOf<TOwnedApi, O>,
        OperationOutputOf<TOwnedApi, O>,
        OperationTransferContextOf<TOwnedApi, O>,
        OperationHandlerErrorOf<TOwnedApi, O>,
        OperationUpdateOf<TOwnedApi, O>
      > & { client: Trellis<TTrellisApi, TKv, TJobs> },
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
      args: OperationHandlerContext<
        InferSchemaType<TOwnedApi["operations"][O]["input"]>,
        OperationProgressOf<TOwnedApi, O>,
        OperationOutputOf<TOwnedApi, O>,
        OperationTransferContextOf<TOwnedApi, O>,
        OperationHandlerErrorOf<TOwnedApi, O>,
        OperationUpdateOf<TOwnedApi, O>
      > & { client: Trellis<TTrellisApi, TKv, TJobs> },
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
> = ((handler: OperationHandler<TContract, O>) => Promise<void>) &
  OperationControlRegistration<ParticipantOwnedApi<TContract>, O>;

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
  /** Trellis HTTPS origin, or literal loopback HTTP origin for local development. */
  trellisUrl: string;
  participant: TContract;
  name?: string;
  /**
   * Provisioned service identity. Without it the service enrolls itself with
   * `enrollment` or `TRELLIS_ENROLLMENT_SECRET`; giving both is an error.
   */
  seed?: string;
  /** Enroll as a new instance with the deployment's enrollment secret. */
  enrollment?: TrellisServiceEnrollmentOpts;
  /** Discovery and connection timeout in milliseconds. */
  timeout?: number;
  /** Cancels discovery and connection establishment. */
  signal?: AbortSignal;
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
  /** Additional raw-transport hook for application-level bookkeeping. @internal */
  onTransportEvent?: (event: unknown) => void;
  /** Process-local connection handle started before the transport connected. @internal */
  telemetry?: ReturnType<typeof startConnectionTelemetry>;
  /** The process's lifecycle, started before the transport connected. @internal */
  lifecycle?: ServiceLifecycle;
  /** Live settings, whose rejections the `settings` health check reports. @internal */
  config?: ConfigHandle<Record<string, unknown>>;
}): Promise<TrellisServiceSession<TOwnedApi, TTrellisApi, TJobs, TKv>> {
  const resolvedLog = resolveServiceLogger(args.runtime.log);
  const lifecycle =
    args.lifecycle ?? new ServiceLifecycle(args.runtime.readiness, resolvedLog);
  const liveBindings = args.liveBindings ?? (() => args.bindings);
  // Resource handles follow the logical connection's current generation; a
  // runtime without a generation manager wraps its single connection.
  const serviceTransport: TrellisTransportProvider =
    args.transport ?? fixedTransportProvider(args.nc);
  const acquireTimeoutMs = args.runtime.timeout ?? 30_000;
  const onTransportEvent = (event: unknown) => {
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
  const currentApi = (args.runtime.trellisApi ??
    args.runtime.api) as TOwnedApi & TTrellisApi;
  const runtimeApi = currentApi as TOwnedApi & TTrellisApi;

  const runtime = TrellisServiceRuntime.create(
    args.name,
    args.nc,
    {
      sessionKey: args.auth.sessionKey,
      sign: args.auth.sign,
      contextDigest: args.contextDigest,
      authorityOwner: args.auth.authorityOwner,
    },
    {
      log: resolvedLog,
      timeout: args.runtime.timeout,
      stream: args.runtime.stream,
      api: runtimeApi,
      contractId: args.contractId,
      contractDigest: args.contractDigest,
      inboxPrefix: args.inboxPrefix,
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
      authorityOwner: args.auth.authorityOwner,
    },
    {
      log: resolvedLog,
      timeout: args.runtime.timeout,
      stream: args.runtime.stream,
      api: runtimeApi,
      contractId: args.contractId,
      contractDigest: args.contractDigest,
      inboxPrefix: args.inboxPrefix,
      eventConsumers: {
        metadata: args.contractEventConsumers,
        bindings: args.bindings.eventConsumers,
      },
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
    phase: () => lifecycle.phase,
  });
  health.add("nats", () => ({
    status:
      serviceTransport.isClosed() || serviceTransport.currentNats().isClosed()
        ? "failed"
        : "ok",
    ...(serviceTransport.isClosed()
      ? { summary: "NATS connection closed" }
      : {}),
  }));

  // A revision the service's own checks rejected degrades its health until
  // one is accepted.
  const settings = args.config;
  if (settings) health.add("settings", () => settings.healthCheck());

  // Each webhook endpoint feeds the service's health with the check Trellis
  // keeps for it, so a wrong or missing secret shows on the service.
  if (args.healthIdentity) {
    for (const [name, consumer] of Object.entries(
      args.contractEventConsumers ?? {},
    )) {
      if (!consumer.uses?.[WEBHOOKS_API.identity]) continue;
      health.add(
        `webhook:${name}`,
        webhookHealthCheck(
          serviceTransport,
          args.inboxPrefix,
          args.healthIdentity.deploymentId,
          name,
        ),
      );
    }
  }

  const heartbeatEnabled = args.healthIdentity !== undefined;
  let healthPublishTimer: ReturnType<typeof setInterval> | undefined;
  const healthPublishingAbort = new AbortController();
  const publishHealthSample = createHealthHeartbeatPublisher({
    nc: args.nc,
    auth: args.auth,
    transport: serviceTransport,
    inboxPrefix: args.inboxPrefix,
  });
  let publishingHeartbeat: Promise<void> | undefined;
  let publishAgain = false;
  const publishHealthHeartbeat = (): Promise<void> => {
    if (!args.healthIdentity || healthPublishingAbort.signal.aborted) {
      return Promise.resolve();
    }
    if (publishingHeartbeat) {
      // A sample taken before a phase change would hide it; send another.
      publishAgain = true;
      return publishingHeartbeat;
    }
    publishingHeartbeat = (async () => {
      let lease: TransportLease | undefined;
      try {
        lease = await serviceTransport.acquireCurrent({
          signal: healthPublishingAbort.signal,
          deadlineMs: Date.now() + acquireTimeoutMs,
          background: true,
        });
        if (healthPublishingAbort.signal.aborted) return;
        const sample = await health.sample();
        if (healthPublishingAbort.signal.aborted) return;
        // Pin the admitted generation through sampling and the JetStream PubAck.
        await publishHealthSample(sample);
      } catch (error) {
        resolvedLog.warn(
          { error },
          "Failed to build or publish health heartbeat",
        );
      } finally {
        lease?.release();
        publishingHeartbeat = undefined;
      }
      if (publishAgain) {
        publishAgain = false;
        await publishHealthHeartbeat();
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
          alias,
          binding,
          storeHandleConstructorToken,
        ),
      ]),
    ),
    ...(args.healthIdentity
      ? {
          operationStagingBucket: `trellis_operation_staging_${args.healthIdentity.deploymentId}`,
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
    lifecycle,
    args.runtime.stopTimeoutMs ?? 25_000,
    trellisServiceConstructorToken,
  ]) as TrellisServiceSession<TOwnedApi, TTrellisApi, TJobs, TKv>;
  resources.handlerResources = {
    kv: service.kv,
    store: service.store,
    jobs: service.jobs,
  };
  resources.transfer = operationTransfer;
  Object.defineProperty(service, SERVICE_DEPLOYMENT_ID, {
    value: args.healthIdentity?.deploymentId,
  });

  if (heartbeatEnabled) {
    // A phase change is reported at once, so a watcher sees it move.
    lifecycle.subscribe(() => void publishHealthHeartbeat());
    // So is what became of a revision of its settings.
    settings?.onOutcome(() => void publishHealthHeartbeat());
    // Generation 1 activates after construction, so admission must not block it.
    void publishHealthHeartbeat();
    healthPublishTimer = setInterval(() => {
      void publishHealthHeartbeat();
    }, health.publishIntervalMs);
    void serviceTransport
      .closed()
      .then(stopHealthPublishing, stopHealthPublishing);
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
  const source = args.event as Record<string, Record<string, ServiceEventLeaf>>;

  for (const [groupName, leaves] of Object.entries(source)) {
    const group: Record<string, SqlOutboxEventEnqueueLeaf> = {};
    for (const [leafName, leaf] of Object.entries(leaves)) {
      group[leafName] = {
        enqueue: (payload) =>
          AsyncResult.from(
            (async () => {
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
            })(),
          ),
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

function createSqlOutboxJobEnqueueFacade<
  TJobs extends ParticipantJobsMetadata,
>(args: {
  contractJobs: TJobs;
  jobsBinding?: ResourceBindingJobs;
  repository: SqlOutboxRepository;
  onEnqueued(): void;
}): SqlOutboxJobEnqueueFacade<TJobs> {
  const facade: Record<string, Record<string, unknown>> = {};

  for (const queueType of Object.keys(
    args.contractJobs as Record<string, unknown>,
  )) {
    const jobsBinding = args.jobsBinding;
    const queueBinding = jobsBinding?.queues[queueType];
    const queue = queueType;
    facade[queue] = {
      create: (payload: unknown) => {
        if (!queueBinding) {
          return AsyncResult.err(
            new ValidationError({
              errors: [
                {
                  path: "/",
                  message: `Jobs binding unavailable for queue '${queue}'`,
                },
              ],
            }),
          );
        }
        const submissionId = ulid();
        const jobId = newId("job");
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
            traceparent: submission.context.traceparent,
            ...(submission.context.tracestate
              ? { tracestate: submission.context.tracestate }
              : {}),
          },
        };

        return AsyncResult.from(
          (async () => {
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
          })(),
        );
      },
      submit: (payload: unknown) => {
        if (!queueBinding) {
          return AsyncResult.err(
            new ValidationError({
              errors: [
                {
                  path: "/",
                  message: `Jobs binding unavailable for queue '${queue}'`,
                },
              ],
            }),
          );
        }
        const submissionId = ulid();
        const jobId = newId("job");
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
            traceparent: submission.context.traceparent,
            ...(submission.context.tracestate
              ? { tracestate: submission.context.tracestate }
              : {}),
          },
        };

        return AsyncResult.from(
          (async () => {
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
          })(),
        );
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
  return AsyncResult.from(
    (async () => {
      try {
        await task();
        return okVoid();
      } catch (cause) {
        return Result.err(toUnexpectedError(cause));
      }
    })(),
  );
}

function isTerminalJobState(
  state: string,
): state is TerminalJob<unknown, unknown>["state"] {
  return (
    state === "completed" ||
    state === "failed" ||
    state === "cancelled" ||
    state === "expired" ||
    state === "skipped" ||
    state === "stale" ||
    state === "dead" ||
    state === "dismissed"
  );
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
    return (
      Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => operationOutputsEqual(value, right[index]))
    );
  }
  if (typeof left === "object") {
    if (Object.getPrototypeOf(left) !== Object.prototype) return false;
    if (Object.getPrototypeOf(right) !== Object.prototype) return false;
    const leftRecord = left as Record<string, unknown>;
    const rightRecord = right as Record<string, unknown>;
    const leftKeys = Object.keys(leftRecord);
    const rightKeys = Object.keys(rightRecord);
    return (
      leftKeys.length === rightKeys.length &&
      leftKeys.every(
        (key) =>
          Object.hasOwn(rightRecord, key) &&
          operationOutputsEqual(leftRecord[key], rightRecord[key]),
      )
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
  return (
    suffix.length === 2 &&
    suffix[0] === event.jobId &&
    suffix[1] === event.eventType
  );
}

function snapshotFromLifecycleEvent<TPayload, TResult>(
  current: JobSnapshot<TPayload, TResult>,
  event: InternalJobEvent<TPayload, TResult>,
): JobSnapshot<TPayload, TResult> {
  if (
    event.service !== current.service ||
    event.jobType !== current.type ||
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
      return event.payload === undefined
        ? base
        : {
            ...base,
            payload: event.payload,
          };
    case "started":
      return { ...base, startedAt: event.timestamp };
    case "progress":
      return event.progress === undefined
        ? base
        : {
            ...base,
            progress: event.progress,
          };
    case "logged":
      return event.logs === undefined
        ? base
        : {
            ...base,
            logs: [...(current.logs ?? []), ...event.logs],
          };
    case "waiting": {
      if (event.waitEdge === undefined) return base;
      const waitingOn = (current.waitingOn ?? []).filter(
        (edge) => edge.id !== event.waitEdge?.id,
      );
      return { ...base, waitingOn: [...waitingOn, event.waitEdge] };
    }
    case "resumed":
      return event.waitEdge === undefined
        ? base
        : {
            ...base,
            waitingOn: (current.waitingOn ?? []).filter(
              (edge) => edge.id !== event.waitEdge?.id,
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
      return event.error === undefined
        ? base
        : {
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
      JobSnapshot<TPayload, TResult> | undefined;
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
              !event ||
              !subjectMatchesLifecycleEvent(msg.subject, queueBinding, event)
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
        JobSnapshot<TPayload, TResult> | undefined;
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
        JobSnapshot<TPayload, TResult> | undefined;
      if (current && isTerminalJobSnapshot(current)) {
        return Promise.resolve(current);
      }
      if (stopped) {
        return Promise.reject(
          toUnexpectedError(new Error("job lifecycle tracker stopped")),
        );
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
  readHistory: (
    queue: string,
    jobId: string,
  ) => Promise<Array<{ seq: number; data: Uint8Array }>>;
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
    AsyncResult.from(
      (async () => {
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
            const event = await getLatestLifecycleEvent(
              undefined,
              "JOBS",
              args.queueBinding.publishPrefix,
              args.seed,
              () => args.readHistory(args.queueType, args.seed.id),
            );
            if (event) {
              current = snapshotFromLifecycleEvent(
                args.seed,
                event as InternalJobEvent<TPayload, TResult>,
              );
            }
          } else {
            current =
              args.lifecycle.get<TPayload, TResult>({
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
      })(),
    );

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
            : (lifecycle.apply(event) ?? current);
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
        return Result.err(
          toUnexpectedError(
            new Error("Job updates are not configured for this queue"),
          ),
        );
      }
      if (!/^[^.>*\s]+$/u.test(args.jobId)) {
        return Result.err(
          new ValidationError({
            errors: [
              {
                path: "/jobId",
                message: "Job id must be one NATS token",
              },
            ],
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
              const lifecycleAttempt =
                args.lifecycle.attempt({
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
  log: LoggerLike;
  nc: NatsConnection;
  contractJobs: TJobs;
  readHistory: (
    queue: string,
    jobId: string,
  ) => Promise<Array<{ seq: number; data: Uint8Array }>>;
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
  const handlers = new Map<
    string,
    {
      handler: RegisteredJobHandler<unknown, unknown>;
      concurrency: number;
    }
  >();
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
  // Started hosts that can stop taking jobs without cancelling running ones.
  const drainable: Array<{ drain(): Promise<void> }> = [];
  let startupPromise:
    Promise<Result<JobWorkerHostAdapter, BaseError>> | undefined;
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
        return AsyncResult.err(
          toUnexpectedError(
            new Error(`Job updates are unavailable for queue '${queueType}'`),
          ),
        );
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
        AsyncResult.from(
          (async () => {
            let lease: TransportLease | undefined;
            try {
              if (!jobsBinding) {
                return Result.err(
                  toUnexpectedError(new Error("Jobs bindings are unavailable")),
                );
              }
              const queueBinding = jobsBinding.queues[queueType];
              if (!queueBinding) {
                return Result.err(
                  toUnexpectedError(
                    new Error(
                      `Jobs binding for queue '${queueType}' is unavailable`,
                    ),
                  ),
                );
              }

              lease = await acquireSubmissionTransport();
              const submissionManager = lease
                ? manager.withTransport(
                    jetstream(lease.nc),
                    createNatsJobKeyCoordinator(lease.nc),
                  )
                : manager;
              const created = await submissionManager.create(
                queueType,
                payload,
              );
              return Result.ok(
                createJobRef({
                  readHistory: args.readHistory,
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
                }),
              );
            } catch (cause) {
              if (
                cause instanceof JobNotEnqueuedError ||
                cause instanceof TransportError
              ) {
                return Result.err(cause);
              }
              return Result.err(
                resourceExhausted("jobs", queueType, cause) ??
                  toUnexpectedError(cause),
              );
            } finally {
              lease?.release();
            }
          })(),
        ),
      submit: (payload) =>
        AsyncResult.from(
          (async () => {
            let lease: TransportLease | undefined;
            try {
              if (!jobsBinding) {
                return Result.err(
                  toUnexpectedError(new Error("Jobs bindings are unavailable")),
                );
              }
              const queueBinding = jobsBinding.queues[queueType];
              if (!queueBinding) {
                return Result.err(
                  toUnexpectedError(
                    new Error(
                      `Jobs binding for queue '${queueType}' is unavailable`,
                    ),
                  ),
                );
              }

              lease = await acquireSubmissionTransport();
              const submissionManager = lease
                ? manager.withTransport(
                    jetstream(lease.nc),
                    createNatsJobKeyCoordinator(lease.nc),
                  )
                : manager;
              const outcome = await submissionManager.submit(
                queueType,
                payload,
              );
              if (outcome.kind === "accepted") {
                return Result.ok({
                  kind: "accepted",
                  key: outcome.key,
                  ref: createJobRef({
                    readHistory: args.readHistory,
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
                    readHistory: args.readHistory,
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
              return Result.err(
                resourceExhausted("jobs", queueType, cause) ??
                  toUnexpectedError(cause),
              );
            } finally {
              lease?.release();
            }
          })(),
        ),
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
    drain: async () => {
      // Workers still starting have taken no job: cancel their startup.
      if (startupPromise) {
        await managedWorkers.stop();
        return;
      }
      await Promise.allSettled(drainable.map((host) => host.drain()));
    },
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
          return Result.err(
            toUnexpectedError(
              new Error(
                "Jobs infrastructure bindings are unavailable for this service",
              ),
            ),
          );
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
                return AsyncResult.err(
                  toUnexpectedError(
                    new Error(
                      `Job updates are unavailable for queue '${queueType}'`,
                    ),
                  ),
                );
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
              getLatestLifecycleEvent: async (job) =>
                getLatestLifecycleEvent(
                  undefined,
                  "JOBS",
                  queueBinding.publishPrefix,
                  job,
                  () => args.readHistory(queueType, job.id),
                ),
              nats: args.nc,
              instanceId: ulid(),
              queueTypes: [queueType],
              queueConcurrency: {
                [queueType]: registration.concurrency,
              },
              manager,
              heartbeatPublisher: args.nc,
              getProjectedJob: (job) =>
                Promise.resolve(
                  lifecycle.get({
                    service: job.service,
                    jobType: job.type,
                    id: job.id,
                  }),
                ),
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
                    readHistory: args.readHistory,
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
                            return Result.err(
                              toUnexpectedError(
                                new Error("Job attempt is no longer active"),
                              ),
                            );
                          }
                          if (
                            !queueBinding.update ||
                            !queueBinding.updatesPrefix ||
                            !updateSchema
                          ) {
                            return Result.err(
                              toUnexpectedError(
                                new Error(
                                  `Job updates are unavailable for queue '${queueType}'`,
                                ),
                              ),
                            );
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
                                errors: [
                                  {
                                    path: "/",
                                    message:
                                      "Job update must be JSON-serializable",
                                  },
                                ],
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
                              new TextEncoder().encode(
                                JSON.stringify({
                                  jobId: job.job().id,
                                  attempt: job.job().tries + 1,
                                  sequence: updateSequence,
                                  timestamp: new Date().toISOString(),
                                  update: parsed,
                                }),
                              ),
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
                // The job ID is the reference Console and the CLI show.
                const logJobFailure = (error: BaseError) =>
                  logFailure(args.log, error, {
                    route: queueType,
                    requestId: jobErrorContext.requestId,
                    traceId: jobErrorContext.traceId,
                    service: args.serviceName,
                    extra: {
                      job_id: job.job().id,
                      tries: job.job().tries,
                      retry: error instanceof RetryJobError,
                    },
                  });

                let handled: unknown | Result<never, BaseError>;
                try {
                  handled = (await registration.handler(publicJob)).take();
                } catch (cause) {
                  const annotatedError = annotateHandlerBoundaryError(
                    cause,
                    jobErrorContext,
                  );
                  logJobFailure(annotatedError);
                  const signal =
                    annotatedError instanceof RetryJobError
                      ? InternalJobProcessError.retryable
                      : InternalJobProcessError.failed;
                  throw signal(serializeJobHandlerError(annotatedError));
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
                  logJobFailure(annotatedError);
                  const signal =
                    annotatedError instanceof RetryJobError
                      ? InternalJobProcessError.retryable
                      : InternalJobProcessError.failed;
                  throw signal(serializeJobHandlerError(annotatedError));
                }
                return handled;
              },
            };
            startupCancellation.signal.throwIfAborted();
            if (args.transport instanceof TransportGenerationManager) {
              const host = startManagedJobWorkerHost(
                {
                  jobs: jobsBinding,
                  workStream,
                },
                {
                  ...workerOptions,
                  transport: args.transport,
                  intakeOwner: args.intakeOwner,
                  queueType,
                  cancellationRegistry,
                },
              );
              hosts.push(host);
              drainable.push(host);
              startingHosts.add(host);
              await host.ready;
            } else {
              const host = await startNatsWorkerHostFromBinding(
                {
                  jobs: jobsBinding,
                  workStream,
                },
                workerOptions,
              );
              hosts.push(host);
              startingHosts.add(host);
            }
          }
        } catch (cause) {
          const stopResults = await Promise.allSettled(
            hosts.map((host) => host.stop()),
          );
          const stopErrors = stopResults
            .filter(
              (result): result is PromiseRejectedResult =>
                result.status === "rejected",
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
            host.stop(startupCancellation.signal.reason),
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

function connectServiceSession<
  const TContract extends GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >,
>(
  args: TrellisServiceConnectArgs<TContract>,
): AsyncResult<
  TrellisServiceSession<
    ParticipantOwnedApi<TContract>,
    ParticipantTrellisApi<TContract>,
    ParticipantJobsOf<TContract>,
    ParticipantKvOf<TContract>
  >,
  AuthError | TransportError | UnexpectedError
> {
  return AsyncResult.from(
    (async () => {
      let connected: Awaited<ReturnType<typeof connectProvisioned>> | undefined;
      let lifecycle: ServiceLifecycle | undefined;
      const telemetry = startConnectionTelemetry("service");
      try {
        const name = args.name ?? args.participant.identity;
        lifecycle = new ServiceLifecycle(
          args.runtime?.readiness,
          resolveServiceLogger(args.runtime?.log),
        );
        if (args.telemetry !== false && args.telemetry?.enabled !== false)
          await initTelemetry(name);
        let ingress: ServiceProviderIngress | undefined;
        // A development Trellis's own certificate is accepted like a browser
        // would: by its fingerprint, asked once and remembered.
        await acceptServerCertificateFor(
          args.trellisUrl,
          args.participant.identity,
        );
        const discovered = await discoverNativeOAuth(args.trellisUrl, {
          signal: args.signal,
        });
        if (isErr(discovered)) throw discovered.error;
        const server = discovered.unwrapOrElse(() => {
          throw new Error("Native discovery result narrowed incorrectly");
        });
        const provisioned = {
          servers: [...server.natsServers],
          trellisInstanceId: server.trust.instanceId,
          audienceNatsAccount: server.trust.account,
          issuers: [server.trust.issuer],
          rotationSequence: server.trust.rotationSequence,
          natsSentinel: server.natsSentinel,
          seed: "",
          timeout: args.timeout,
          signal: args.signal,
        };
        const connect = () =>
          connectProvisioned(provisioned, "service", {
            onPreActivate: async (generation, retainAuthority) => {
              if (ingress) {
                // Readmitted with only the right to submit a build, this
                // build no longer answers for its instance: the deployment
                // moved past it. It stops rather than linger without
                // authority; a current build installs itself on connect.
                const lease = retainAuthority();
                const metadata =
                  lease.authority.extensions["trellis.nativeParticipant.v1"];
                lease.release();
                if (
                  metadata &&
                  typeof metadata === "object" &&
                  !Array.isArray(metadata) &&
                  metadata.participantDigest === ""
                ) {
                  const error = new Error(
                    "This build is no longer installed for its instance; restart it with the deployment's current build",
                  );
                  void connected?.manager.terminate(error);
                  throw error;
                }
              }
              await ingress?.install({
                id: generation.id,
                nc: generation.nc,
                contextDigest: generation.contextDigest,
                retainAuthority,
                lease: () => generation.lease(),
                registerDisposal: (handle) =>
                  generation.registerDisposal(handle),
              });
            },
            onDrain: (generation) => ingress?.retireIntake(generation.id),
          });
        const identity = await connectWithServiceIdentity({
          seed: args.seed,
          enrollment: args.enrollment,
          trellisUrl: server.issuer,
          origin: new URL(server.issuer).origin,
          signal: args.signal,
          participantId: args.participant.identity,
          participantDigest: args.participant.digest,
          log: resolveServiceLogger(args.runtime?.log),
          connect: async (seed) => {
            provisioned.seed = seed;
            connected = await connect();
            return connected;
          },
        });
        connected = identity.connected;
        let own = connected.owner.own();
        let binding = own.authority.binding;
        own.release();
        let authorization = Value.Parse(
          ServiceSignedAuthorizationSchema,
          connected.context.registry.authorization,
        );
        // A build whose participant differs from the instance's installed
        // revision submits itself, then connects again under the new revision.
        // A service claim has nothing installed yet, so whatever connects
        // first submits itself the same way.
        while (
          binding.kind === "service" &&
          (binding.participantId === "" ||
            binding.participantId === args.participant.identity) &&
          authorization.participantDigest !== args.participant.digest
        ) {
          await awaitServiceUpdate(
            connected,
            args.participant,
            resolveServiceLogger(args.runtime?.log),
            (text) => lifecycle?.status(text),
            args.signal,
          );
          await connected.manager.close();
          connected = await connect();
          own = connected.owner.own();
          binding = own.authority.binding;
          own.release();
          authorization = Value.Parse(
            ServiceSignedAuthorizationSchema,
            connected.context.registry.authorization,
          );
        }
        if (binding.kind !== "service")
          throw new Error("ServiceBindingRequired");
        if (
          binding.participantId !== args.participant.identity ||
          authorization.participantId !== binding.participantId
        )
          throw new Error("ServiceImplementationMismatch");
        const contract = getParticipantRuntime(args.participant);
        const manager = connected.manager;
        const owner = connected.owner;
        const configFields = args.participant.config ?? {};
        let config: ConfigHandle<Record<string, unknown>> | undefined;
        let readOwnConfig:
          ((atConnect: boolean) => Promise<AuthOwnConfigResponse>) | undefined;
        if (Object.keys(configFields).length > 0) {
          const configClient = createTrellisInternal(
            args.participant.identity,
            manager.currentNats(),
            {
              sessionKey: connected.auth.sessionKey,
              sign: connected.auth.sign,
              contextDigest: () => owner.ownDigest ?? "",
              authorityOwner: connected.auth.authorityOwner,
            },
            { inboxPrefix: connected.inboxPrefix, transport: manager },
          );
          readOwnConfig = (atConnect) =>
            configClient.requestOwnConfig(atConnect);
          config = await loadConfig({
            participantId: args.participant.identity,
            fields: configFields,
            read: readOwnConfig,
            report: (outcome) => configClient.reportOwnConfig(outcome),
            log: resolveServiceLogger(args.runtime?.log),
            status: (text) => lifecycle?.status(text),
            ...(args.signal ? { signal: args.signal } : {}),
          });
        }
        const service = await createConnectedService<
          ParticipantOwnedApi<TContract>,
          ParticipantTrellisApi<TContract>,
          ParticipantJobsOf<TContract>,
          ParticipantKvOf<TContract>
        >({
          name,
          nc: manager.currentNats(),
          auth: connected.auth,
          inboxPrefix: connected.inboxPrefix,
          contextDigest: () => owner.ownDigest ?? "",
          operationConnectionId: connected.context.runtimeId,
          contractId: binding.participantId,
          contractDigest: authorization.participantDigest,
          participantDigest: authorization.participantDigest,
          contractJobs: contract.jobs as ParticipantJobsOf<TContract>,
          contractKv: contract.kv as ParticipantKvOf<TContract>,
          contractEventConsumers: contract.eventConsumers,
          ephemeralEventNeeds: participantEphemeralEventNeeds(args.participant),
          runtime: {
            ...args.runtime,
            api: contract.ownedApi as ParticipantOwnedApi<TContract>,
            trellisApi: contract.api as ParticipantTrellisApi<TContract>,
          },
          bindings: {
            ...authorization.resourceRuntime,
            kv: authorization.resourceRuntime.kv ?? {},
            store: authorization.resourceRuntime.store ?? {},
          },
          liveBindings: () => {
            const resources = Value.Parse(
              ServiceSignedAuthorizationSchema,
              connected!.context.registry.authorization,
            ).resourceRuntime;
            return {
              ...resources,
              kv: resources.kv ?? {},
              store: resources.store ?? {},
            };
          },
          availability: participantAvailability(
            args.participant,
            authorization.resourceRuntime,
          ),
          healthIdentity: {
            instanceId: binding.instanceId,
            deploymentId: binding.deploymentId,
          },
          transport: manager,
          activeNats: () => manager.currentNats(),
          telemetry,
          lifecycle,
          ...(config ? { config } : {}),
        });
        ingress = service[SERVICE_PROVIDER_INGRESS];
        const generation = manager.currentGeneration();
        if (!generation) throw new Error("AdmissionUnavailable");
        await ingress.install({
          id: generation.id,
          nc: generation.nc,
          contextDigest: generation.contextDigest,
          lease: () => generation.lease(),
          registerDisposal: (handle) => generation.registerDisposal(handle),
        });
        service[SERVICE_TRANSPORT_LIFETIME] = manager;
        if (identity.enrolled) {
          // An enrolled instance that cannot reconnect may have been retired
          // while it was away; its key then never connects again, so the
          // service ends and a restart enrolls a new instance.
          let away = 0;
          let checking = false;
          const watch = setInterval(() => {
            if (manager.isClosed()) return clearInterval(watch);
            if (manager.currentGeneration()) return void (away = 0);
            if (++away < 2 || checking) return;
            checking = true;
            serviceIdentityStatus({
              trellisUrl: server.issuer,
              origin: new URL(server.issuer).origin,
              seed: identity.seed,
            })
              .then((state) => {
                if (state !== "retired" && state !== "removed") return;
                clearInterval(watch);
                void manager.terminate(
                  new Error(
                    "This instance was retired or removed while it was disconnected; start the process again to enroll a new instance",
                  ),
                );
              })
              .catch(() => {})
              .finally(() => {
                checking = false;
              });
          }, 2_500);
          watch.unref?.();
          void manager.closed().finally(() => clearInterval(watch));
        }
        if (identity.retireOnStop) {
          // The key lives only in this process, so its instance could never
          // connect again once the process ends.
          const retiring = createTrellisInternal(
            args.participant.identity,
            manager.currentNats(),
            {
              sessionKey: connected.auth.sessionKey,
              sign: connected.auth.sign,
              contextDigest: () => owner.ownDigest ?? "",
              authorityOwner: connected.auth.authorityOwner,
            },
            { inboxPrefix: connected.inboxPrefix, transport: manager },
          );
          service[SERVICE_RETIRE_OWN] = () => retiring.requestRetireOwn();
        }
        if (config && readOwnConfig) {
          const read = readOwnConfig;
          service[SERVICE_CONFIG] = config;
          config.watch({
            transport: manager,
            subject: AUTH_API.actions["event:Config.Changed"].subject,
            matches: (event) =>
              event.deploymentId === binding.deploymentId &&
              (event.instanceId === undefined ||
                event.instanceId === binding.instanceId),
            read: async (atConnect) => ownReading(await read(atConnect)),
            timeoutMs: args.runtime?.timeout ?? 30_000,
          });
          // A connection that ends without stop() must not leave the watch retrying.
          const watching = config;
          void manager.closed().finally(() => watching.retire());
        }
        transitionConnectionAvailability(service.connection, true, "connected");
        return Result.ok(service);
      } catch (cause) {
        telemetry.dispose();
        lifecycle?.set("stopped");
        await connected?.manager.close();
        return Result.err(
          cause instanceof AuthError || cause instanceof TransportError
            ? cause
            : toUnexpectedError(cause),
        );
      }
    })(),
  );
}

/**
 * Submit this build's IDL to Auth and wait until it is installed for this
 * instance. An update that asks for new access waits for an administrator;
 * its review link is logged and printed as a terminal QR code.
 */
async function awaitServiceUpdate(
  connected: Awaited<ReturnType<typeof connectProvisioned>>,
  participant: { identity: string; packageEvidence: unknown },
  log: LoggerLike,
  status: (text: string) => void,
  signal?: AbortSignal,
): Promise<void> {
  const client = createTrellisInternal(
    participant.identity,
    connected.manager.currentNats(),
    {
      sessionKey: connected.auth.sessionKey,
      sign: connected.auth.sign,
      contextDigest: () => connected.owner.ownDigest ?? "",
      authorityOwner: connected.auth.authorityOwner,
    },
    { inboxPrefix: connected.inboxPrefix, transport: connected.manager },
  );
  const submit = async () =>
    (
      await client.requestServiceUpdate("ServiceUpdates.Submit", {
        participantId: participant.identity,
        packageEvidence: participant.packageEvidence,
      })
    ).update;
  try {
    let update = await submit();
    let shown: string | undefined;
    const backoff = new Backoff(1_000, 15_000);
    for (;;) {
      if (update.state === "applied") return;
      if (update.state === "rejected")
        throw new Error(
          `An administrator rejected this build's update (${update.reviewUrl ?? `trellis svc ${update.deploymentId} updates`})`,
        );
      if (update.state === "superseded") {
        update = await submit();
        continue;
      }
      if (shown !== update.updateId) {
        shown = update.updateId;
        const reviewUrl = update.reviewUrl;
        status(
          `Waiting for approval: ${reviewUrl ?? `trellis svc ${update.deploymentId} approve`}`,
        );
        log.warn(
          { participant: participant.identity, reviewUrl },
          "This build asks for new access; waiting for an administrator to approve it",
        );
        console.error(
          reviewUrl === undefined
            ? `\n${participant.identity} needs an administrator to approve its update with\n  trellis svc ${update.deploymentId} approve`
            : `\n${participant.identity} needs an administrator to approve its update.\nOpen ${reviewUrl}\nor scan:\n${renderUnicodeCompact(reviewUrl)}`,
        );
      }
      // An abort during the last Status request has already fired.
      signal?.throwIfAborted();
      await new Promise<void>((resolve, reject) => {
        const abort = () => {
          clearTimeout(timer);
          reject(signal?.reason);
        };
        const timer = setTimeout(() => {
          signal?.removeEventListener("abort", abort);
          resolve();
        }, backoff.failed());
        signal?.addEventListener("abort", abort, { once: true });
      });
      update = (
        await client.requestServiceUpdate("ServiceUpdates.Status", {
          updateId: update.updateId,
        })
      ).update;
    }
  } catch (cause) {
    // Approval retires this session, sometimes before Submit's reply arrives.
    // Connecting again shows whether the update installed; if not, Submit
    // returns the same update.
    if (cause instanceof TransportError) return;
    throw cause;
  }
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
        direction === "subscribe" &&
        typeof descriptorName === "string" &&
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
  declare [SERVICE_CONFIG]: ConfigHandle<Record<string, unknown>> | undefined;
  declare [SERVICE_RETIRE_OWN]: (() => Promise<void>) | undefined;

  /** Settings Trellis keeps for this service. */
  get config(): ParticipantConfig<Record<string, unknown>> {
    // A service without settings still has a handle of its own, so what it
    // registers or derives goes with it when it stops.
    return (this[SERVICE_CONFIG] ??= new ConfigHandle<Record<string, unknown>>(
      {},
      { values: {}, revision: {} },
      this.#log,
    ));
  }
  /** Event lifecycle surface for service startup listeners and publishers. */
  readonly event: ActiveEventFacade<TTrellisApi>;
  readonly kv: ServiceKvFacade<TKv>;
  readonly store: Record<string, StoreHandle>;
  readonly jobs: ServiceJobsFacadeOf<TJobs, Trellis<TTrellisApi, TKv, TJobs>>;
  readonly health: ServiceHealth;
  readonly handle: TypedServiceHandleFacade<TOwnedApi, TTrellisApi, TKv, TJobs>;
  /** Framework-neutral lifecycle handle for the service runtime connection. */
  readonly connection: TrellisConnection;
  readonly #operationTransfer: ServiceTransfer;
  readonly #stopHealthPublishing: () => Promise<void>;
  readonly #managedJobWorkers: ManagedJobWorkers;
  readonly #contractJobs: TJobs;
  readonly #jobsBinding?: ResourceBindingJobs;
  readonly #ownedOutboxDispatchers = new Set<OutboxDispatcher>();
  readonly #log: LoggerLike;
  readonly #lifecycle: ServiceLifecycle;
  readonly #stopTimeoutMs: number;
  /** Handler registrations still subscribing; serving waits for them. */
  readonly #registrations = new Set<Promise<unknown>>();
  /** The first registration that failed; the service then never serves. */
  #registrationFailure?: { error: unknown };
  /**
   * Provider intake being installed, by generation id. A stop retires it at
   * once and waits for the work it took.
   */
  readonly #installing = new Map<Promise<void>, number>();
  /** Set when a stop begins: no generation takes new intake after that. */
  #intakeFenced = false;
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
    lifecycle: ServiceLifecycle,
    stopTimeoutMs: number,
    token: typeof trellisServiceConstructorToken,
  ) {
    if (token !== trellisServiceConstructorToken) {
      throw new TypeError("TrellisService instances are created by connect()");
    }
    const storeBindings = bindings.store ?? {};
    this.name = name;
    this.auth = auth;
    this.#nc = nc;
    this.#log = log;
    this.#lifecycle = lifecycle;
    this.#stopTimeoutMs = stopTimeoutMs;
    this.#runtime = runtime;
    this.#handlerTrellis = handlerTrellis;
    Object.defineProperty(this, PROVIDER_CALLER, {
      value: providerCaller,
    });
    // Durable deliveries, like jobs, wait until the service serves.
    (providerCaller as ProviderIngressOwner).holdEventDeliveries();
    Object.defineProperty(this, SERVICE_PROVIDER_INGRESS, {
      value: {
        // Both the outbound framework surfaces and the operation runtime own
        // per-generation intake on the same generation.
        install: async (target) => {
          // A stopping service takes no new work on a later generation either.
          if (this.#intakeFenced) return;
          const outbound = providerCaller as ProviderIngressOwner;
          // Each owner registers its intake here before its first await, so a
          // stop can wait for the work even of an install it cut short.
          const attached: ProviderGenerationAttachment[] = [];
          const owned: typeof target = {
            ...target,
            registerDisposal: (handle) => {
              attached.push(handle);
              return target.registerDisposal?.(handle) ?? true;
            },
          };
          const installing = (async () => {
            try {
              attached.push(await outbound.installProviderIngress(owned));
              if (!this.#intakeFenced)
                attached.push(
                  await this.#runtime.installProviderIngress(owned),
                );
            } catch (error) {
              // Retired by the stop partway through: nothing more to install.
              if (!this.#intakeFenced) throw error;
            }
            // A stop that began meanwhile retired this intake; wait for the
            // work it took before it did.
            if (this.#intakeFenced)
              await Promise.allSettled(
                attached.map((attachment) => attachment.intakeStopped),
              );
          })();
          this.#installing.set(installing, target.id);
          try {
            await installing;
          } finally {
            this.#installing.delete(installing);
          }
        },
        retireIntake: (id) => this.#retireIntake(id),
      } satisfies ServiceProviderIngress,
    });
    this.event = event;
    this.kv = kv;
    this.store = Object.fromEntries(
      Object.entries(storeBindings).map(([alias, binding]) => [
        alias,
        new InternalStoreHandle(
          transport,
          acquireTimeoutMs,
          alias,
          binding,
          storeHandleConstructorToken,
        ),
      ]),
    );
    this.#operationTransfer = operationTransfer;
    const jobs = createJobsFacade<TJobs, Trellis<TTrellisApi, TKv, TJobs>>({
      serviceName: name,
      contractId: health.contractId,
      contractDigest: health.contractDigest,
      log,
      nc,
      contractJobs,
      transport,
      acquireTimeoutMs,
      intakeOwner: this.#runtime,
      readHistory: (queue, jobId) =>
        readJobHistory(
          (input) => this.#runtime.requestJobHistory(input),
          queue,
          jobId,
        ),
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
  ): AsyncResult<
    void,
    TransportError | ResourceExhaustedError | UnexpectedError
  > {
    return this.#handlerTrellis.publishPrepared(event);
  }

  /** Stops new provider intake on one generation, for both intake owners. */
  #retireIntake(id: number): void {
    (this[PROVIDER_CALLER] as ProviderIngressOwner).retireProviderIngress(id);
    this.#runtime.retireProviderIngress(id);
  }

  /** Keeps a registration until it settles, so serving can wait for it. */
  #registering<T>(registration: Promise<T>): Promise<T> {
    this.#registrations.add(registration);
    void registration.then(
      () => this.#registrations.delete(registration),
      (error: unknown) => {
        this.#registrations.delete(registration);
        this.#registrationFailure ??= { error };
      },
    );
    return registration;
  }

  #createHandleFacade(): ServiceHandleFacade {
    const rpc: ServiceHandleFacade["rpc"] = {};
    for (const method of Object.keys(this.#runtime.api.rpc ?? {})) {
      addSurfaceLeaf(rpc, method, (handler) =>
        this.#registering(
          this.#runtime.mountRuntime(
            method,
            async ({ input, context }) =>
              await Promise.resolve(
                (
                  handler as (
                    args: unknown,
                  ) =>
                    | Promise<Result<unknown, BaseError>>
                    | Result<unknown, BaseError>
                )({
                  input,
                  context,
                  client: this.#handlerTrellis,
                }),
              ),
          ),
        ),
      );
    }

    const live: ServiceHandleFacade["live"] = {};
    for (const liveName of Object.keys(this.#runtime.api.lives ?? {})) {
      addSurfaceLeaf(live, liveName, (handler) =>
        this.#registering(
          this.#runtime.liveHandle(liveName).handle((context) =>
            (handler as (args: unknown) => unknown | Promise<unknown>)({
              ...context,
              client: this.#handlerTrellis,
            }),
          ),
        ),
      );
    }

    const operation: Record<
      string,
      Record<string, ServiceHandleOperationLeaf>
    > = {};
    for (const operationName of Object.keys(
      this.#runtime.api.operations ?? {},
    )) {
      const registration = this.#operation(
        operationName as keyof TOwnedApi["operations"] & string,
      );
      const leaf: ServiceHandleOperationLeaf = Object.assign(
        (handler: (context: unknown) => unknown) =>
          this.#registering(
            registration.handle((context) =>
              handler({
                ...context,
                client: this.#handlerTrellis,
              }),
            ),
          ),
        { control: registration.control, reconcile: registration.reconcile },
      );
      addSurfaceLeaf(operation, operationName, leaf);
    }

    return { rpc, live, operation };
  }

  #createSqlOutboxBinding<TTx>(options: TrellisServiceSqlOutboxOptions<TTx>): {
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
        AsyncResult.from(
          (async () => {
            try {
              if (!this.#jobsBinding) {
                return Result.ok({
                  kind: "invalid",
                  error: "Jobs bindings are unavailable",
                });
              }
              const jobsBinding = normalizeResourceJobsBinding(
                this.#jobsBinding,
              );
              const queueBinding = jobsBinding.queues[message.name];
              if (!queueBinding) {
                return Result.ok({
                  kind: "invalid",
                  error: `Jobs binding for queue '${message.name}' is unavailable`,
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
              const expectedMode =
                message.kind === "job.create" ? "create" : "submit";
              const expectedSubject = `${queueBinding.publishPrefix}.${submission.jobId}.created`;
              if (
                submission.submissionId !== message.id ||
                submission.mode !== expectedMode ||
                submission.service !== jobsBinding.serviceName ||
                submission.queue !== message.name ||
                message.subject !== expectedSubject ||
                message.headers["request-id"] !==
                  submission.context.requestId ||
                message.headers.traceparent !==
                  submission.context.traceparent ||
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
          })(),
        ),
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
        AsyncResult.from(
          (async () => {
            try {
              const message = await binding.repository.get(submissionId);
              if (message?.state !== "dispatched") {
                return Result.ok(undefined);
              }
              const outcome = message.outcome;
              if (
                !outcome ||
                typeof outcome !== "object" ||
                !("kind" in outcome) ||
                typeof outcome.kind !== "string"
              ) {
                return Result.err(
                  toUnexpectedError(
                    new Error(
                      `Outbox job submission '${submissionId}' has no valid outcome`,
                    ),
                  ),
                );
              }
              return Result.ok({ ...outcome, kind: outcome.kind });
            } catch (cause) {
              return Result.err(toUnexpectedError(cause));
            }
          })(),
        ),
      transaction: <TResult>(
        work: (
          context: SqlOutboxTransactionContext<TTx, TTrellisApi, TJobs>,
        ) => Promise<TResult> | TResult,
      ) =>
        AsyncResult.from(
          (() => {
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
          })(),
        ),
    };
  }

  /**
   * Creates a short-lived receive transfer grant for the caller of an RPC.
   *
   * Pass the handler's `context`; the grant is bound to that verified caller's
   * session, connection, authority and the request's permission. Bytes come
   * from `store` and `key`, or from any `stream` whose `info` declares its size
   * and SHA-256 digest up front. The grant binds that metadata; bytes that end
   * differently fail the transfer.
   */
  createTransfer(
    args: {
      direction: "receive";
      context: RpcHandlerContext;
      expiresInMs?: number;
    } & DownloadSource,
  ): AsyncResult<ReceiveTransferGrant, TransferError> {
    const { caller, permission, requiredCapabilities, inboxPrefix } =
      args.context;
    if (caller.type !== "verified") {
      return AsyncResult.err(
        new TransferError({
          operation: "initiateDownload",
          cause: new Error("A receive transfer needs a verified caller"),
        }),
      );
    }
    return AsyncResult.from(
      this.#operationTransfer.initiateDownload({
        ...(args.stream
          ? { stream: args.stream, info: args.info }
          : { store: args.store, key: args.key }),
        sessionKey: caller.sessionPublicKey,
        connectionId: caller.runtimeId,
        contextDigest: caller.authorityDigest,
        permission,
        requiredCapabilities,
        inboxPrefix,
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
    return AsyncResult.from(
      (async () => {
        const completed = await this.#runtime.operations
          .complete(operationId, output)
          .take();
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
      })(),
    );
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
    AuthError | TransportError | UnexpectedError
  > {
    return AsyncResult.from(
      (async () => {
        const connected = await connectServiceSession(args);
        if (isErr(connected)) {
          return connected;
        }
        const service = connected.unwrapOrElse(() => {
          throw new Error("Connected service result narrowed incorrectly");
        });
        return Result.ok(createProviderRuntime(service, args.participant));
      })(),
    );
  }

  /**
   * Resolves the first time the service serves: connected, its build
   * installed, and every handler, job worker and consumer registered before
   * `wait()` taking work. It never resolves for a service that stops first.
   */
  get ready(): Promise<void> {
    return this.#lifecycle.ready;
  }

  /** Where the service is in its lifecycle now. */
  get phase(): ServicePhase {
    return this.#lifecycle.phase;
  }

  /**
   * Serves until the service stops, then resolves; rejects when its
   * connection ends for good, before serving too, or without serving when a
   * handler registered before it could not subscribe. It also rejects with
   * the error of a stop that fails. SIGTERM and SIGINT stop it gracefully
   * while it waits, as `stop()` does, and the process then exits once nothing
   * holds it open, or 5 seconds after the stop timeout at the latest, even if
   * the stop has not finished. A stop a signal began that fails is logged and
   * reported through this rejection, never left unhandled. A second signal
   * ends the process at once.
   */
  async wait(): Promise<void> {
    this.#waitPromise ??= (async () => {
      const unhandle = this.#handleStopSignals();
      const transport = this[SERVICE_TRANSPORT_LIFETIME];
      const closed = transport?.closed() ?? this.#nc.closed();
      try {
        const outbound = this[PROVIDER_CALLER] as ProviderIngressOwner;
        const started = (async () => {
          await Promise.allSettled([...this.#registrations]);
          // A handler that could not subscribe takes no work, so the service
          // never serves without it.
          if (this.#registrationFailure) throw this.#registrationFailure.error;
          // Serving waits until the broker has every durable consumer.
          await outbound.eventListenersAttached();
        })();
        // A connection that closes first ends the wait: it never serves.
        const closedFirst = await Promise.race([
          started.then(() => false),
          closed.then(() => true),
        ]);
        if (!closedFirst && !this.#stopPromise && !transport?.isClosed()) {
          // Jobs and durable deliveries flow from here on, with requests.
          await this.#managedJobWorkers.start().orThrow();
          outbound.serveEventDeliveries();
          // Serving follows the connection: lost, it reconnects and is not
          // ready meanwhile; recovered, it serves again.
          const follow = () => {
            if (this.#stopPromise) return;
            this.#lifecycle.set(
              this.connection.status.phase === "connected"
                ? "serving"
                : "connecting",
            );
          };
          this.connection.subscribe(follow);
          follow();
          // Every check and derive registered before wait() has run, so the
          // starting revision's outcome is final and Auth hears it now.
          this[SERVICE_CONFIG]?.started();
        }
        // Supervise the logical connection: an authoritative terminal failure of
        // the logical transport must stop the service. A single physical
        // generation being reaped is not a stop.
        const ended = await closed;
        if (ended instanceof Error) {
          throw ended;
        }
        if (!this.#stopPromise) {
          throw new Error("Trellis service connection closed unexpectedly");
        }
      } finally {
        await this.stop();
        unhandle();
      }
    })();

    await this.#waitPromise;
  }

  #handleStopSignals(): () => void {
    let received = false;
    const onSignal = (signal: NodeJS.Signals) => {
      if (received) {
        // Asked twice: end now, with the status the signal itself gives.
        process.exit(signal === "SIGINT" ? 130 : 143);
      }
      received = true;
      this.#log.info(
        { service: this.name, signal },
        "Stopping: finishing the work in flight",
      );
      // A signal asks the process to end. Neither a handler still running
      // past the deadline nor a stop that never finishes may keep it alive:
      // 5 seconds after the stop timeout the process exits, which leaves the
      // code after wait() a few seconds. The timer never delays a process that
      // ends on its own.
      setTimeout(() => process.exit(), this.#stopTimeoutMs + 5_000).unref();
      // wait() rejects with a failed stop, so the application handles it
      // there; this promise is logged, never left unhandled to kill the
      // process outside that path.
      this.stop().catch((error: unknown) => {
        this.#log.error(
          { service: this.name, signal, error },
          "Stopping after the signal failed",
        );
      });
    };
    const unhandle = () => {
      process.off("SIGTERM", onSignal);
      process.off("SIGINT", onSignal);
    };
    process.on("SIGTERM", onSignal);
    process.on("SIGINT", onSignal);
    return unhandle;
  }

  /**
   * Stops the service gracefully. It takes no new requests, jobs or event
   * deliveries, so other instances get them, and ends its Live streams so
   * their callers can open them again elsewhere. It then waits up to
   * `stopTimeoutMs` for the requests, deliveries and jobs it holds; whatever
   * is still running then is handed back at once for another instance, and
   * the connection closes.
   */
  async stop(): Promise<void> {
    this.#stopPromise ??= (async () => {
      this.#intakeFenced = true;
      // Intake still being installed takes nothing from here on either.
      for (const id of this.#installing.values()) this.#retireIntake(id);
      this.#lifecycle.set("stopping");
      this[SERVICE_CONFIG]?.stop();
      const transport = this[SERVICE_TRANSPORT_LIFETIME];
      const outbound = this[PROVIDER_CALLER] as ProviderIngressOwner;
      const runtime = this.#runtime;
      if (transport && !transport.isClosed()) {
        let timer: ReturnType<typeof setTimeout> | undefined;
        const deadline = new Promise<void>((resolve) => {
          timer = setTimeout(resolve, this.#stopTimeoutMs);
        });
        const settled = Promise.allSettled([
          transport.retireAllIntake(),
          ...this.#installing.keys(),
          this.#managedJobWorkers.drain(),
          outbound.drainEventListeners(),
          outbound.endLiveSessions(),
          runtime.endLiveSessions(),
        ]);
        await Promise.race([settled, deadline]);
        clearTimeout(timer);
        // Past the deadline, what still runs goes back for another instance.
        outbound.abandonEventDeliveries();
      }
      this.connection.stopObserving();
      this.#handlerTrellis.stopEventListeners();
      for (const dispatcher of this.#ownedOutboxDispatchers) {
        dispatcher.stop();
      }
      this.#ownedOutboxDispatchers.clear();

      // Name a shutdown phase that stalls, so a stuck stop is diagnosable.
      let phase = "health publishing";
      let phaseStarted = Date.now();
      let warned = "";
      const watchdog = setInterval(() => {
        if (warned === phase || Date.now() - phaseStarted < 10_000) return;
        warned = phase;
        this.#log.warn(
          { service: this.name, phase, elapsedMs: Date.now() - phaseStarted },
          "Service stop is still waiting on a shutdown phase",
        );
      }, 1_000);
      const enter = (next: string) => {
        phase = next;
        phaseStarted = Date.now();
      };
      try {
        await this.#stopHealthPublishing();
      } finally {
        try {
          enter("job workers");
          await this.#managedJobWorkers.stop().orThrow();
        } finally {
          try {
            enter("operation transfers");
            await this.#operationTransfer.stop();
          } finally {
            try {
              const retire = this[SERVICE_RETIRE_OWN];
              if (retire && transport && !transport.isClosed()) {
                enter("instance retirement");
                try {
                  await retire();
                } catch (error) {
                  // Timed retirement removes the instance later instead.
                  this.#log.warn(
                    { service: this.name, error },
                    "Could not retire this instance at stop",
                  );
                }
              }
              // Nothing is served any more, so the clients and pools built
              // from settings can go.
              enter("settings");
              await this[SERVICE_CONFIG]?.retire();
              enter("runtime");
              await this.#runtime.stop();
            } finally {
              try {
                enter("connection close");
                await this.connection.close();
              } finally {
                clearInterval(watchdog);
                this.#lifecycle.set("stopped");
              }
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
          args: OperationHandlerContext<
            InferSchemaType<TOwnedApi["operations"][O]["input"]>,
            OperationProgressOf<TOwnedApi, O>,
            OperationOutputOf<TOwnedApi, O>,
            OperationTransferContextOf<TOwnedApi, O>,
            BaseError
          > & { client: Trellis<TTrellisApi, TKv, TJobs> },
        ) => unknown | Promise<unknown>,
      ) =>
        registration.handle((context) =>
          handler({
            ...context,
            client: this.#handlerTrellis,
          }),
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
    AuthError | TransportError | UnexpectedError
  > {
    return TrellisServiceSession.connect(args);
  }
}
