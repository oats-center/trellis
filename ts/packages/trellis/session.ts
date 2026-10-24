import { DurableEventConsumers } from "./session/durable_events.ts";
import {
  sessionRequestSigningDigest,
  sessionEventSigningDigest,
  sessionResponseSigningDigest,
} from "./auth/message_digests.ts";
import { jetstream, type JetStreamClient } from "@nats-io/jetstream";
import {
  createInbox,
  headers as natsHeaders,
  type Msg,
  type MsgHdrs,
  type NatsConnection,
} from "@nats-io/nats-core";
import type { LiveDesc } from "./participant.ts";
import {
  type PermissionAtom as DescriptorPermissionAtom,
  routeQueueGroup,
  type RuntimeApi,
} from "./participant_runtime/api.ts";

import { encodeEventSubjectParameterToken } from "./helpers.ts";
import { openLive, openLiveOperationWatch } from "./live/client_open.ts";
import { LiveProvider, parseLiveOpen } from "./live/provider.ts";
import type {
  OriginalEventPublication,
  SessionEvent,
  SessionRequest,
  SessionResponse,
} from "./auth/protocol_wasm.ts";
import { LiveAuthorityGuard } from "./live/authority.ts";
import type { LiveSubscription } from "./live/subscription.ts";
import { LiveStreamError } from "./live/types.ts";

import {} from "./auth/protocol_wasm.ts";

import type {
  PendingSessionRequest,
  VerifiedAuthority,
} from "./auth/authority.ts";
import { operationRecordRepresentation } from "./service/runtime/operation_record.ts";
import {
  fixedTransportProvider,
  type TransportLease,
  type TrellisTransportProvider,
} from "./transport/generations.ts";
import { PromotedOwnAuthorityView } from "./own_authority.ts";
import {
  AsyncResult,
  BaseError,
  err,
  isErr,
  type MaybeAsync,
  ok,
  Result,
} from "@oatscenter/result";
import {
  createNatsHeaderCarrier,
  extractTraceContext,
  injectTraceContext,
  recordCatalogDuration,
  recordCatalogUpDown,
  recordRpcAttempt,
  SpanStatusCode,
  startClientSpan,
  startServerSpan,
  trellisRoute,
  withSpanAsync,
} from "./telemetry/mod.ts";
import { Pointer } from "typebox/value";
import { ulid } from "ulid";
import { encodeSchema, type JsonValue, parse } from "./codec.ts";
import {
  AuthError,
  type SchemaValidationError,
  TransferError,
  TransportError,
  UnexpectedError,
  ValidationError,
} from "./errors/index.ts";
import { RemoteError } from "./errors/RemoteError.ts";
import { logger, type LoggerLike } from "./globals.ts";
import { type ResourceMigrations, TypedKV } from "./kv.ts";
import { TrellisErrorDataSchema } from "./errors/RemoteError.ts";

import {
  OperationInvoker,
  type OperationTransport,
  operationStartSubject,
} from "./operations.ts";
import type { Span } from "./telemetry/mod.ts";
import {
  type DeleteOutput as StateDeleteResponse,
  type GetOutput as StateGetResponse,
  type PutOutput as StateSetResponse,
  type RegisterOutput as StateRegisterResponse,
} from "./internal_sdk/generated/apis/state/mod.js";

import { permission as actionPermission } from "./participant_runtime/participant.ts";
import {
  createTransferHandle,
  type FileInfo,
  type ReceiveTransferGrant,
  type ReceiveTransferHandle,
  type SendTransferGrant,
  type SendTransferHandle,
  type TransferBody,
  type TransferGrant,
  type TransferOperationError,
} from "./transfer.ts";
import { TrellisTasks } from "./tasks.ts";
import { TrellisConnection } from "./connection.ts";
import { API as JOBS_HISTORY_API } from "./internal_sdk/generated/apis/jobs/mod.js";
import type {
  JobsHistoryRequest,
  JobsHistoryResponse,
} from "./internal_sdk/generated/types/index.js";
import {
  type EventOpts,
  type GenerationIntakeInstall,
  type LiveHandlerContext,
  type LiveInputBuilder,
  type LiveRegistration,
  type LiveSubscribeOpts,
  type ProviderGenerationAttachment,
  type ProviderIngress,
  type RequestOpts,
  type TrellisAuth,
  type TrellisMode,
  type TrellisOpts,
} from "./session/types.ts";
import {
  type EventDescriptorOf,
  type EventOf,
  type EventsOf,
  type LiveDescriptorOf,
  type LiveEventOf,
  type LiveInputOf,
  type LivesOf,
  type MethodsOf,
  type OperationsOf,
  type PreparedTrellisEvent,
  type RequestInputOf,
  type RequestMethodOf,
  type RequestOutputOf,
  type RpcDescriptorOf,
  type RpcRequestShapes,
  type RuntimeStateStores,
} from "./session/contract_types.ts";
import {
  createEventListenerContext,
  internalEphemeralEventNeeds,
  internalEventConsumers,
  type InternalizedTrellisOpts,
  type TrellisInternalOpts,
} from "./session/events.ts";
import {
  STATE_RUNTIME_RPC,
  StateHandle,
  type StateFacade,
  type ValueStateStoreClient,
} from "./session/state.ts";
import {
  addSurfaceLeaf,
  type ActiveEventFacade,
  type ActiveEventPublishFacade,
  type ActiveLiveFacade,
  type ActiveOperationFacade,
  type ActiveRpcFacade,
  type ActiveRpcHandleFacade,
  type EventCallback,
  type HandlerFn,
  type HandlerTrellis,
  type MaybePromise,
  type ProcessedRpcResponse,
  type RpcHandlerContext,
  type RuntimeEventLeaf,
  type RuntimeEventPublishLeaf,
  type RuntimeLiveLeaf,
  type RuntimeOperationLeaf,
  type RuntimeRpcLeaf,
  type SurfaceGroups,
} from "./session/facades.ts";
import {
  type DurableOperationRecord,
  type OperationSurface,
  type RuntimeOperationDesc,
  type RuntimeOperationRecord,
} from "./session/operations.ts";
import {
  EMPTY_TRELLIS_API,
  activeTraceId,
  causeLogData,
  causeMessage,
  ingressRetiredError,
  isBrowserAuthRequiredError,
  isRuntimeRpcErrorDesc,
  payloadByteLength,
  traceIdFromTraceparent,
} from "./session/helpers.ts";
import {
  toCatalogAction,
  verifyLocalAuthorization,
  type RuntimeRpcErrorDesc,
  type SessionCaller,
  type VerifiedCaller,
} from "./session/local_authorization.ts";
import {
  MAX_PENDING_OPENINGS,
  base64urlEncode,
  callerOwnsReply,
  classifyRequestTransportFailure,
  createTransportError,
  encodeRuntimeSchema,
  mapTransportAcquireFailure,
  outcomeForFailure,
  parseRuntimeSchema,
  requestFailedTransportError,
  safeJson,
} from "./session/wire.ts";
import {
  annotateHandlerBoundaryError,
  isResultLike,
  reconstructDeclaredRpcError,
  recordRuntimeError,
} from "./session/handler_errors.ts";
export {
  verifyLocalAuthorization,
  toCatalogAction,
} from "./session/local_authorization.ts";
export type {
  VerifiedCaller,
  InternalCaller,
  SessionCaller,
} from "./session/local_authorization.ts";
export {
  safeJson,
  classifyRequestTransportFailure,
  createActionUnavailableError,
  base64urlEncode,
  base64urlDecode,
  toArrayBuffer,
  sha256,
} from "./session/wire.ts";
export type {
  GenerationIntakeTarget,
  GenerationIntakeInstallResult,
  GenerationIntakeInstall,
  ProviderGenerationAttachment,
  TrellisSigner,
  TrellisAuth,
  TrellisMode,
  TrellisOpts,
  RequestOpts,
  EventOpts,
  LiveSubscribeOpts,
  LiveInputBuilder,
  LiveHandlerContext,
  LiveRegistration,
} from "./session/types.ts";
export type {
  RuntimeStateStoreShape,
  RuntimeStateStores,
  RuntimeStateStoresForContract,
  MethodsOf,
  RpcMethodNameOf,
  OperationsOf,
  LivesOf,
  RpcInputOf,
  RpcOutputOf,
  OperationHandlerErrorOf,
  TrellisEventHeader,
  TrellisEventMessage,
  PreparedTrellisEvent,
  LiveInputOf,
  LiveEventOf,
  OperationInputOf,
  OperationProgressOf,
  OperationUpdateOf,
  OperationOutputOf,
  RpcRequestErrorOf,
  RpcHandlerErrorOf,
  EventName,
  EventType,
  EventPayload,
} from "./session/contract_types.ts";
export {
  isOperationDeferred,
  buildRuntimeOperationSnapshot,
  isTerminalRuntimeOperationSnapshot,
} from "./session/operations.ts";
export type {
  OperationRuntimeHandle,
  OperationDeferred,
  AcceptedOperation,
  OperationTransferHandle,
  OperationHandlerContext,
  OperationRegistration,
  OperationTransferContextOf,
  OperationSurface,
  RuntimeOperationDesc,
  RuntimeOperationSignal,
  RuntimeOperationSignalWaiter,
  RuntimeOperationTransferProgress,
  RuntimeOperationState,
  RuntimeOperationSnapshot,
  RuntimeOperationRecord,
  DurableOperationRecord,
  RuntimeOperationAcceptedEnvelope,
  RuntimeOperationControlRequest,
  RuntimeOperationController,
} from "./session/operations.ts";
export {
  StateConflictError,
  ResourceUnavailableError,
  StateHandle,
} from "./session/state.ts";
export type {
  StateValue,
  ValueStateStoreClient,
  StateFacade,
  StateResourceCalls,
  StateRegistration,
  StateWatch,
} from "./session/state.ts";
export {
  isResultLike,
  annotateHandlerBoundaryError,
} from "./session/handler_errors.ts";
export type { HandlerErrorAnnotationContext } from "./session/handler_errors.ts";
export type { EventListenerContext } from "./session/events.ts";
export type {
  ActiveRpcFacade,
  ActiveEventFacade,
  ActiveEventPublishFacade,
  ActiveLiveFacade,
  ActiveOperationFacade,
  ActiveRpcHandleFacade,
  LiveSurface,
  RpcHandlerContext,
  HandlerTrellis,
  HandlerKvFacade,
  HandlerStoreHandle,
  HandlerJobQueue,
  HandlerJobsFacade,
  HandlerTrellisForContract,
  HandlerFn,
} from "./session/facades.ts";

