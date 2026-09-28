import {
  AsyncResult,
  type BaseError,
  err,
  type Result,
} from "@oatscenter/result";
import {
  type CallerHandlerActionSurface,
  type CallerRuntime,
  type CallerSelectedAction,
  createCallerRuntime,
  selectedActionAvailabilityError,
} from "./caller.ts";
import type { TrellisConnection } from "./connection.ts";
import {
  type GeneratedParticipant,
  getParticipantRuntime,
} from "./participant_runtime/participant.ts";
import {
  lowerCamelSurfaceName,
  pascalSurfaceName,
} from "./participant_runtime/surface_names.ts";
import type { PascalActionName } from "./participant_runtime/surface_names.ts";
import type {
  EventListenerContext,
  EventOpts,
  PreparedTrellisEvent,
} from "./session.ts";
import type { InferSchemaType, RuntimeApi } from "./participant_runtime/api.ts";
import type {
  ParticipantJobsMetadata,
  ParticipantKvMetadata,
} from "./participant_runtime/metadata.ts";
import type {
  GeneratedServiceParticipant,
  LiveHandler,
  OperationControlRegistration,
  OperationHandler,
  RpcHandler,
  ServiceHandlerClient,
  ServiceJobsFacadeOf,
} from "./service/runtime/service.ts";

export const PROVIDER_CALLER = Symbol("trellis.provider.caller");

export type ProviderCaller = object;

type ProviderBase<TService> = TService extends {
  readonly health: infer THealth;
  readonly connection: infer TConnection;
  readonly name: infer TName;
  readonly createSqlOutbox: infer TCreateSqlOutbox;
  readonly createTransfer: infer TCreateTransfer;
} ? {
    readonly health: THealth;
    readonly connection: TConnection;
    readonly name: TName;
    readonly createSqlOutbox: TCreateSqlOutbox;
    readonly createTransfer: TCreateTransfer;
    wait(): Promise<void>;
    stop(): Promise<void>;
  }
  : {};

type ProviderIdentity<TService> = TService extends {
  readonly connection: infer TConnection;
  readonly name: infer TName;
} ? { readonly connection: TConnection; readonly name: TName }
  : {};

type ServiceContract<TContract extends GeneratedParticipant> = Extract<
  TContract,
  GeneratedServiceParticipant<
    RuntimeApi,
    RuntimeApi | undefined,
    ParticipantJobsMetadata,
    ParticipantKvMetadata
  >
>;
type OwnedApi<TContract extends GeneratedParticipant> = TContract extends {
  readonly __runtimeTypes?: { readonly ownedApi: infer TApi };
} ? Extract<TApi, RuntimeApi>
  : never;

type JobsOfContract<TContract extends GeneratedParticipant> = TContract extends
  {
    readonly __runtimeTypes?: { jobs: infer TJobs };
  } ? Extract<TJobs, ParticipantJobsMetadata>
  : Record<string, never>;

type ProviderEventPublisher<TEvent> =
  & ((event: TEvent) => AsyncResult<void, BaseError>)
  & {
    prepare(
      event: TEvent,
    ): Result<PreparedTrellisEvent, BaseError>;
  };

type ProviderResources<
  TContract extends GeneratedParticipant,
  TService,
> = {
  readonly kv: TService extends { readonly kv: infer TKv } ? TKv : never;
  readonly store: TService extends { readonly store: infer TStore } ? TStore
    : never;
  readonly jobs: ServiceJobsFacadeOf<
    JobsOfContract<TContract>,
    ProviderHandlerClient<TContract, TService>
  >;
};

type ProviderHandlerCommon<
  TContract extends GeneratedParticipant,
  TService,
> =
  & ProviderIdentity<TService>
  & Pick<
    CallerRuntime<TContract>,
    "availability" | "watchAvailability" | "publishPrepared" | "transfer"
  >;

type ProviderOwnedPublish<TContract extends GeneratedParticipant> = {
  readonly [
    K in
      & keyof OwnedApi<TContract>["events"]
      & string as `publish${PascalActionName<K>}`
  ]: ProviderEventPublisher<
    InferSchemaType<OwnedApi<TContract>["events"][K]["event"]>
  >;
};

/** Caller and bound-resource surface available inside provider handlers. */
export type ProviderHandlerClient<
  TContract extends GeneratedParticipant,
  TService,
> =
  & CallerHandlerActionSurface<TContract>
  & ProviderOwnedPublish<TContract>
  & ProviderResources<TContract, TService>
  & ProviderHandlerCommon<TContract, TService>;

type ProviderOwnedRegistrations<TContract extends GeneratedParticipant> =
  & {
    readonly [
      K in
        & keyof OwnedApi<TContract>["rpc"]
        & string as `handle${PascalActionName<K>}`
    ]: (handler: RpcHandler<ServiceContract<TContract>, K>) => Promise<void>;
  }
  & {
    readonly [
      K in
        & keyof OwnedApi<TContract>["operations"]
        & string as `handle${PascalActionName<K>}`
    ]:
      & ((
        handler: OperationHandler<ServiceContract<TContract>, K>,
      ) => Promise<void>)
      & OperationControlRegistration<OwnedApi<TContract>, K>;
  }
  & {
    readonly [
      K in
        & keyof NonNullable<OwnedApi<TContract>["lives"]>
        & string as `handle${PascalActionName<K>}`
    ]: (handler: LiveHandler<ServiceContract<TContract>, K>) => Promise<void>;
  }
  & {
    readonly [
      K in
        & keyof OwnedApi<TContract>["events"]
        & string as `on${PascalActionName<K>}`
    ]: (
      handler: (args: {
        event: InferSchemaType<OwnedApi<TContract>["events"][K]["event"]>;
        context: EventListenerContext;
        client: ServiceHandlerClient<ServiceContract<TContract>>;
      }) => unknown | Promise<unknown>,
      subjectData?: Record<string, unknown>,
      options?: EventOpts,
    ) => AsyncResult<void, BaseError>;
  };

type ProviderSelectedEventSubscriptions<
  TContract extends GeneratedParticipant,
  TService,
> = {
  readonly [
    A in Extract<
      CallerSelectedAction<TContract>,
      { kind: "event"; direction: "subscribe" }
    > as `on${PascalActionName<A["generatedName"]>}`
  ]: (
    handler: (args: {
      event: InferSchemaType<A["payload"]>;
      context: EventListenerContext;
      client: ProviderHandlerClient<TContract, TService>;
    }) => unknown | Promise<unknown>,
    subjectData?: Record<string, unknown>,
    options?: EventOpts,
  ) => AsyncResult<void, BaseError>;
};

/** Connected provider facade for a generated service participant. */
export type ProviderRuntime<
  TContract extends GeneratedParticipant,
  TService,
> =
  & ProviderBase<TService>
  & ProviderHandlerClient<TContract, TService>
  & ProviderOwnedRegistrations<TContract>
  & ProviderSelectedEventSubscriptions<TContract, TService>;

type ProviderQueue = {
  create(payload: unknown): unknown;
  submit(payload: unknown): unknown;
  updates(jobId: string, options?: unknown): unknown;
  handle(
    handler: (args: Record<string, unknown>) => unknown,
    options?: unknown,
  ): unknown;
};

type ProviderService = {
  readonly kv: unknown;
  readonly store: unknown;
  readonly jobs: unknown;
  readonly health: unknown;
  readonly connection: unknown;
  readonly name: unknown;
  readonly createSqlOutbox: (...args: never[]) => unknown;
  readonly createTransfer: (...args: never[]) => unknown;
  readonly handle: Record<
    string,
    Record<
      string,
      Record<
        string,
        & ((handler: (args: Record<string, unknown>) => unknown) => unknown)
        & {
          accept?: (args: unknown) => unknown;
          control?: (operationId: string) => unknown;
          reconcile?: (operationId: string) => unknown;
        }
      >
    >
  >;
  readonly [PROVIDER_CALLER]: ProviderCaller;
  publishPrepared(event: unknown): unknown;
  wait(): Promise<void>;
  stop(): Promise<void>;
};

function surfacePath(name: string): readonly [string, string] {
  const [head, ...tail] = name.split(".");
  return [
    lowerCamelSurfaceName(head!),
    lowerCamelSurfaceName(tail.length === 0 ? name : tail.join(".")),
  ];
}

/** Projects a connected service into its flat provider and caller vocabulary. */
export function createProviderRuntime<
  TContract extends GeneratedParticipant,
  TService extends object,