export type { NatsConnection } from "@nats-io/nats-core";

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
  const { eventConsumers, ephemeralEventNeeds, inboxPrefix, ...publicOpts } =
    opts ?? {};
  const internalOpts: InternalizedTrellisOpts<TA> = {
    ...publicOpts,
    [internalEventConsumers]: eventConsumers,
    [internalEphemeralEventNeeds]: ephemeralEventNeeds,
  };
  return new Trellis<TA, TMode, TState>(
    name,
    nats,
    auth,
    internalOpts,
    inboxPrefix,
  );
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
  readonly live: ActiveLiveFacade<TA>;
  readonly handle: { readonly rpc: ActiveRpcHandleFacade<TA, TRequests> };
  /** Framework-neutral lifecycle handle for this Trellis runtime connection. */
  readonly connection: TrellisConnection;
  readonly contractId?: string;
  readonly contractDigest?: string;

  #transport: TrellisTransportProvider;
  #operationFacade?: ActiveOperationFacade<TA>;
  #adaptiveTransport: boolean;
  #auth: TrellisAuth;
  #inboxPrefix: string;
  readonly api: TA;
  #replyRequests = new WeakMap<
    Msg,
    { request: SessionRequest; digest: string }
  >();
  #log: LoggerLike;
  readonly #durableEvents: DurableEventConsumers<TA>;
  #tasks: TrellisTasks;
  #rpcTaskSeq = 0;
  #liveTaskSeq = 0;
  #hasExplicitApi: boolean;
  #onSessionNotFound?: () => MaybePromise<void>;
  #operationStore?: Promise<TypedKV<DurableOperationRecord>>;
  #operationDeploymentId?: string;
  #ephemeralEventNeeds?: ReadonlySet<string>;
  #liveClosers = new Set<(terminal: void | Error) => void>();
  #resourceGeneration: () => number;
  #resourceAvailability: (name: string) => boolean;
  #stateMigrations: Readonly<
    Record<string, ResourceMigrations<unknown> | undefined>
  >;
  /**
   * Logical provider core: registered RPC handlers survive every transport
   * generation. Ingress subscriptions are installed per generation.
   */
  #rpcRegistrations = new Map<
    string,
    {
      method: MethodsOf<TA>;
      ctx: RpcDescriptorOf<TA, MethodsOf<TA>>;
      subject: string;
      fn: HandlerFn<TA, MethodsOf<TA>, TA, HandlerTrellis<TA, TRequests>>;
      handlerTrellis: HandlerTrellis<TA, TRequests>;
    }
  >();
  /**
   * Logical provider core: registered live handlers survive every transport
   * generation. Each holds a per-generation installer that builds the admitted
   * provider from that generation's exact context.
   */
  #liveRegistrations = new Map<
    string,
    {
      install: (target: {
        id: number;
        nc: NatsConnection;
        contextDigest: string;
        retainAuthority?: () => VerifiedAuthority;
        lease?: () => TransportLease;
      }) => Promise<{
        drain: () => Promise<void>;
        done: Promise<void>;
        /**
         * Reinstall this generation's generic live-open intake on the same
         * provider (its control subscription, retained authority, and accepted
         * sessions stay intact). Used when a drained survivor is reactivated.
         */
        reopen: () =>
          | { drain: () => Promise<void>; done: Promise<void> }
          | Promise<{ drain: () => Promise<void>; done: Promise<void> }>;
        /**
         * Retire this generation's provider: stop session-serving control, fence
         * and terminate owned sessions, and release its retained authority. Run
         * only when the generation physically closes or a failed install is
         * rolled back, never at a planned generic-intake cutover.
         */
        dispose: () => Promise<void>;
      }>;
    }
  >();
  /**
   * Internal framework surfaces (operations) that install their own generic
   * intake per admitted generation through the shared provider-ingress
   * mechanism instead of subscribing on a fixed connection.
   */
  #generationIntakes = new Map<string, GenerationIntakeInstall>();
  /**
   * One installed provider ingress per admitted generation, keyed by the
   * generation id the owning runtime reports.
   */
  #providerIngress = new Map<number, ProviderIngress>();

  /** Current owner-backed, catalog-verified advisory identity and action projection. */
  readonly ownAuthority: PromotedOwnAuthorityView | undefined;

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
    this.#adaptiveTransport = opts?.transport !== undefined;
    this.#transport = opts?.transport ?? fixedTransportProvider(nats);
    const liveClosers = this.#liveClosers;
    void this.#transport.closed().then((terminal) => {
      for (const close of liveClosers) close(terminal);
      liveClosers.clear();
    });
    this.#auth = auth as TrellisAuth;
    this.#inboxPrefix = inboxPrefix;
    this.api = (api ?? EMPTY_TRELLIS_API) as TA;
    this.#log = (opts?.log ?? logger).child({ lib: "trellis" });
    this.timeout = opts?.timeout ?? 3000;
    this.stream = opts?.stream ?? "trellis";
    this.contractId = opts?.contractId;
    this.contractDigest = opts?.contractDigest;
    this.#hasExplicitApi = api !== undefined;
    this.#onSessionNotFound = opts?.onSessionNotFound;
    this.#resourceGeneration = opts?.resourceGeneration ?? (() => 0);
    this.#resourceAvailability = opts?.resourceAvailability ?? (() => true);
    this.#stateMigrations = opts?.stateMigrations ?? {};
    const trellis = this;
    this.#durableEvents = new DurableEventConsumers<TA>(
      {
        get timeout() {
          return trellis.timeout;
        },
        get tasks() {
          return trellis.#tasks;
        },
        get log() {
          return trellis.#log;
        },
        get adaptiveTransport() {
          return trellis.#adaptiveTransport;
        },
        transport: () => trellis.#transport,
        nats: () => trellis.#nats,
        js: () => trellis.#js,
        requestBuiltRpc: (method, input, ctx, opts) =>
          trellis.#requestBuiltRpc(method, input, ctx, opts),
        invokeEventHandler: (args) => trellis.#invokeEventHandler(args),
        parseEventMessage: (event, ctx, msg) =>
          trellis.#parseEventMessage(event, ctx, msg),
        validateEventProof: (event, descriptor, msg, original) =>
          trellis.#validateEventProof(event, descriptor, msg, original),
      },
      internalOpts?.[internalEventConsumers] ?? {},
    );
    this.#ephemeralEventNeeds = internalOpts?.[internalEphemeralEventNeeds];
    this.connection =
      opts?.connection ?? new TrellisConnection({ kind: "client" });
    if (auth.authorityOwner) {
      const view = new PromotedOwnAuthorityView(auth.authorityOwner, this.api);
      this.ownAuthority = view;
      void view.refresh().catch(() => undefined);
      void this.#transport.closed().then(() => view.close());
    }

    this.#tasks = new TrellisTasks({ log: this.#log });
    this.state = this.#createStateFacade(opts?.state as TState | undefined);
    this.rpc = this.#createRpcFacade();
    this.handle = { rpc: this.#createRpcHandleFacade() };
    this.event = this.#createEventFacade();
    this.live = this.#createLiveFacade();
  }

  get #nats(): NatsConnection {
    return this.#transport.currentNats();
  }

  get #js(): JetStreamClient {
    return jetstream(this.#nats);
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
    const stateRpc = (name: "Register" | "Get" | "Put" | "Delete") => {
      const descriptor = Object.values(this.api.rpc).find(
        (candidate) =>
          candidate.permission.apiId === "trellis.state" &&
          candidate.permission.apiVersion === "v1" &&
          candidate.permission.surfaceName === name,
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
            registration: descriptor.registration,
            isCurrent: () => generation === this.#resourceGeneration(),
            isAvailable: () => this.#resourceAvailability(resourceName),
            calls: {
              watch: (input, opts) => {
                const live = Object.values(this.api.lives ?? {}).find(
                  (candidate) =>
                    candidate.permission.apiId === "trellis.state" &&
                    candidate.permission.apiVersion === "v1" &&
                    candidate.permission.surfaceName === "Watch",
                );
                if (!live)
                  throw new Error(
                    "Generated participant is missing State.Watch",
                  );
                return this.#subscribeLive<typeof input, StateGetResponse>(
                  "State.Watch",
                  live,
                  input,
                  opts,
                );
              },
              register: (input) =>
                this.#requestBuiltRpc<StateRegisterResponse>(
                  "State.Register",
                  input,
                  {
                    ...STATE_RUNTIME_RPC.Register,
                    subject: stateRpc("Register").subject,
                    permission: stateRpc("Register").permission,
                  },
                ),
              get: (input) =>
                this.#requestBuiltRpc<StateGetResponse>("State.Get", input, {
                  ...STATE_RUNTIME_RPC.Get,
                  subject: stateRpc("Get").subject,
                  permission: stateRpc("Get").permission,
                }),
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
                    permission: stateRpc("Put").permission,
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
                    permission: stateRpc("Delete").permission,
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
      addSurfaceLeaf(surface, method, (handler) =>
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

  #createLiveFacade(): ActiveLiveFacade<TA> {
    const surface: SurfaceGroups<RuntimeLiveLeaf> = {};
    for (const live of Object.keys(this.api.lives ?? {})) {
      const leaf: RuntimeLiveLeaf = (input, opts) =>
        this.liveHandle(live as LivesOf<TA>)
          .input(input as LiveInputOf<TA, LivesOf<TA>>)
          .subscribe(opts) as AsyncResult<LiveSubscription<unknown>, BaseError>;
      addSurfaceLeaf(surface, live, leaf);
    }
    return surface as ActiveLiveFacade<TA>;
  }

  #createHandlerTrellis(): HandlerTrellis<TA, TRequests> {
    return {
      rpc: this.rpc,
      event: this.#createEventPublishFacade(),
      live: this.live,
      operation: this.operation,
      request: this.request.bind(this),
      prepare: (event, data) => this.prepare(event, data),
      publish: (event, data) => this.publish(event, data),
      publishPrepared: (event) => this.publishPrepared(event),
      stopEventListeners: () => this.stopEventListeners(),
    };
  }

  /** Operation methods initialized after derived runtime construction. */
  get operation(): ActiveOperationFacade<TA> {
    return (this.#operationFacade ??= this.#createOperationFacade());
  }

  #createOperationFacade(): ActiveOperationFacade<TA> {
    const surface: SurfaceGroups<RuntimeOperationLeaf> = {};
    for (const operation of Object.keys(this.api.operations ?? {})) {
      addSurfaceLeaf(
        surface,
        operation,
        this.operationHandle(
          operation,
        ) as OperationInvoker<RuntimeOperationDesc>,
      );
    }
    return surface as ActiveOperationFacade<TA>;
  }

  #unknownApiError(
    kind: "RPC method" | "operation" | "event" | "live",
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

  async operationStoreHandle(): Promise<TypedKV<DurableOperationRecord>> {
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
          this.#transport,
          bucket,
          operationRecordRepresentation,
          {
            // The platform producer owns framework retention and size limits.
            bindOnly: true,
            acquireTimeoutMs: this.timeout,
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

  /** Persists a fenced operation; fresh admission must never replace an invocation. */
  async saveOperationRecord(
    runtime: RuntimeOperationRecord,
    createOnly = false,
  ): Promise<void> {
    const store = await this.operationStoreHandle();
    const loaded = await store.getEntry(runtime.id);
    const loadedValue = loaded.take();
    if (isErr(loadedValue)) throw loadedValue.error;
    const existing = loadedValue?.value;
    if (
      (createOnly && loadedValue !== undefined) ||
      (existing && existing.revision !== runtime.revision)
    ) {
      throw new Error("operation revision conflict");
    }
    const revision = existing ? runtime.revision + 1 : runtime.revision;
    const record: DurableOperationRecord = {
      invocationId: runtime.id,
      deploymentId: runtime.deploymentId,
      callerSessionKey: runtime.callerSessionKey,
      invocationDigest: runtime.invocationDigest,
      caller: runtime.caller,
      creatorPrincipalId: runtime.creatorPrincipalId,
      creatorBinding: runtime.creatorBinding,
      apiId: runtime.apiId,
      operation: runtime.operation,
      input: runtime.input,
      ...(runtime.telemetry ? { telemetry: runtime.telemetry } : {}),
      revision,
      ownerInstanceId: runtime.ownerInstanceId,
      ownerConnectionId: runtime.ownerConnectionId,
      ownerEpoch: runtime.ownerEpoch,
      leaseExpiresAt: runtime.leaseExpiresAt,
      ...(existing?.cancelRequestedAt || runtime.cancelRequestedAt
        ? {
            cancelRequestedAt:
              existing?.cancelRequestedAt ?? runtime.cancelRequestedAt,
          }
        : {}),
      ...(runtime.transferGrant
        ? { transferGrant: runtime.transferGrant }
        : {}),
      ...(runtime.uploadState ? { uploadState: runtime.uploadState } : {}),
      sequence: runtime.sequence,
      signalSequence: runtime.signalSequence,
      signals: runtime.signals,
      snapshot: { ...runtime.snapshot, revision },
    };
    const saved =
      createOnly || loadedValue === undefined
        ? await store.create(runtime.id, record)
        : await store.replace(runtime.id, loadedValue.revision, record);
    const value = saved.take();
    if (isErr(value)) throw value.error;
    runtime.cancelRequestedAt = record.cancelRequestedAt;
    runtime.revision = revision;
    runtime.snapshot.revision = revision;
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
    const ctx = rpcApi[method] as
      | {
          subject: string;
          permission: DescriptorPermissionAtom;
          input: unknown;
          output: unknown;
          callerCapabilities: readonly string[];
          errors?: readonly string[];
          declaredErrorTypes?: readonly string[];
          runtimeErrors?: readonly RuntimeRpcErrorDesc[];
        }
      | undefined;
    if (!ctx) {
      return AsyncResult.from(
        Promise.resolve(
          err(
            new UnexpectedError({
              cause: this.#unknownApiError("RPC method", method.toString()),
              context: { method: method.toString() },
            }),
          ),
        ),
      );
    }

    return this.#requestBuiltRpcUnknown(method, input, ctx, opts);
  }

  #requestBuiltRpcUnknown(
    method: string,
    input: unknown,
    ctx: {
      subject: string;
      permission: DescriptorPermissionAtom;
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
      permission: DescriptorPermissionAtom;
      input: unknown;
      output: unknown;
      callerCapabilities: readonly string[];
      errors?: readonly string[];
      declaredErrorTypes?: readonly string[];
      runtimeErrors?: readonly RuntimeRpcErrorDesc[];
    },
    opts?: RequestOpts,
  ): AsyncResult<TOutput, BaseError> {
    return AsyncResult.from(
      (async () => {
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
        const route = trellisRoute("rpc", method);
        const span = startClientSpan(route);
        const attempt = async (): Promise<Result<TOutput, BaseError>> => {
          const msgResult = await this.#requestMessage({
            method,
            route,
            subject,
            payload: msg,
            timeout: opts?.timeout ?? this.timeout,
            signal: opts?.signal,
            callerCapabilities: ctx.callerCapabilities,
            permission: ctx.permission,
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
                hint: "Retry the request. If it keeps happening, check the Trellis capability handling this request.",
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
                hint: "Retry the request. If it keeps happening, check the Trellis capability handling this request.",
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
              ? ctx.declaredErrorTypes.filter(
                  (value): value is string => typeof value === "string",
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
              hint: "Retry the request. If it keeps happening, check the Trellis capability handling this request.",
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
          // One logical RPC duration including retries, serialization, and final
          // decode. The attempt counter remains the transport-attempt boundary.
          const logicalStartedAt = performance.now();
          const recordLogicalOutcome = (outcome: string): void => {
            recordCatalogDuration(
              "trellis.rpc.client.duration",
              performance.now() - logicalStartedAt,
              { "trellis.route": route, "trellis.outcome": outcome },
            );
          };
          try {
            const result = await attempt();
            const value = result.take();
            const outcome = isErr(value)
              ? outcomeForFailure(value.error)
              : "ok";
            if (isErr(value)) {
              span.setStatus({
                code: SpanStatusCode.ERROR,
                message: outcome,
              });
            } else {
              span.setStatus({ code: SpanStatusCode.OK });
            }
            recordLogicalOutcome(outcome);
            return result;
          } catch (cause) {
            const unexpected =
              cause instanceof TransportError
                ? cause
                : new UnexpectedError({ cause });
            span.setStatus({
              code: SpanStatusCode.ERROR,
              message: "error",
            });
            recordLogicalOutcome("error");
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
      })(),
    );
  }

  async #handleBrowserAuthRequired(error: unknown): Promise<void> {
    if (!this.#onSessionNotFound || !isBrowserAuthRequiredError(error)) {
      return;
    }

    await this.#onSessionNotFound();
  }

  async #authenticateLiveRequest(args: {
    msg: Msg;
    permission: DescriptorPermissionAtom | undefined;
    requiredCapabilities: readonly string[];
  }): Promise<Result<SessionCaller, BaseError>> {
    return await verifyLocalAuthorization({
      kind: "request",
      cache: this.#auth.authorityOwner,
      message: args.msg,
      permission: args.permission,
      requiredCapabilities: args.requiredCapabilities,
    });
  }

  liveHandle<F extends LivesOf<TA>>(
    live: F,
  ): LiveInputBuilder<LiveInputOf<TA, F>, LiveEventOf<TA, F>> &
    LiveRegistration<LiveInputOf<TA, F>, LiveEventOf<TA, F>> {
    const descriptor = this.api.lives?.[live] as
      LiveDescriptorOf<TA, F> | undefined;
    if (!descriptor) {
      throw this.#unknownApiError("live", live.toString());
    }

    return {
      input: (input: LiveInputOf<TA, F>) => ({
        subscribe: (opts?: LiveSubscribeOpts) =>
          this.#subscribeLive(
            live.toString(),
            descriptor,
            input,
            opts,
          ) as AsyncResult<LiveSubscription<LiveEventOf<TA, F>>, BaseError>,
      }),
      handle: (
        handler: (
          context: LiveHandlerContext<LiveInputOf<TA, F>, LiveEventOf<TA, F>>,
        ) => unknown | Promise<unknown>,
      ) => this.#handleLive(live.toString(), descriptor, handler),
    };
  }

  #subscribeLive<TInput, TEvent>(
    live: string,
    descriptor: LiveDesc,
    input: TInput,
    opts?: LiveSubscribeOpts,
  ): AsyncResult<LiveSubscription<TEvent>, BaseError> {
    let transport: { nc: NatsConnection; release(): void } | undefined;
    let subscription: LiveSubscription<TEvent> | undefined;
    const closeOnNats = () => {
      subscription?.close();
      this.#liveClosers.delete(closeOnNats);
    };
    return AsyncResult.from(
      (async (): Promise<Result<LiveSubscription<TEvent>, BaseError>> => {
        const payload = encodeRuntimeSchema(descriptor.input, input).take();
        if (isErr(payload)) {
          recordRuntimeError(payload.error, {
            surface: "live",
            direction: "client",
            operation: live,
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
            surface: "live",
            direction: "client",
            operation: live,
            phase: "request_template",
          });
          return subject;
        }
        if (opts?.signal?.aborted) {
          const error = createTransportError({
            code: "trellis.live.subscribe_aborted",
            message:
              "The live subscription was aborted before Trellis acknowledged it.",
            hint: "Retry the subscription if the live observation is still needed.",
            context: { live, subject },
          });
          return err(error);
        }
        const abort = () => {
          const current = subscription;
          current?.fence();
          if (current)
            void current
              .close()
              .take()
              .catch(() => undefined);
        };
        try {
          const cache = this.#auth.authorityOwner;
          if (!cache) {
            throw new LiveStreamError(
              "authorization_unavailable",
              "provider authorization cache is required for live observations",
            );
          }
          // Cancellation is live before the first awaited open allocation.
          // Fencing is synchronous and local; the bounded close exchange that
          // follows is what tells the provider to abort its own handler, so a
          // consumer abort must not leave the provider waiting out peer
          // inactivity.
          opts?.signal?.addEventListener("abort", abort, { once: true });
          if (opts?.signal?.aborted) {
            throw new LiveStreamError("cancelled", "live open was aborted");
          }
          const acquired = (
            await this.#acquireRequestTransport({
              method: live,
              subject,
              deadlineMs: Date.now() + this.timeout,
              signal: opts?.signal,
            })
          ).take();
          if (isErr(acquired)) {
            opts?.signal?.removeEventListener("abort", abort);
            recordRuntimeError(acquired.error, {
              surface: "live",
              direction: "client",
              operation: live,
              phase: "handshake",
            });
            return acquired;
          }
          transport = acquired;
          if (opts?.signal?.aborted) {
            throw new LiveStreamError("cancelled", "live open was aborted");
          }
          subscription = await openLive(
            {
              nats: transport.nc,
              inboxPrefix: this.#inboxPrefix,
              timeoutMs: this.timeout,
              sessionKey: this.#auth.sessionKey,
              live: this.connection.live,
              createRequestProof: (s, p, r) =>
                this.createRequestProof(s, p, r, descriptor.permission),
              authority: {
                cache,
                apiId: `${descriptor.permission.apiId}@${descriptor.permission.apiVersion}`,
                action: toCatalogAction(descriptor.permission),
                localContextDigest: this.#contextDigest(),
              },
              decodeEvent: (value) => {
                const parsed = parseRuntimeSchema(
                  descriptor.event,
                  value as JsonValue,
                ).take();
                if (isErr(parsed)) throw parsed.error;
                return parsed as TEvent;
              },
            },
            subject,
            payload,
          );
          // The abort listener was installed before the open; a signal that
          // fired during it is observed here. The bounded close exchange tells
          // the provider to abort its handler instead of waiting out peer
          // inactivity for a session the caller never received.
          if (opts?.signal?.aborted) {
            subscription.fence();
            void subscription
              .close()
              .take()
              .catch(() => undefined);
            throw new LiveStreamError("cancelled", "live open was aborted");
          }
          this.#liveClosers.add(closeOnNats);
          // The endpoint's own telemetry owner records the Live metrics
          // projection from the same local state; no second accounting here.
          void subscription.closed.then(() => {
            transport?.release();
            opts?.signal?.removeEventListener("abort", abort);
            this.#liveClosers.delete(closeOnNats);
          });
          return ok(subscription!);
        } catch (cause) {
          opts?.signal?.removeEventListener("abort", abort);
          if (subscription) {
            void subscription.closed.then(() => transport?.release());
          } else {
            transport?.release();
          }
          const error =
            cause instanceof TransportError
              ? cause
              : cause instanceof LiveStreamError
                ? createTransportError({
                    code: `trellis.live.${cause.code}`,
                    message: cause.message,
                    hint: "Retry the subscription. If it keeps failing, check Trellis runtime health.",
                    cause,
                    context: { live, subject },
                  })
                : createTransportError({
                    code: "trellis.live.subscribe_failed",
                    message:
                      "Trellis could not subscribe to the live observation.",
                    hint: "Retry the subscription. If it keeps failing, check Trellis runtime health.",
                    cause,
                    context: { live, subject },
                  });
          recordRuntimeError(error, {
            surface: "live",
            direction: "client",
            operation: live,
            phase: "handshake",
          });
          return err(error);
        }
      })(),
    );
  }

  async #handleLive<TInput, TEvent>(
    live: string,
    descriptor: LiveDesc,
    handler: (
      context: LiveHandlerContext<TInput, TEvent>,
    ) => unknown | Promise<unknown>,
  ): Promise<void> {
    const subject = this.template(descriptor.subject, {}, true).take();
    if (isErr(subject)) throw subject.error;
    const cache = this.#auth.authorityOwner;
    if (!cache) {
      throw createTransportError({
        code: "trellis.live.listen_failed",
        message: "Trellis could not listen for live requests.",
        hint: "Provider authorization cache is required for live observations.",
        context: { live, subject },
      });
    }
    const start = (target: {
      id: number;
      nc: NatsConnection;
      contextDigest: string;
      retainAuthority?: () => VerifiedAuthority;
      lease?: () => TransportLease;
    }) =>
      this.#startLiveProvider({
        ...target,
        live,
        subject,
        descriptor,
        handler,
      });
    // A logical registration installs a provider per admitted generation, so a
    // new opening routes to the generation that admits it while an already
    // accepted session keeps its own generation's provider.
    if (!this.#adaptiveTransport) {
      await start({
        id: 0,
        nc: this.#nats,
        contextDigest: this.#contextDigest(),
        lease: undefined,
      });
      return;
    }
    this.#liveRegistrations.set(live, { install: start });
    for (const ingress of this.#providerIngress.values()) {
      if (ingress.retired) continue;
      const installed = await start({
        id: ingress.id,
        nc: ingress.nc,
        contextDigest: ingress.contextDigest,
        retainAuthority: ingress.retainAuthority,
        lease: ingress.lease,
      });
      ingress.retireFns.push({
        owner: "live",
        drain: installed.drain,
        done: installed.done,
        reopen: installed.reopen,
      });
      ingress.disposeFns.push({ owner: "live", dispose: installed.dispose });
    }
  }

  /**
   * Build one generation's admitted live provider and its intake.
   *
   * The live-open subscription is generic intake (retired when the generation is
   * superseded). The control subscription serves only the sessions this
   * generation accepted and is retired when the generation physically closes.
   * Each accepted session pins this generation through its `lease` until it
   * terminates.
   */
  async #startLiveProvider<TInput, TEvent>(args: {
    id: number;
    nc: NatsConnection;
    contextDigest: string;
    retainAuthority?: () => VerifiedAuthority;
    lease?: () => TransportLease;
    live: string;
    subject: string;
    descriptor: LiveDesc;
    handler: (
      context: LiveHandlerContext<TInput, TEvent>,
    ) => unknown | Promise<unknown>;
  }): Promise<{
    drain: () => Promise<void>;
    done: Promise<void>;
    reopen: () =>
      | { drain: () => Promise<void>; done: Promise<void> }
      | Promise<{ drain: () => Promise<void>; done: Promise<void> }>;
    dispose: () => Promise<void>;
  }> {
    const { id, nc, contextDigest, lease, live, subject, descriptor, handler } =
      args;
    const cache = this.#auth.authorityOwner;
    if (!cache) {
      throw createTransportError({
        code: "trellis.live.listen_failed",
        message: "Trellis could not listen for live requests.",
        hint: "Provider authorization cache is required for live observations.",
        context: { live, subject },
      });
    }
    const ownGuard = await LiveAuthorityGuard.retain(
      cache,
      contextDigest,
      {
        kind: "local-provider",
        apiId: `${descriptor.permission.apiId}@${descriptor.permission.apiVersion}`,
        action: toCatalogAction(descriptor.permission),
      },
      args.retainAuthority,
    );
    const own = ownGuard.identity;
    if (own.binding.kind !== "service" && own.binding.kind !== "device") {
      ownGuard.release();
      throw new Error("live provider requires a provisioned binding");
    }
    const refreshOwnAuthority = async (): Promise<void> => {
      const current = this.#contextDigest();
      if (!current) {
        throw new Error(
          "no current authorization context to refresh live authority",
        );
      }
      const candidate = await ownGuard.prepareReplacement(current);
      const lost = ownGuard.commitReplacement(candidate);
      if (lost !== undefined) {
        throw new Error(`live own authority replacement rejected: ${lost}`);
      }
    };
    const provider = new LiveProvider({
      nats: nc,
      lease,
      identity: {
        connectionId: own.runtimeId,
        sessionKey: own.sessionPublicKey,
        principalId: own.principalId,
        participantId: own.binding.participantId,
        deploymentId: own.binding.deploymentId,
        instanceId: own.binding.instanceId,
      },
      sign: async (digest) => await this.#auth.sign(digest),
      ownGuard,
      refreshOwnAuthority,
      action: toCatalogAction(descriptor.permission),
      retainCallerAuthority: (digest, action) =>
        LiveAuthorityGuard.retain(cache, digest, {
          kind: "observer",
          apiId: `${descriptor.permission.apiId}@${descriptor.permission.apiVersion}`,
          action,
        }),
      manager: this.connection.live,
    });
    let controlSub: ReturnType<NatsConnection["subscribe"]> | undefined;
    // Generic live-open intake. Re-creatable so a survivor reactivation can
    // reinstall the open subscription on the same provider without duplicating
    // the provider or disturbing accepted sessions/receipts.
    const openIntake = (): {
      drain: () => Promise<void>;
      done: Promise<void>;
    } => {
      const openSub = nc.subscribe(subject, {
        queue: routeQueueGroup(subject),
      });
      const openDone = Promise.withResolvers<void>();
      const openTask = AsyncResult.try(async () => {
        // Admission bound before any verification work is spawned: an unbounded
        // set of attacker-supplied openings must not create unbounded tasks.
        let inFlight = 0;
        for await (const msg of openSub) {
          if (inFlight >= MAX_PENDING_OPENINGS) continue;
          inFlight += 1;
          void this.#acceptLiveOpen(
            live,
            descriptor,
            msg,
            handler,
            provider,
          ).finally(() => {
            inFlight -= 1;
          });
        }
      });
      this.#tasks.add(
        `live:${id}:${live}:open:${this.#liveTaskSeq++}`,
        openTask,
      );
      openTask.then(
        () => openDone.resolve(),
        () => openDone.resolve(),
      );
      return {
        drain: () => openSub.drain().catch(() => undefined),
        done: openDone.promise,
      };
    };
    let open: { drain: () => Promise<void>; done: Promise<void> } | undefined;
    try {
      controlSub = nc.subscribe(provider.wildcardSubject(subject));
      // Registered once this generation can actually receive control on the
      // route, so takeover reaches a generation that has not yet served a
      // session; `dispose()` unregisters it transactionally on failure.
      provider.registerRoute(subject);
      open = openIntake();
    } catch (cause) {
      const error = createTransportError({
        code: "trellis.live.listen_failed",
        message: "Trellis could not listen for live requests.",
        hint: "Check the service deployment digest and runtime permissions, then restart the service.",
        cause,
        context: { live, subject },
      });
      recordRuntimeError(error, {
        surface: "live",
        direction: "server",
        operation: live,
        phase: "listen",
      });
      // Clean up the partial install: no subscription exists, so no session
      // can be served; unregister the provider and release its retained own
      // authority rather than leaking the guard for the process lifetime.
      await provider.dispose().catch(() => undefined);
      ownGuard.release();
      throw error;
    }
    const controlTask = AsyncResult.try(async () => {
      for await (const msg of controlSub) {
        await provider.handleControl(msg, async (controlMsg) => {
          const caller = await this.#authenticateLiveRequest({
            msg: controlMsg,
            permission: descriptor.permission,
            requiredCapabilities: descriptor.subscribeCapabilities,
          });
          const callerValue = caller.take();
          if (isErr(callerValue) || callerValue.type !== "verified") {
            return undefined;
          }
          return {
            connectionId: callerValue.runtimeId,
            sessionKey: callerValue.sessionPublicKey,
            principalId: callerValue.principalId,
            binding: callerValue.binding,
            contextDigest: callerValue.authorityDigest,
          };
        });
      }
    });
    this.#tasks.add(
      `live:${id}:${live}:control:${this.#liveTaskSeq++}`,
      controlTask,
    );
    const controlling = controlSub;
    return {
      drain: () => open!.drain(),
      done: open!.done,
      reopen: async () => {
        // Adopt the session's current authorization context before admitting new
        // sessions on a reactivated survivor whose original context may have
        // been superseded by the reduction. A failure to establish verified
        // current authority propagates so the caller rolls the reinstall back
        // rather than admitting fresh sessions under stale evidence.
        await refreshOwnAuthority();
        return (open = openIntake());
      },
      dispose: async () => {
        void controlling.drain().catch(() => undefined);
        await provider.dispose();
        ownGuard.release();
      },
    };
  }

  async #acceptLiveOpen<TInput, TEvent>(
    live: string,
    descriptor: LiveDesc,
    msg: Msg,
    handler: (
      context: LiveHandlerContext<TInput, TEvent>,
    ) => unknown | Promise<unknown>,
    provider: LiveProvider,
  ): Promise<void> {
    let replyOwned = false;
    try {
      const caller = await this.#authenticateLiveRequest({
        msg,
        permission: descriptor.permission,
        requiredCapabilities: descriptor.subscribeCapabilities,
      });
      const callerValue = caller.take();
      if (isErr(callerValue) || callerValue.type !== "verified") {
        // An unverified caller cannot prove its reply destination, so it gets
        // no denial, validation error, or offer on any subject.
        return;
      }
      if (!callerOwnsReply(msg.reply, callerValue.inboxPrefix)) {
        // Never disclose to or reflect onto a destination outside the verified
        // caller's inbox prefix.
        return;
      }
      replyOwned = true;
      const opening = parseLiveOpen(msg.data);
      if (!opening) {
        this.#respondWithError(
          msg,
          createTransportError({
            code: "trellis.live.invalid_request",
            message: "Live opening is not a live open envelope.",
            hint: "Use the live Live client.",
            context: { live },
          }),
        );
        return;
      }
      const parsed = parseRuntimeSchema(
        descriptor.input,
        opening.input as JsonValue,
      ).take();
      if (isErr(parsed)) {
        this.#respondWithError(msg, parsed.error);
        return;
      }
      const subject = this.template(descriptor.subject, {}, true).take();
      if (isErr(subject)) {
        this.#respondWithError(msg, subject.error);
        return;
      }
      await provider.offer(
        msg,
        subject,
        {
          openId: opening.openId,
          receiveMaxPayloadBytes: opening.receiveMaxPayloadBytes,
        },
        {
          connectionId: callerValue.runtimeId,
          sessionKey: callerValue.sessionPublicKey,
          principalId: callerValue.principalId,
          binding: callerValue.binding,
          contextDigest: callerValue.authorityDigest,
        },
        async ({ emit, signal }) => {
          await handler({
            input: parsed as TInput,
            caller: callerValue,
            signal,
            emit: (event: TEvent) =>
              AsyncResult.from(
                (async () => {
                  const payload = encodeRuntimeSchema(
                    descriptor.event,
                    event,
                  ).take();
                  if (isErr(payload)) return payload;
                  await emit(JSON.parse(payload));
                  return ok(undefined);
                })(),
              ),
          });
        },
      );
    } catch (cause) {
      if (!replyOwned) return;
      const error = annotateHandlerBoundaryError(cause, {
        live,
        requestId: msg.headers?.get("request-id"),
        service: this.name,
        contractId: this.contractId,
        contractDigest: this.contractDigest,
        traceId: traceIdFromTraceparent(msg.headers?.get("traceparent")),
      });
      this.#respondWithError(msg, error);
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
      requestJson: (subject, body, expectedDeploymentId) => {
        const route = trellisRoute("operation", operation.toString());
        const error = unavailable?.();
        const permissions = descriptor.permissions;
        const boundStart =
          expectedDeploymentId === undefined
            ? undefined
            : operationStartSubject(descriptor, expectedDeploymentId);
        const isStart =
          subject === descriptor.subject || subject === boundStart;
        if (
          !isStart &&
          (boundStart === undefined || subject !== `${boundStart}.control`)
        ) {
          return AsyncResult.from(
            Promise.resolve(
              err(
                new TransportError({
                  code: "trellis.operation.invalid_route",
                  message:
                    "Operation subject does not match its descriptor and deployment.",
                  hint: "Use the generated Operation start or restored owner reference.",
                }),
              ),
            ),
          );
        }
        const control =
          body && typeof body === "object" && !Array.isArray(body)
            ? body
            : undefined;
        const permission = isStart
          ? permissions.invoke
          : control?.action === "cancel"
            ? permissions.cancel
            : control?.action === "signal" && typeof control.signal === "string"
              ? permissions.control[control.signal]
              : permissions.observe;
        return error
          ? AsyncResult.from(Promise.resolve(err(error)))
          : this.#requestJson(
              subject,
              body as JsonValue,
              route,
              permission,
              expectedDeploymentId,
            );
      },
      watchJson: (subject, body, decodeEvent, expectedDeploymentId) => {
        const error = unavailable?.();
        if (error) return AsyncResult.from(Promise.resolve(err(error)));
        const permissions = Reflect.get(descriptor as object, "permissions") as
          Record<string, DescriptorPermissionAtom> | undefined;
        const observe = permissions?.observe;
        return this.#watchJson(
          subject,
          body as JsonValue,
          decodeEvent,
          observe,
          expectedDeploymentId,
          subject.slice(0, -".control".length),
        );
      },
      putTransfer: (
        grant: SendTransferGrant,
        body: TransferBody,
      ): AsyncResult<FileInfo, TransferOperationError> =>
        AsyncResult.from(
          (async () => {
            const handle = createTransferHandle(
              this.#transport,
              this.#auth,
              this.timeout,
              grant,
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
          })(),
        ),
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
      this.#transport,
      this.#auth,
      this.timeout,
      grant,
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
    // On generations the task is just the broker acknowledgement; on a fixed
    // connection it is the long-running intake and must not be awaited.
    if (this.#adaptiveTransport) await task;
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
    const registration = { method, ctx, subject, fn, handlerTrellis };
    this.#rpcRegistrations.set(method, registration);

    if (this.#adaptiveTransport) {
      // Logical provider core: the handler is registered once and every
      // admitted generation gets its own queue-grouped ingress subscription,
      // so overlapping generations deliver each request exactly once.
      const flushed: Promise<void>[] = [];
      for (const ingress of this.#providerIngress.values()) {
        if (ingress.retired) continue;
        ingress.retireFns.push(this.#installRpcIngress(ingress, registration));
        flushed.push(ingress.nc.flush());
      }
      // Registration is usable only once the broker has acknowledged SUB.
      return AsyncResult.try(async () => {
        await Promise.all(flushed);
      });
    }

    const sub = this.#nats.subscribe(subject, {
      queue: routeQueueGroup(subject),
    });
    return this.#runRpcIntake(sub, registration);
  }

  /**
   * Register an internal framework surface that installs its own generic
   * intake per admitted generation.
   *
   * Used by the operation runtime so start/reconcile/control intake is created
   * on every generation with stable queue groups rather than on one fixed
   * connection. The returned `drain`/`done` participate in the ingress lease
   * lifetime. @internal
   */
  /** Whether this connection owns physical generations through a provider. */
  protected get adaptiveTransport(): boolean {
    return this.#adaptiveTransport;
  }

  protected async declareGenerationIntake(
    id: string,
    install: GenerationIntakeInstall,
  ): Promise<void> {
    this.#generationIntakes.set(id, install);
    if (!this.#adaptiveTransport) return;
    for (const ingress of this.#providerIngress.values()) {
      if (ingress.retired) continue;
      const installed = await install({
        id: ingress.id,
        nc: ingress.nc,
        contextDigest: ingress.contextDigest,
        retainAuthority: ingress.retainAuthority,
        lease: ingress.lease,
      });
      ingress.retireFns.push({
        intakeId: id,
        drain: installed.drain,
        done: installed.done,
      });
      ingress.disposeFns.push({ intakeId: id, dispose: installed.dispose });
      // Registration is usable only once the broker has acknowledged SUB.
      await ingress.nc.flush();
    }
  }

  /**
   * Register framework-owned per-generation intake on this logical connection.
   *
   * Installs on every already-admitted generation and on every future candidate
   * before it becomes the default. `retractFrameworkIntake` stops and disposes
   * the installed intake so a later candidate can never resurrect a stopped
   * framework host. @internal
   */
  async declareFrameworkIntake(
    id: string,
    install: GenerationIntakeInstall,
  ): Promise<void> {
    await this.declareGenerationIntake(id, install);
  }

  /**
   * Retract one framework intake registration and stop its installed intake on
   * every admitted generation. Accepted work is drained before each entry's
   * `done` resolves; `dispose` releases per-generation resources. @internal
   */
  async retractFrameworkIntake(id: string): Promise<void> {
    this.#generationIntakes.delete(id);
    if (!this.#adaptiveTransport) return;
    const pending: Promise<void>[] = [];
    for (const ingress of this.#providerIngress.values()) {
      const retired = ingress.retireFns.filter(
        (entry) => entry.intakeId === id,
      );
      if (retired.length > 0) {
        ingress.retireFns = ingress.retireFns.filter(
          (entry) => entry.intakeId !== id,
        );
        for (const entry of retired) {
          pending.push(
            (async () => {
              await entry.drain();
              await entry.done;
            })(),
          );
        }
      }
      const disposed = ingress.disposeFns.filter(
        (entry) => entry.intakeId === id,
      );
      if (disposed.length > 0) {
        ingress.disposeFns = ingress.disposeFns.filter(
          (entry) => entry.intakeId !== id,
        );
        for (const entry of disposed) {
          // Invoke inside an async wrapper so a synchronous throw in `dispose`
          // is a rejection for this entry, not an abort of the whole retraction.
          pending.push(
            (async () => await entry.dispose())().catch(() => undefined),
          );
        }
      }
    }
    await Promise.allSettled(pending);
  }

  /**
   * Install (or reinstall) provider ingress for one admitted generation.
   *
   * Registers the queue-grouped subscriptions for every handler in the logical
   * provider core. Called before the generation becomes the default so routing
   * exists before new work can select it. Reinstalling a drained survivor
   * restores only this generic intake scope; session/control owners registered
   * outside it are left intact. @internal
   */
  async installProviderIngress(target: {
    id: number;
    nc: NatsConnection;
    /** Exact admitted context this generation's providers are built from. */
    contextDigest: string;
    retainAuthority?: () => VerifiedAuthority;
    /**
     * Lease the exact admitted generation the candidate ingress routes on. It is
     * called once for the ingress-lifetime pin and again per accepted callback;
     * it throws when the generation is no longer admitted, which fails the
     * candidate rather than executing unpinned.
     */
    lease?: () => TransportLease;
    /**
     * Push this ingress's concrete cleanup scope into the owning manager
     * synchronously, before the first install await, so cleanup ownership is
     * never a fallible late lookup. @internal
     */
    registerDisposal?: (handle: ProviderGenerationAttachment) => boolean;
  }): Promise<ProviderGenerationAttachment> {
    const existing = this.#providerIngress.get(target.id);
    if (existing && !existing.retired) {
      return this.#attachmentFor(target.id);
    }
    const ingress: ProviderIngress = existing ?? {
      id: target.id,
      nc: target.nc,
      contextDigest: target.contextDigest,
      lease: target.lease,
      retireFns: [],
      disposeFns: [],
    };
    this.#providerIngress.set(target.id, ingress);
    if (!existing) {
      // First installation only: register the concrete scope now, before any
      // await, so the retained ingress is always owned and disposable even if a
      // later install step fails partway. A false result means the generation is
      // already retired and the manager has taken over this scope's cleanup.
      const accepted = target.registerDisposal?.(
        this.#attachmentFor(target.id),
      );
      if (accepted === false) {
        ingress.retireFns = [];
        ingress.disposeFns = [];
        throw ingressRetiredError(target.id);
      }
    }
    // Reactivation of a drained survivor: reinstall only generic intake
    // (RPC serve + framework intake) before the first await so a partial
    // install is always discoverable and disposable. Live/control/session
    // owners registered outside generic intake are preserved.
    const reinstateGeneric = existing?.retired === true;
    // Reactivation must never resurrect a scope whose disposal has begun; the
    // manager only asks for reactivation of a non-disposing survivor, and this
    // guards the reset from being reached any other way. The manager's
    // `#disposing` boundary is the authority for irreversibility.
    if (existing?.disposing) {
      throw ingressRetiredError(target.id);
    }
    let liveEntries: ProviderIngress["retireFns"] = [];
    if (reinstateGeneric) {
      liveEntries = ingress.retireFns.filter((entry) => entry.owner === "live");
      ingress.retireFns = ingress.retireFns.filter(
        (entry) => entry.owner !== "rpc" && entry.intakeId === undefined,
      );
      // Replacing generic intake does not release its accepted work. Keep every
      // previous framework disposal owner until this physical generation ends,
      // so forced loss also reaches attempts retained by a replaced intake.
    } else {
      ingress.retireFns = [];
      ingress.disposeFns = [];
    }
    ingress.nc = target.nc;
    ingress.contextDigest = target.contextDigest;
    ingress.retainAuthority = target.retainAuthority;
    ingress.lease = target.lease;
    ingress.retired = false;
    ingress.intakeStopped = undefined;
    ingress.disposing = undefined;
    ingress.intakeLease = target.lease?.();
    // Fence for resources created across an install await: the owner scope may
    // have been force-retired while a subscription/consumer was being set up.
    // Identity alone is not enough — `disposeProviderIngress` flags `disposing`
    // and snapshots `disposeFns` before it deletes the map entry, so a pending
    // installer that resolves in that window would push a new owner after the
    // snapshot and have it wiped without cleanup. Retired or disposing scopes
    // therefore reject further installation too.
    const stillOwned = (): boolean =>
      this.#providerIngress.get(ingress.id) === ingress &&
      !ingress.retired &&
      !ingress.disposing;
    try {
      // Reinstall the live providers' generic open intake on the same provider
      // first, under the same owned rollback: their control subscription,
      // retained authority, and accepted sessions are preserved, not duplicated,
      // and a partial reopen is retired like any other partial generic install.
      for (const entry of liveEntries) {
        if (!entry.reopen) continue;
        const reopened = await entry.reopen();
        entry.drain = reopened.drain;
        entry.done = reopened.done;
        if (!stillOwned()) {
          await this.#releaseDetached(entry.drain);
          throw ingressRetiredError(target.id);
        }
      }
      for (const registration of this.#rpcRegistrations.values()) {
        ingress.retireFns.push({
          owner: "rpc",
          ...this.#installRpcIngress(ingress, registration),
        });
      }
      if (!reinstateGeneric) {
        for (const registration of this.#liveRegistrations.values()) {
          const installed = await registration.install({
            id: ingress.id,
            nc: ingress.nc,
            contextDigest: ingress.contextDigest,
            retainAuthority: ingress.retainAuthority,
            lease: ingress.lease,
          });
          if (!stillOwned()) {
            // Release both the just-created generic open subscription and the
            // control/authority scope; an open sub left running until the socket
            // closes would keep accepting sessions on a retired generation.
            await this.#releaseDetached(installed.drain, installed.dispose);
            throw ingressRetiredError(target.id);
          }
          ingress.retireFns.push({
            owner: "live",
            drain: installed.drain,
            done: installed.done,
            reopen: installed.reopen,
          });
          ingress.disposeFns.push({
            owner: "live",
            dispose: installed.dispose,
          });
        }
      }
      for (const [intakeId, intake] of this.#generationIntakes) {
        const installed = await intake({
          id: ingress.id,
          nc: ingress.nc,
          contextDigest: ingress.contextDigest,
          retainAuthority: ingress.retainAuthority,
          lease: ingress.lease,
        });
        if (!stillOwned()) {
          await this.#releaseDetached(installed.dispose);
          throw ingressRetiredError(target.id);
        }
        ingress.retireFns.push({
          intakeId,
          drain: installed.drain,
          done: installed.done,
        });
        ingress.disposeFns.push({
          intakeId,
          dispose: installed.dispose,
        });
      }
      // The SUBs above are only queued client-side. Flush proves the broker has
      // accepted the candidate's routes before it becomes the default and the
      // superseded intake is drained, so cutover cannot drop a request into a
      // gap with no queue-group member.
      await target.nc.flush();
    } catch (cause) {
      // Retain the partially installed attachment: registered intake may have
      // accepted work. Stop new deliveries and keep it discoverable so the
      // owner's retire/dispose settles what was accepted instead of detaching
      // it. The candidate is still reported as failed.
      this.#retireIngress(ingress);
      throw cause;
    }
    return this.#attachmentFor(target.id);
  }

  #attachmentFor(id: number): ProviderGenerationAttachment {
    const ingresses = this.#providerIngress;
    return {
      retireIntake: () => this.retireProviderIngress(id),
      get intakeStopped() {
        return ingresses.get(id)?.intakeStopped ?? Promise.resolve();
      },
      dispose: () => this.disposeProviderIngress(id),
    };
  }

  /** Stop new intake and record when every outstanding intake loop settles. */
  #retireIngress(ingress: ProviderIngress): void {
    if (ingress.retired) return;
    ingress.retired = true;
    // Capture this retirement's own scope: a later reactivation installs a new
    // intake lease and loop set, and this retirement must release exactly the
    // lease and loops it retired, never the reinstalled pin.
    const retiredLease = ingress.intakeLease;
    const done = ingress.retireFns.map(async (entry) => {
      await entry.drain();
      return await entry.done;
    });
    ingress.intakeStopped = Promise.allSettled(done).then(() => {
      retiredLease?.release();
      if (ingress.intakeLease === retiredLease) ingress.intakeLease = undefined;
    });
  }

  /**
   * Stop new provider intake on one generation.
   *
   * `drain()` stops new broker deliveries and protocol-flushes; it does **not**
   * wait for messages already buffered in the client iterator. The
   * ingress-lifetime lease is therefore released only once every intake *loop*
   * has finished, so a buffered accepted delivery stays protected through its
   * per-callback lease. Already-accepted callbacks keep running independently.
   * @internal
   */
  retireProviderIngress(id: number): void {
    const ingress = this.#providerIngress.get(id);
    if (!ingress) return;
    this.#retireIngress(ingress);
  }

  /**
   * Release one generation's provider resources.
   *
   * Called after intake has stopped and accepted leases have drained, so owned
   * sessions and retained authority are torn down coherently. Idempotent; a
   * concurrent call shares the one run. @internal
   */
  async disposeProviderIngress(id: number): Promise<void> {
    const ingress = this.#providerIngress.get(id);
    if (!ingress) return;
    if (ingress.disposing) {
      await ingress.disposing;
      return;
    }
    ingress.disposing = (async () => {
      // Release the ingress-lifetime pin and run coherent per-generation
      // disposal. The graceful caller has already awaited intake; a forced path
      // invokes disposal without blocking on accepted work.
      const disposingLease = ingress.intakeLease;
      disposingLease?.release();
      if (ingress.intakeLease === disposingLease) {
        ingress.intakeLease = undefined;
      }
      await Promise.allSettled(
        ingress.disposeFns.map(async (entry) => await entry.dispose()),
      );
      ingress.disposeFns.length = 0;
      this.#providerIngress.delete(id);
    })();
    await ingress.disposing;
  }

  /**
   * Release resources an owner created across an install await after its scope
   * was force-retired. Every cleanup is attempted even if another fails, and
   * failures are reported, never silently dropped.
   */
  async #releaseDetached(
    ...cleanups: Array<() => Promise<void> | void>
  ): Promise<void> {
    const results = await Promise.allSettled(
      cleanups.map(async (cleanup) => await cleanup()),
    );
    for (const result of results) {
      if (result.status === "rejected") {
        this.#log.warn(
          { error: result.reason },
          "detached provider intake cleanup failed",
        );
      }
    }
  }

  #installRpcIngress(
    ingress: {
      id: number;
      nc: NatsConnection;
      lease?: () => TransportLease;
      retireFns: Array<{
        intakeId?: string;
        drain: () => Promise<void>;
        done: Promise<void>;
      }>;
    },
    registration: {
      method: MethodsOf<TA>;
      ctx: RpcDescriptorOf<TA, MethodsOf<TA>>;
      subject: string;
      fn: HandlerFn<TA, MethodsOf<TA>, TA, HandlerTrellis<TA, TRequests>>;
      handlerTrellis: HandlerTrellis<TA, TRequests>;
    },
  ): { drain: () => Promise<void>; done: Promise<void> } {
    const sub = ingress.nc.subscribe(registration.subject, {
      queue: routeQueueGroup(registration.subject),
    });
    const completion = Promise.withResolvers<void>();
    const intake = this.#runRpcIntake(sub, registration, ingress.lease);
    this.#tasks.add(
      // Unique per install so a survivor reactivation can reinstall its RPC
      // intake without colliding with the drained install's retained task.
      `rpc:${ingress.id}:${registration.method}:${this.#rpcTaskSeq++}`,
      intake,
    );
    // `done` resolves when the intake loop itself finishes consuming buffered
    // messages, independent of when `drain()` protocol-flushes.
    intake.then(
      () => completion.resolve(),
      () => completion.resolve(),
    );
    return {
      drain: () => sub.drain().catch(() => undefined),
      done: completion.promise,
    };
  }

  #runRpcIntake(
    sub: ReturnType<NatsConnection["subscribe"]>,
    registration: {
      method: MethodsOf<TA>;
      ctx: RpcDescriptorOf<TA, MethodsOf<TA>>;
      subject: string;
      fn: HandlerFn<TA, MethodsOf<TA>, TA, HandlerTrellis<TA, TRequests>>;
      handlerTrellis: HandlerTrellis<TA, TRequests>;
    },
    /** Pin the generation that accepted this callback through its reply. */
    leaseFactory?: () => TransportLease,
  ): AsyncResult<void, ValidationError | UnexpectedError> {
    const { method, ctx, fn, handlerTrellis } = registration;
    return AsyncResult.try(async () => {
      for await (const msg of sub) {
        let lease: TransportLease | undefined;
        if (leaseFactory) {
          try {
            lease = leaseFactory();
          } catch {
            // The generation is no longer admitted; never execute unpinned.
            continue;
          }
        }
        try {
          const resultPromise = await this.#processRPCMessage(
            method,
            ctx,
            msg,
            fn,
            handlerTrellis,
          );
          const result = resultPromise.take();

          if (isErr(result)) {
            await this.#respondWithError(msg, result.error, {
              method: String(method),
            });
            continue;
          }

          const sent = await this.respondWithPayload(
            msg,
            result.payload,
            undefined,
            {
              method: String(method),
              responseKind: "success",
            },
          );
          if (sent.isErr()) {
            const responseBytes = payloadByteLength(result.payload);
            const message = causeMessage(sent.error.cause);
            await this.#respondWithError(
              msg,
              new TransportError({
                code: "trellis.rpc.response_send_failed",
                message: message.includes("max_payload")
                  ? "Trellis RPC response exceeded NATS max_payload."
                  : "Trellis could not send the RPC response.",
                hint: "Reduce the requested page size or use a narrower RPC that does not include large detail payloads.",
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
        } finally {
          lease?.release();
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
    const span = startServerSpan(trellisRoute("rpc", method), parentContext);
    const incomingTraceId = traceIdFromTraceparent(
      msg.headers?.get("traceparent"),
    );

    const serverRoute = trellisRoute("rpc", method);
    const serverStartedAt = performance.now();
    recordCatalogUpDown("trellis.rpc.server.inflight", 1, {
      "trellis.route": serverRoute,
    });
    const finishServerObservation = (
      outcome:
        | "ok"
        | "declared_error"
        | "invalid"
        | "denied"
        | "rate_limited"
        | "timeout"
        | "unavailable"
        | "cancelled"
        | "error",
    ): void => {
      recordCatalogUpDown("trellis.rpc.server.inflight", -1, {
        "trellis.route": serverRoute,
      });
      recordCatalogDuration(
        "trellis.rpc.server.duration",
        performance.now() - serverStartedAt,
        { "trellis.route": serverRoute, "trellis.outcome": outcome },
      );
    };

    // Execute the handler within the span's context
    return withSpanAsync(span, async () => {
      const afterReply: (() => MaybePromise<void>)[] = [];
      const execute = async (): Promise<
        Result<ProcessedRpcResponse, BaseError>
      > => {
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
            cache: this.#auth.authorityOwner,
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
          await this.rememberVerifiedRequest(msg, authValue, ctx.permission);
        }

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

        span.setAttribute("auth.caller.type", caller.type);
        if (caller.type === "verified") {
          span.setAttribute("auth.principal.kind", caller.principalKind);
          span.setAttribute("auth.principal.id", caller.principalId);
          span.setAttribute("auth.runtime.id", caller.runtimeId);
          span.setAttribute(
            "auth.authorization_session.id",
            caller.authorizationSessionId,
          );
          if (caller.loginSessionId) {
            span.setAttribute("auth.login_session.id", caller.loginSessionId);
          }
          if (
            caller.binding.kind === "service" ||
            caller.binding.kind === "device"
          ) {
            span.setAttribute(
              "auth.participant.id",
              caller.binding.participantId,
            );
            span.setAttribute(
              "auth.deployment.id",
              caller.binding.deploymentId,
            );
            span.setAttribute("auth.instance.id", caller.binding.instanceId);
          }
        }

        const invokeHandler = fn as (args: {
          input: unknown;
          context: RpcHandlerContext;
          client: HandlerTrellis<TA, TRequests>;
        }) => MaybeAsync<unknown, BaseError>;
        const handlerResultWrapped = await AsyncResult.try(
          async () =>
            await Promise.resolve(
              invokeHandler({
                input: parsedInput,
                context: {
                  caller,
                  sessionKey:
                    caller.type === "verified" ? caller.sessionPublicKey : "",
                  inboxPrefix: callerInboxPrefix,
                  permission: ctx.permission,
                  requiredCapabilities: ctx.callerCapabilities,
                  requestId: handlerRequestIdFromHeader || undefined,
                  traceId: handlerTraceIdFromHeader || undefined,
                  afterReply: (task) => afterReply.push(task),
                },
                client: handlerTrellis,
              }),
            ),
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
              cause:
                error.cause instanceof Error
                  ? { message: error.cause.message, stack: error.cause.stack }
                  : error.cause,
            },
            "Handler threw unexpectedly.",
          );
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: "handler failed",
          });
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
              cause:
                error.cause instanceof Error
                  ? { message: error.cause.message, stack: error.cause.stack }
                  : error.cause,
            },
            "Handler returned error.",
          );
          span.setStatus({
            code: SpanStatusCode.ERROR,
            message: "handler error",
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
        finishServerObservation(outcomeForFailure(result.error));
      } else {
        finishServerObservation("ok");
      }
      span.end();
      return result;
    });
  }

  /** Retain the exact verified opening for certified replies from RPC and Operation handlers. @internal */
  protected async rememberVerifiedRequest(
    msg: Msg,
    caller: VerifiedCaller,
    permission: DescriptorPermissionAtom,
  ): Promise<void> {
    const request: SessionRequest = {
      authorityDigest: caller.authorityDigest,
      apiId: `${permission.apiId}@${permission.apiVersion}`,
      apiGeneration: msg.headers!.get("Trellis-Api-Generation")!,
      acceptedRevision: msg.headers!.get("Trellis-Accepted-Revision")!,
      action: permission.surfaceName,
      subject: msg.subject,
      replySubject: msg.reply!,
      issuedAt: Number(msg.headers!.get("iat")),
      requestId: msg.headers!.get("request-id")!,
    };
    this.#replyRequests.set(msg, {
      request,
      digest: base64urlEncode(
        await sessionRequestSigningDigest(request, msg.data),
      ),
    });
  }

  /** Sign a response against its retained verified opening before publishing it. @internal */
  protected async respondWithPayload(
    msg: Msg,
    payload: string,
    options: { headers?: MsgHdrs } | undefined,
    context: {
      method?: string;
      responseKind: "success" | "error";
    },
  ): Promise<Result<void, UnexpectedError>> {
    const responseBytes = payloadByteLength(payload);
    try {
      const opening = this.#replyRequests.get(msg);
      if (opening) {
        const owner = this.#auth.authorityOwner;
        if (!owner) throw new Error("session authority owner unavailable");
        const own = owner.own();
        try {
          const binding = own.authority.providerBindings.find(
            (binding) =>
              binding.apiId === opening.request.apiId &&
              binding.generation === opening.request.apiGeneration,
          );
          if (!binding)
            throw new Error(
              "provider certificate is not in verified authority",
            );
          const response: SessionResponse = {
            trellisInstanceId: owner.instanceId,
            audienceNatsAccount: owner.account,
            providerAuthorityDigest: own.digest,
            providerCertificateDigest: binding.providerCertificateDigest,
            apiId: opening.request.apiId,
            apiGeneration: binding.generation,
            action: opening.request.action,
            requestAuthorityDigest: opening.request.authorityDigest,
            requestId: opening.request.requestId,
            requestProofDigest: opening.digest,
            subject: msg.reply!,
            kind: context.responseKind,
            issuedAt: this.#currentIat(),
          };
          const proof = base64urlEncode(
            await this.#auth.sign(
              await sessionResponseSigningDigest(
                response,
                new TextEncoder().encode(payload),
              ),
            ),
          );
          own.assertCurrent();
          const responseHeaders = options?.headers ?? natsHeaders();
          responseHeaders.set(
            "Trellis-Session-Response",
            JSON.stringify(response),
          );
          responseHeaders.set("proof", proof);
          options = { ...options, headers: responseHeaders };
        } finally {
          own.release();
        }
      }
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

  async #respondWithError(
    msg: Msg,
    error: Error | BaseError,
    context: { method?: string; responseBytes?: number } = {},
  ): Promise<void> {
    const trellisError =
      error instanceof BaseError && !(error instanceof RemoteError)
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
      await this.respondWithPayload(
        msg,
        '{"type":"UnexpectedError","message":"Failed to serialize error"}',
        { headers: hdrs },
        { method: context.method, responseKind: "error" },
      );
      return;
    }
    await this.respondWithPayload(
      msg,
      serialized.take() as string,
      { headers: hdrs },
      { method: context.method, responseKind: "error" },
    );
  }

  /** Publish a signed error for a previously verified request. */
  async respondWithError(msg: Msg, error: Error | BaseError): Promise<void> {
    await this.#respondWithError(msg, error);
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

      return ok(
        Object.freeze({
          event: event.toString(),
          descriptorIdentity: ctx.descriptorIdentity,
          subject,
          header: Object.freeze(header),
          payload,
          encodedPayload: msg,
          headers: Object.freeze(headerRecord),
        }),
      );
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
  ): AsyncResult<void, TransportError | UnexpectedError> {
    return AsyncResult.from(
      (async () => {
        const startedAt = performance.now();
        const route = trellisRoute("event", event.descriptorIdentity);
        const finish = (outcome: "ok" | "error"): void => {
          recordCatalogDuration(
            "trellis.event.publish.duration",
            performance.now() - startedAt,
            {
              "trellis.route": route,
              "trellis.delivery": "durable",
              "trellis.outcome": outcome,
            },
          );
        };
        // Generation acquisition and the publish+ack share one deadline.
        const deadlineMs = Date.now() + this.timeout;
        let lease: TransportLease | undefined;
        if (this.#adaptiveTransport) {
          try {
            lease = await this.#transport.acquireFor(
              { publish: [event.subject] },
              { deadlineMs },
            );
          } catch (cause) {
            finish("error");
            const error = mapTransportAcquireFailure({
              subject: event.subject,
              cause,
            });
            recordRuntimeError(error, {
              surface: "event",
              direction: "publisher",
              operation: event.event,
              phase: "publish",
            });
            return err(error);
          }
        }
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
          headers.set("Trellis-Api-Generation", proof.apiGeneration);
          headers.set("Trellis-Accepted-Revision", proof.acceptedRevision);
          headers.set("Trellis-Session-Event", JSON.stringify(proof.event));
          headers.set("session-key", this.#auth.sessionKey);

          logger.trace(
            { subject: event.subject },
            `Publishing ${event.event} event.`,
          );
          const js = lease ? jetstream(lease.nc) : this.#js;
          await js.publish(event.subject, event.encodedPayload, {
            headers,
            timeout: Math.max(1, deadlineMs - Date.now()),
          });
          finish("ok");
          return ok(undefined);
        } catch (cause) {
          finish("error");
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
        } finally {
          lease?.release();
        }
      })(),
    );
  }

  publish(
    event: string,
    data: Record<string, unknown>,
  ): AsyncResult<void, ValidationError | TransportError | UnexpectedError> {
    const prepared = this.prepare(event, data).take();
    if (isErr(prepared)) {
      return AsyncResult.from<
        never,
        ValidationError | TransportError | UnexpectedError
      >(Promise.resolve(prepared));
    }
    return this.publishPrepared(prepared);
  }

  listenEvent<E extends EventsOf<TA>>(
    event: E,
    subjectData: Record<string, unknown>,
    fn: EventCallback<EventOf<TA, E>>,
    opts?: EventOpts,
  ): AsyncResult<
    void,
    AuthError | ValidationError | TransportError | UnexpectedError
  > {
    return AsyncResult.from(
      (async () => {
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
            // A declared durable consumer grants Consume authority only. Raw
            // ephemeral observation needs its own Event Subscribe need; fail
            // fast rather than sitting on a subscription the broker will not
            // deliver to.
            const need =
              (ctx as { descriptorName?: string }).descriptorName ??
              `event:${eventName}`;
            if (
              this.#ephemeralEventNeeds !== undefined &&
              !this.#ephemeralEventNeeds.has(need)
            ) {
              return err(
                new AuthError({
                  reason: "insufficient_permissions",
                  message:
                    `ephemeral event delivery for '${eventName}' requires an Event Subscribe authority; ` +
                    "a declared consumer grants durable delivery only",
                  context: { event: String(eventName), requiredNeed: need },
                }),
              );
            }
            // An ephemeral observation pins one generation for its lifetime: the
            // subscription is opened and closed on the same physical connection.
            let lease: TransportLease | undefined;
            if (this.#adaptiveTransport) {
              try {
                lease = await this.#transport.acquireFor(
                  { subscribe: [subject] },
                  {
                    ...(opts.signal ? { signal: opts.signal } : {}),
                    // Bound opening with the normal request budget instead of an
                    // unbounded adoption wait when no signal is supplied.
                    deadlineMs: Date.now() + this.timeout,
                  },
                );
              } catch (cause) {
                return err(mapTransportAcquireFailure({ subject, cause }));
              }
            }
            return await this.#startEphemeralEvent(
              eventName,
              ctx,
              subject,
              fn,
              opts.signal,
              lease,
            );
          }

          const groupResult = this.#durableEvents.resolveGroup(eventName, opts);
          const group = groupResult.take();
          if (isErr(group)) return group;

          this.#durableEvents.register({
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
            new UnexpectedError({
              cause,
              context: { event: event.toString() },
            }),
          );
        }
      })(),
    );
  }

  async #startEphemeralEvent(
    event: EventsOf<TA>,
    ctx: EventDescriptorOf<TA, EventsOf<TA>>,
    subject: string,
    fn: EventCallback<EventOf<TA, EventsOf<TA>>>,
    signal?: AbortSignal,
    lease?: TransportLease,
  ): Promise<Result<void, ValidationError | UnexpectedError>> {
    const nc = lease?.nc ?? this.#nats;
    let sub: ReturnType<NatsConnection["subscribe"]> | undefined;
    try {
      sub = nc.subscribe(subject);
      if (signal) {
        if (signal.aborted) {
          sub.unsubscribe();
          lease?.release();
          return ok(undefined);
        }
        signal.addEventListener(
          "abort",
          () => {
            sub?.unsubscribe();
            lease?.release();
          },
          { once: true },
        );
      }
    } catch (cause) {
      if (sub) {
        sub.unsubscribe();
      }
      lease?.release();
      return err(
        new UnexpectedError({
          cause,
          context: { event: String(event), subject },
        }),
      );
    }

    const task = AsyncResult.try(async () => {
      try {
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
      } finally {
        lease?.release();
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
    // One handler-delivery attempt including its existing verification and
    // disposition boundary.
    const route = trellisRoute("consumer", String(args.event));
    const startedAt = performance.now();
    const finish = (outcome: string): void => {
      recordCatalogDuration(
        "trellis.event.process.duration",
        performance.now() - startedAt,
        { "trellis.route": route, "trellis.outcome": outcome },
      );
    };
    try {
      const result = await Promise.resolve(
        args.fn(
          args.payload as EventOf<TA, EventsOf<TA>>,
          createEventListenerContext({
            subject: args.message.subject,
            mode: args.mode,
            ...(args.group ? { group: args.group } : {}),
            message: args.message,
          }),
        ),
      );
      const outcome = isResultLike(result) ? result.take() : result;
      if (isErr(outcome)) {
        finish("retry");
        return err(annotateHandlerBoundaryError(outcome.error, annotation));
      }
      finish("ok");
      return ok(undefined);
    } catch (cause) {
      finish("error");
      return err(annotateHandlerBoundaryError(cause, annotation));
    }
  }

  /** @internal Read authoritative history for a verified deployment-owned job. */
  async requestJobHistory(
    input: JobsHistoryRequest,
  ): Promise<JobsHistoryResponse> {
    const action = JOBS_HISTORY_API.actions["rpc:History"];
    return await this.#requestBuiltRpc<JobsHistoryResponse>("History", input, {
      subject: action.subject,
      permission: actionPermission(JOBS_HISTORY_API.identity, action, "call"),
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
    _event: EventsOf<TA>,
    descriptor: EventDescriptorOf<TA, EventsOf<TA>>,
    msg: Pick<Msg, "data" | "headers" | "subject">,
    originalPublication?: OriginalEventPublication,
  ): Promise<Result<VerifiedCaller, BaseError>> {
    return await verifyLocalAuthorization({
      kind: "event",
      cache: this.#auth.authorityOwner,
      message: msg,
      originalPublication,
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
    this.#durableEvents.stop();
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
        value !== undefined &&
        value !== null &&
        value !== "*" &&
        ((typeof value !== "string" && typeof value !== "number") ||
          (typeof value === "number" &&
            (!Number.isFinite(value) ||
              (Number.isInteger(value) && !Number.isSafeInteger(value)))))
      ) {
        return err(
          new ValidationError({
            errors: [
              {
                path: key,
                message: "Subject template values must be strings or numbers",
              },
            ],
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
    const digest =
      typeof this.#auth.contextDigest === "function"
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
    permission: DescriptorPermissionAtom,
    role: "consumer" | "provider" = "consumer",
  ): Promise<{
    proof: string;
    iat: number;
    requestId: string;
    contextDigest: string;
    request: SessionRequest;
    requestProofDigest: string;
  }> {
    const owner = this.#auth.authorityOwner;
    if (!owner) throw new Error("session authority owner unavailable");
    const own = owner.own();
    try {
      const contextDigest = own.digest;
      const payloadBytes = new TextEncoder().encode(payload);
      const iat = this.#currentIat();
      const requestId = ulid();
      const apiId = `${permission.apiId}@${permission.apiVersion}`;
      const api = own.authority.apis.find((api) => api.apiId === apiId);
      if (!api) throw new Error("request API is not in verified authority");
      if (role === "provider")
        await own.authorizeProvider(
          apiId,
          api.generation,
          toCatalogAction(permission),
        );
      else await own.authorize(apiId, toCatalogAction(permission));
      const request: SessionRequest = {
        authorityDigest: contextDigest,
        apiId,
        apiGeneration: api.generation,
        acceptedRevision: api.acceptedRevision,
        action: permission.surfaceName,
        subject,
        replySubject: reply,
        issuedAt: iat,
        requestId,
      };
      const digest = await sessionRequestSigningDigest(request, payloadBytes);
      const sigBytes = await this.#auth.sign(digest);
      own.assertCurrent();
      return {
        proof: base64urlEncode(sigBytes),
        iat,
        requestId,
        contextDigest,
        request,
        requestProofDigest: base64urlEncode(digest),
      };
    } finally {
      own.release();
    }
  }

  async #createEventProof(event: PreparedTrellisEvent): Promise<{
    proof: string;
    contextDigest: string;
    apiGeneration: string;
    acceptedRevision: string;
    event: SessionEvent;
  }> {
    const owner = this.#auth.authorityOwner;
    if (!owner) throw new Error("session authority owner unavailable");
    const own = owner.own();
    try {
      const permission = this.api.events[event.event]?.publishPermission;
      if (!permission) throw new Error("event descriptor unavailable");
      const apiId = `${permission.apiId}@${permission.apiVersion}`;
      const api = own.authority.apis.find((api) => api.apiId === apiId);
      if (!api) throw new Error("event API is not in verified authority");
      await own.authorize(apiId, toCatalogAction(permission));
      const signedEvent: SessionEvent = {
        authorityDigest: own.digest,
        apiId,
        apiGeneration: api.generation,
        acceptedRevision: api.acceptedRevision,
        action: permission.surfaceName,
        subject: event.subject,
        eventId: event.header.id,
        eventTime: event.header.time,
      };
      const digest = await sessionEventSigningDigest(
        signedEvent,
        new TextEncoder().encode(event.encodedPayload),
      );
      const proof = base64urlEncode(await this.#auth.sign(digest));
      own.assertCurrent();
      return {
        proof,
        contextDigest: own.digest,
        event: signedEvent,
        apiGeneration: api.generation,
        acceptedRevision: api.acceptedRevision,
      };
    } finally {
      own.release();
    }
  }

  /**
   * Acquire the physical transport for one finite request attempt.
   *
   * When generation routing is installed, the exact generation covering the
   * request is leased for the whole exchange; otherwise the single fixed
   * connection is returned with a no-op release. Acquisition failures become
   * ordinary request transport errors, never `transport_upgrade_required`.
   */
  async #acquireRequestTransport(args: {
    method?: string;
    subject: string;
    deadlineMs: number;
    signal?: AbortSignal;
    callerCapabilities?: readonly string[];
  }): Promise<Result<TransportLease, TransportError>> {
    try {
      const authority = this.ownAuthority;
      if (authority && !authority.current) {
        const signal = AbortSignal.any([
          ...(args.signal ? [args.signal] : []),
          AbortSignal.timeout(Math.max(0, args.deadlineMs - Date.now())),
        ]);
        await new Promise<void>((resolve, reject) => {
          let unsubscribe = () => {};
          const changed = () => {
            if (!authority.current && !this.#transport.isClosed()) return;
            unsubscribe();
            signal.removeEventListener("abort", aborted);
            resolve();
          };
          const aborted = () => {
            unsubscribe();
            signal.removeEventListener("abort", aborted);
            reject(
              requestFailedTransportError({
                code: args.signal?.aborted
                  ? "trellis.request.cancelled"
                  : "trellis.request.timeout",
                message: args.signal?.aborted
                  ? "The request was cancelled."
                  : "The request timed out waiting for verified authority.",
                hint: "Wait for verified authority before making another request.",
                method: args.method,
                subject: args.subject,
                cause: signal.reason,
              }),
            );
          };
          signal.addEventListener("abort", aborted, { once: true });
          unsubscribe = authority.subscribe(changed);
          // subscribe invokes synchronously; recheck after retaining cleanup.
          if (signal.aborted) aborted();
          else changed();
        });
      }
    } catch (cause) {
      return err(
        cause instanceof TransportError
          ? cause
          : mapTransportAcquireFailure({ ...args, cause }),
      );
    }
    if (!this.#adaptiveTransport) {
      if (this.#nats.isClosed()) {
        return err(
          requestFailedTransportError({
            code: "trellis.request.closed",
            message: "The Trellis connection is closed.",
            hint: "Connect to Trellis again before making another request.",
            method: args.method,
            subject: args.subject,
          }),
        );
      }
      return ok(await this.#transport.acquireCurrent());
    }
    let lease: TransportLease;
    try {
      lease = await this.#transport.acquireFor(
        {
          publish: [args.subject],
          subscribe: [`${this.#inboxPrefix}.>`],
        },
        {
          ...(args.signal ? { signal: args.signal } : {}),
          deadlineMs: args.deadlineMs,
        },
      );
    } catch (cause) {
      return err(
        mapTransportAcquireFailure({
          method: args.method,
          subject: args.subject,
          callerCapabilities: args.callerCapabilities,
          cause,
        }),
      );
    }
    return ok(lease);
  }

  async #requestMessage(args: {
    method?: string;
    /** Bounded registered-route token for attempt accounting. */
    route?: string;
    subject: string;
    payload: string;
    permission: DescriptorPermissionAtom;
    timeout: number;
    signal?: AbortSignal;
    callerCapabilities?: readonly string[];
    span?: Span;
    expectedDeploymentId?: string;
  }): Promise<Result<Msg, TransportError>> {
    const recordAttempt = (outcome: string): void => {
      if (args.route !== undefined) recordRpcAttempt(args.route, outcome);
    };
    {
      if (args.signal?.aborted) {
        return err(
          classifyRequestTransportFailure({
            method: args.method,
            subject: args.subject,
            callerCapabilities: args.callerCapabilities,
            cause:
              args.signal.reason ??
              new DOMException("Request aborted", "AbortError"),
          }),
        );
      }
      // Acquire and publish once. A potentially executed request is never replayed.
      const deadlineMs = Date.now() + args.timeout;
      const acquired = await this.#acquireRequestTransport({
        method: args.method,
        subject: args.subject,
        deadlineMs,
        signal: args.signal,
        callerCapabilities: args.callerCapabilities,
      });
      const acquiredTransport = acquired.take();
      if (isErr(acquiredTransport)) {
        recordAttempt(
          acquiredTransport.error.code === "trellis.request.cancelled"
            ? "cancelled"
            : acquiredTransport.error.code === "trellis.request.timeout"
              ? "timeout"
              : "unavailable",
        );
        return err(acquiredTransport.error);
      }
      const { nc, release } = acquiredTransport;
      try {
        // Create the exact reply inbox before signing so the proof binds the
        // reply subject the response arrives on.
        const reply = createInbox(this.#inboxPrefix);
        const authHeaders = await this.createRequestProof(
          args.subject,
          args.payload,
          reply,
          args.permission,
        );
        const headers = natsHeaders();
        headers.set("authorization-context", authHeaders.contextDigest);
        headers.set("session-key", this.#auth.sessionKey);
        headers.set("proof", authHeaders.proof);
        headers.set("iat", String(authHeaders.iat));
        headers.set("request-id", authHeaders.requestId);
        headers.set(
          "Trellis-Session-Request",
          JSON.stringify(authHeaders.request),
        );
        headers.set(
          "Trellis-Api-Generation",
          authHeaders.request.apiGeneration,
        );
        headers.set(
          "Trellis-Accepted-Revision",
          authHeaders.request.acceptedRevision,
        );
        injectTraceContext(createNatsHeaderCarrier(headers), args.span);

        let waitFailure:
          | {
              cause: unknown;
              kind: "cancelled" | "timeout";
            }
          | undefined;
        const result = await AsyncResult.try(async () => {
          const response = Promise.withResolvers<Msg>();
          const owner = this.#auth.authorityOwner;
          if (!owner) throw new Error("session authority owner unavailable");
          const pending: PendingSessionRequest = {
            request: authHeaders.request,
            proofDigest: authHeaders.requestProofDigest,
            action: toCatalogAction(args.permission),
            expectedDeploymentId: args.expectedDeploymentId ?? null,
          };
          let verified = false;
          let verifying = 0;
          const timer = setTimeout(
            () => {
              waitFailure = {
                cause: new DOMException("Request timed out", "TimeoutError"),
                kind: "timeout",
              };
              response.reject(waitFailure.cause);
            },
            Math.max(1, deadlineMs - Date.now()),
          );
          const abort = () => {
            waitFailure = {
              cause:
                args.signal?.reason ??
                new DOMException("Request aborted", "AbortError"),
              kind: "cancelled",
            };
            response.reject(waitFailure.cause);
          };
          args.signal?.addEventListener("abort", abort, { once: true });
          const subscription = nc.subscribe(reply, {
            callback: (error, message) => {
              if (error) response.reject(error);
              else if (
                message.data.length === 0 &&
                message.headers?.code === 503
              ) {
                response.reject(new Error("no responders"));
              } else if (!verified && verifying < 8) {
                // Unauthenticated traffic cannot consume the pending opening.
                verifying++;
                void (async () => {
                  const encoded = message.headers?.values(
                    "Trellis-Session-Response",
                  );
                  const proofs = message.headers?.values("proof");
                  if (
                    encoded?.length !== 1 ||
                    proofs?.length !== 1 ||
                    encoded[0].length > 16 * 1024
                  )
                    return;
                  const signed = JSON.parse(encoded[0]) as SessionResponse;
                  await owner.verifyResponse(
                    signed,
                    message.data,
                    proofs[0],
                    message.subject,
                    pending,
                  );
                  if (
                    signed.kind !==
                    (message.headers?.get("status") === "error"
                      ? "error"
                      : "success")
                  )
                    return;
                  if (
                    !verified &&
                    Date.now() < deadlineMs &&
                    !args.signal?.aborted
                  ) {
                    verified = true;
                    response.resolve(message);
                  }
                })()
                  .catch(() => undefined)
                  .finally(() => {
                    verifying--;
                  });
              }
            },
          });
          // NATS noMux requests abandon their promise when connection closure
          // cancels the subscription timer. Bind settlement to this subscription.
          subscription.closed.then((error) => {
            response.reject(
              error ?? new Error("connection closed before RPC response"),
            );
          });
          // Publish denials arrive on this generation's own status stream, not
          // the reply inbox. Watch the leased connection, never a mutable
          // logical default.
          const stopWatching = acquiredTransport.watchStatus((event) => {
            const error = (event as { error?: unknown } | null)?.error;
            if (
              error instanceof Error &&
              ((error.name === "PermissionViolationError" &&
                Reflect.get(error, "operation") === "publish" &&
                Reflect.get(error, "subject") === args.subject) ||
                error.name === "AuthorizationError" ||
                error.name === "UserAuthenticationExpiredError")
            )
              response.reject(error);
          });
          try {
            if (args.signal?.aborted) {
              abort();
            } else {
              nc.publish(args.subject, args.payload, { headers, reply });
            }
            return await response.promise;
          } finally {
            verified = true;
            clearTimeout(timer);
            args.signal?.removeEventListener("abort", abort);
            stopWatching();
            subscription.unsubscribe();
          }
        });

        if (result.isOk()) {
          recordAttempt("ok");
          return ok(result.take() as Msg);
        }

        const cause = result.error.cause;
        if (waitFailure && cause === waitFailure.cause) {
          recordAttempt(waitFailure.kind);
          return err(
            requestFailedTransportError({
              code: `trellis.request.${waitFailure.kind}`,
              message:
                waitFailure.kind === "cancelled"
                  ? "The request wait was cancelled."
                  : "The request deadline elapsed.",
              hint: "Stopping the wait does not undo work already accepted by the service.",
              cause,
              method: args.method,
              subject: args.subject,
            }),
          );
        }
        const message = cause instanceof Error ? cause.message : String(cause);

        this.#log.warn(
          { method: args.method, subject: args.subject, error: message },
          "NATS request failed",
        );
        recordAttempt(
          args.signal?.aborted
            ? "cancelled"
            : /timeout/i.test(message)
              ? "timeout"
              : "unavailable",
        );
        return err(
          classifyRequestTransportFailure({
            method: args.method,
            subject: args.subject,
            callerCapabilities: args.callerCapabilities,
            cause,
          }),
        );
      } finally {
        release();
      }
    }
  }

  #requestJson(
    subject: string,
    body: JsonValue,
    route: string,
    permission: DescriptorPermissionAtom,
    expectedDeploymentId?: string,
  ): AsyncResult<JsonValue, TransportError | UnexpectedError> {
    return AsyncResult.from(
      (async () => {
        const span = startClientSpan(route);
        return await withSpanAsync(span, async () => {
          try {
            const payload = JSON.stringify(body);
            const response = (
              await this.#requestMessage({
                route,
                subject,
                payload,
                permission,
                timeout: this.timeout,
                span,
                expectedDeploymentId,
              })
            ).take();
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
                hint: "Retry the request. If it keeps happening, reconnect to Trellis and try again.",
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

            if (response.headers?.get("status") === "error") {
              const errorData = parse(TrellisErrorDataSchema, json).take();
              if (isErr(errorData)) {
                return err(
                  createTransportError({
                    code: "trellis.request.invalid_response",
                    message:
                      "Trellis returned an invalid operation error response.",
                    hint: "Inspect the provider's operation error response.",
                    cause: errorData.error,
                    context: { subject },
                  }),
                );
              }
              return err(
                createTransportError({
                  code: "trellis.operation.remote_error",
                  message: errorData.message,
                  hint: "Inspect the provider error and its context.",
                  context: {
                    subject,
                    remoteErrorType: errorData.type,
                    remoteContext: errorData.context,
                  },
                }),
              );
            }

            span.setStatus({ code: SpanStatusCode.OK });
            return ok(json);
          } catch (cause) {
            const error = new UnexpectedError({ cause });
            span.setStatus({
              code: SpanStatusCode.ERROR,
              message: "operation transport failure",
            });
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
      })(),
    );
  }

  #watchJson<T>(
    subject: string,
    body: JsonValue,
    decodeEvent: (value: unknown) => T | undefined,
    observePermission: DescriptorPermissionAtom | undefined,
    expectedDeploymentId: string | undefined,
    operationSubject: string,
  ): AsyncResult<LiveSubscription<T>, TransportError | UnexpectedError> {
    return AsyncResult.from(
      (async () => {
        const record =
          body && typeof body === "object" && !Array.isArray(body)
            ? (body as { operationId?: unknown; includeUpdates?: unknown })
            : undefined;
        if (typeof record?.operationId !== "string") {
          const error = createTransportError({
            code: "trellis.watch.failed",
            message: "Operation watch requires a durable operation id.",
            hint: "Retry watching the operation with a valid operation id.",
            context: { subject },
          });
          recordRuntimeError(error, {
            surface: "operation",
            direction: "client",
            operation: "watchJson",
            phase: "request_encoding",
          });
          return err(error);
        }
        if (!subject.endsWith(".control")) {
          const error = createTransportError({
            code: "trellis.watch.failed",
            message: "Operation watch must open on the control route.",
            hint: "Use the operation control subject for watch.",
            context: { subject },
          });
          recordRuntimeError(error, {
            surface: "operation",
            direction: "client",
            operation: "watchJson",
            phase: "request_encoding",
          });
          return err(error);
        }
        const cache = this.#auth.authorityOwner;
        if (!cache || !observePermission) {
          const error = createTransportError({
            code: "trellis.watch.failed",
            message: "The operation watch authority is unavailable.",
            hint: "Reconnect to Trellis before watching the operation.",
            context: { subject },
          });
          recordRuntimeError(error, {
            surface: "operation",
            direction: "client",
            operation: "watchJson",
            phase: "request_encoding",
          });
          return err(error);
        }
        let transport: { nc: NatsConnection; release(): void } | undefined;
        try {
          const acquired = (
            await this.#acquireRequestTransport({
              subject,
              deadlineMs: Date.now() + this.timeout,
            })
          ).take();
          if (isErr(acquired)) {
            recordRuntimeError(acquired.error, {
              surface: "operation",
              direction: "client",
              operation: "watchJson",
              phase: "handshake",
            });
            return acquired;
          }
          transport = acquired;
          const subscription = await openLiveOperationWatch(
            {
              nats: transport.nc,
              inboxPrefix: this.#inboxPrefix,
              timeoutMs: this.timeout,
              sessionKey: this.#auth.sessionKey,
              live: this.connection.live,
              createRequestProof: (s, p, r) =>
                this.createRequestProof(s, p, r, observePermission),
              authority: {
                cache,
                apiId: `${observePermission.apiId}@${observePermission.apiVersion}`,
                action: toCatalogAction(observePermission),
                localContextDigest: this.#contextDigest(),
                expectedDeploymentId,
              },
              decodeEvent,
            },
            operationSubject,
            subject,
            {
              operationId: record.operationId,
              includeUpdates: record.includeUpdates === true,
            },
          );
          // Own the watch like a standalone live open, but fence only a clean
          // local close: an involuntary transport error keeps its loss outcome.
          const closeOnNats = (terminal: void | Error) => {
            if (!(terminal instanceof Error)) subscription.close();
            this.#liveClosers.delete(closeOnNats);
          };
          this.#liveClosers.add(closeOnNats);
          void subscription.closed.then(() => {
            transport?.release();
            this.#liveClosers.delete(closeOnNats);
          });
          return ok(subscription);
        } catch (cause) {
          transport?.release();
          const error =
            cause instanceof TransportError
              ? cause
              : cause instanceof LiveStreamError
                ? createTransportError({
                    code: cause.codeString(),
                    message: cause.message,
                    hint: "Retry watching the operation. If it keeps failing, check Trellis runtime health.",
                    cause,
                    context: { subject, operationId: record.operationId },
                  })
                : createTransportError({
                    code: "trellis.watch.failed",
                    message: "Trellis could not start the operation watch.",
                    hint: "Retry watching the operation. If it keeps failing, reconnect to Trellis and try again.",
                    cause,
                    context: { subject, operationId: record.operationId },
                  });
          recordRuntimeError(error, {
            surface: "operation",
            direction: "client",
            operation: "watchJson",
            phase: "handshake",
          });
          return err(error);
        }
      })(),
    );
  }
}