>(
  connectedService: TService,
  contract: TContract,
): ProviderRuntime<TContract, TService> {
  const service = connectedService as TService & ProviderService;
  const connection = service.connection as TrellisConnection;
  const provider: Record<string, unknown> = {
    kv: service.kv,
    store: service.store,
    health: service.health,
    connection,
    name: service.name,
    createSqlOutbox: service.createSqlOutbox.bind(service),
    createTransfer: service.createTransfer.bind(service),
    publishPrepared: service.publishPrepared.bind(service),
    wait: service.wait.bind(service),
    stop: service.stop.bind(service),
  };
  const queues = service.jobs as Record<string, ProviderQueue>;
  provider.jobs = Object.fromEntries(
    Object.entries(queues).map(([name, queue]) => [name, {
      create: (payload: unknown) => queue.create(payload),
      submit: (payload: unknown) => queue.submit(payload),
      updates: (jobId: string, options?: unknown) =>
        queue.updates(jobId, options),
      handle: (
        handler: (args: Record<string, unknown>) => unknown,
        options?: unknown,
      ) =>
        queue.handle((args) => handler({ ...args, client: provider }), options),
    }]),
  );
  const caller = createCallerRuntime(service[PROVIDER_CALLER], contract) as
    & Record<string, unknown>
    & CallerRuntime<TContract>;
  provider.availability = caller.availability;
  provider.watchAvailability = caller.watchAvailability;
  provider.transfer = caller.transfer;
  for (const action of getParticipantRuntime(contract).actions) {
    const connected = caller[action.connectedName];
    if (
      action.descriptor.kind === "event" && action.direction === "subscribe"
    ) {
      provider[action.connectedName] = (
        handler: (args: Record<string, unknown>) => unknown,
        subjectData?: Record<string, unknown>,
        options?: unknown,
      ) => {
        const unavailable = selectedActionAvailabilityError(connection, action);
        if (unavailable) {
          return AsyncResult.from(Promise.resolve(err(unavailable)));
        }
        return (service[PROVIDER_CALLER] as {
          listenEvent(
            event: string,
            subjectData: Record<string, unknown>,
            handler: (message: unknown, context: unknown) => unknown,
            options?: unknown,
          ): unknown;
        }).listenEvent(
          action.name,
          subjectData ?? {},
          (message, context) =>
            handler({ event: message, context, client: provider }),
          options,
        );
      };
    } else {
      provider[action.connectedName] = connected;
    }
  }

  for (
    const name of Object.keys(getParticipantRuntime(contract).ownedApi.rpc)
  ) {
    const [group, leaf] = surfacePath(name);
    const register = service.handle.rpc![group]![leaf]!;
    provider[`handle${pascalSurfaceName(name)}`] = (
      handler: (args: Record<string, unknown>) => unknown,
    ) => register((args) => handler({ ...args, client: provider }));
  }
  for (
    const name of Object.keys(
      getParticipantRuntime(contract).ownedApi.operations,
    )
  ) {
    const [group, leaf] = surfacePath(name);
    const register = service.handle.operation![group]![leaf]!;
    const expose = (
      handler: (args: Record<string, unknown>) => unknown,
    ) => register((args) => handler({ ...args, client: provider }));
    provider[`handle${pascalSurfaceName(name)}`] = Object.assign(expose, {
      ...(register.accept ? { accept: register.accept.bind(register) } : {}),
      ...(register.control ? { control: register.control.bind(register) } : {}),
      ...(register.reconcile
        ? { reconcile: register.reconcile.bind(register) }
        : {}),
    });
  }
  for (
    const name of Object.keys(
      getParticipantRuntime(contract).ownedApi.lives ?? {},
    )
  ) {
    const [group, leaf] = surfacePath(name);
    const register = service.handle.live![group]![leaf]!;
    provider[`handle${pascalSurfaceName(name)}`] = (
      handler: (args: Record<string, unknown>) => unknown,
    ) => register((args) => handler({ ...args, client: provider }));
  }
  for (
    const name of Object.keys(getParticipantRuntime(contract).ownedApi.events)
  ) {
    provider[`on${pascalSurfaceName(name)}`] = (
      handler: (args: Record<string, unknown>) => unknown,
      subjectData?: Record<string, unknown>,
      options?: unknown,
    ) =>
      (service[PROVIDER_CALLER] as {
        listenEvent(
          event: string,
          subjectData: Record<string, unknown>,
          handler: (message: unknown, context: unknown) => unknown,
          options?: unknown,
        ): unknown;
      }).listenEvent(
        name,
        subjectData ?? {},
        (message, context) =>
          handler({ event: message, context, client: provider }),
        options,
      );
    const publish = Object.assign(
      (event: Record<string, unknown>) =>
        (service[PROVIDER_CALLER] as {
          publish(event: string, data: Record<string, unknown>): unknown;
        }).publish(name, event),
      {
        prepare: (event: Record<string, unknown>) =>
          (service[PROVIDER_CALLER] as {
            prepare(
              event: string,
              data: Record<string, unknown>,
            ): Result<PreparedTrellisEvent, BaseError>;
          }).prepare(name, event),
      },
    );
    provider[`publish${pascalSurfaceName(name)}`] = publish;
  }

  return provider as ProviderRuntime<TContract, TService>;
}
