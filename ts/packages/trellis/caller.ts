import {
  AsyncResult,
  type BaseError,
  err,
  ok,
  type Result,
} from "@oatscenter/result";
import type { LiveSubscription } from "./live/subscription.ts";
import type { Codec } from "./generated.ts";
import type { TrellisConnection } from "./connection.ts";
import type { TransportError } from "./errors/index.ts";
import type { TypedKV } from "./kv.ts";
import type { OperationInvoker } from "./operations.ts";
import type { TypedStore } from "./store.ts";
import {
  type GeneratedCaller,
  getCallerRuntime,
  type RuntimeSelectedAction,
} from "./participant_runtime/participant.ts";
import { createActionUnavailableError } from "./session.ts";
import type {
  EventListenerContext,
  EventOpts,
  LiveSubscribeOpts,
  PreparedTrellisEvent,
  RequestOpts,
  RuntimeStateStoresForContract,
  StateFacade,
  Trellis,
} from "./session.ts";
import type {
  ConnectedActionName,
  PascalActionName,
} from "./participant_runtime/surface_names.ts";
import { cursorItems, cursorPages } from "./pagination.ts";

type CodecValue<T> = T extends { decode(value: unknown): infer TValue }
  ? TValue
  : never;
type GeneratedCodec = Readonly<{ decode(value: unknown): unknown }>;
type SelectedActionShape = {
  kind: "rpc" | "operation" | "event" | "live";
  descriptorName: string;
  direction: unknown;
  input?: unknown;
  output?: unknown;
  event?: unknown;
  payload?: unknown;
};
type DescriptorName<T> = T extends `${string}:${infer TName}` ? TName : never;

type OperationDescriptorFor<TAction> = {
  subject: string;
  permissions: {
    invoke: { apiId: string; apiVersion: `v${number}` };
  };
  input: TAction extends { input: infer TInput } ? TInput : undefined;
  output: TAction extends { output: infer TOutput } ? TOutput : undefined;
  progress: TAction extends { progress: infer TProgress }
    ? TProgress
    : undefined;
  update: TAction extends { update: infer TUpdate } ? TUpdate : undefined;
  transfer: TAction extends { upload: true }
    ? { direction: "send" }
    : undefined;
};
type SelectedDescriptor<TSelection> = TSelection extends {
  api: { actions: Readonly<Record<string, unknown>> };
  actions: readonly { descriptorName: string; direction: unknown }[];
}
  ? TSelection["actions"][number] extends infer TSelected
    ? TSelected extends { descriptorName: infer TName extends string }
      ? TName extends keyof TSelection["api"]["actions"]
        ? TSelection["api"]["actions"][TName] & {
            direction: TSelected extends { direction: infer TDirection }
              ? TDirection
              : never;
          }
        : never
      : never
    : never
  : never;
type SelectedAction<TContract extends GeneratedCaller> =
  TContract["uses"][number] extends infer TSelection
    ? TSelection extends {
        api: { identity: string; actions: Readonly<Record<string, unknown>> };
        actions: readonly { descriptorName: string; direction: unknown }[];
      }
      ? SelectedDescriptor<TSelection> extends infer TAction
        ? TAction extends SelectedActionShape
          ? TAction & {
              generatedName: TContract["actionNames"][`${TSelection["api"]["identity"]}:${TAction["descriptorName"]}`];
            }
          : never
        : never
      : never
    : never;

type ActionMethod<TAction extends SelectedActionShape> =
  TAction["kind"] extends "rpc"
    ? TAction["input"] extends GeneratedCodec
      ? TAction["output"] extends GeneratedCodec
        ? TAction extends {
            pagination: "cursor";
          }
          ? CodecValue<TAction["output"]> extends {
              items: readonly (infer TItem)[];
              page: { nextCursor?: string };
            }
            ? ((
                input: CodecValue<TAction["input"]>,
                opts?: RequestOpts,
              ) => AsyncResult<CodecValue<TAction["output"]>, BaseError>) & {
                pages(
                  input?: Omit<CodecValue<TAction["input"]>, "page">,
                  opts?: RequestOpts,
                ): AsyncIterable<
                  Result<CodecValue<TAction["output"]>, BaseError>
                >;
                items(
                  input?: Omit<CodecValue<TAction["input"]>, "page">,
                  opts?: RequestOpts,
                ): AsyncIterable<Result<TItem, BaseError>>;
              }
            : never
          : (
              input: CodecValue<TAction["input"]>,
              opts?: RequestOpts,
            ) => AsyncResult<CodecValue<TAction["output"]>, BaseError>
        : never
      : never
    : TAction["kind"] extends "operation"
      ? TAction["input"] extends GeneratedCodec
        ? ((
            input: CodecValue<TAction["input"]>,
          ) => ReturnType<
            OperationInvoker<OperationDescriptorFor<TAction>>["input"]
          >) & {
            resume: OperationInvoker<OperationDescriptorFor<TAction>>["resume"];
          }
        : never
      : TAction["kind"] extends "live"
        ? TAction["input"] extends GeneratedCodec
          ? TAction["event"] extends GeneratedCodec
            ? (
                input: CodecValue<TAction["input"]>,
                opts?: LiveSubscribeOpts,
              ) => AsyncResult<
                LiveSubscription<CodecValue<TAction["event"]>>,
                BaseError
              >
            : never
          : never
        : TAction["kind"] extends "event"
          ? TAction["payload"] extends GeneratedCodec
            ? TAction["direction"] extends "publish"
              ? ((
                  event: CodecValue<TAction["payload"]>,
                ) => AsyncResult<void, BaseError>) & {
                  prepare(
                    event: CodecValue<TAction["payload"]>,
                  ): ReturnType<Trellis["prepare"]>;
                }
              : (
                  handler: (
                    event: CodecValue<TAction["payload"]>,
                    context: EventListenerContext,
                  ) => unknown | Promise<unknown>,
                  opts?: EventOpts,
                ) => AsyncResult<void, BaseError>
            : never
          : never;

type ActionRecord<TAction> = TAction extends SelectedActionShape & {
  generatedName: string;
}
  ? {
      readonly [
        K in TAction["kind"] extends "event"
          ? TAction["direction"] extends "publish"
            ? `publish${PascalActionName<TAction["generatedName"]>}`
            : `on${PascalActionName<TAction["generatedName"]>}`
          : ConnectedActionName<TAction["generatedName"]>
      ]: ActionMethod<TAction>;
    }
  : never;
type UnionToIntersection<T> = (
  T extends unknown ? (value: T) => void : never
) extends (value: infer TIntersection) => void
  ? TIntersection
  : never;

type CallerResources<T> = T extends { resources: infer R }
  ? R
  : Record<never, never>;
type KvFacadeFor<TContract extends GeneratedCaller> = {
  readonly [
    Name in keyof CallerResources<TContract> as CallerResources<TContract>[Name] extends {
      kind: "kv";
    }
      ? Name
      : never
  ]: CallerResources<TContract>[Name] extends {
    codec: infer TCodec;
  }
    ? TypedKV<CodecValue<TCodec>>
    : never;
};

type StoreFacadeFor<TContract extends GeneratedCaller> = {
  readonly [
    Name in keyof CallerResources<TContract> as CallerResources<TContract>[Name] extends {
      kind: "store";
    }
      ? Name
      : never
  ]: TypedStore;
};

/** Minimum participant contract accepted by the public caller connector. */
export type CallerParticipant = GeneratedCaller;

/** One selected caller action with its generated surface name resolved. */
export type CallerSelectedAction<TContract extends GeneratedCaller> =
  SelectedAction<TContract>;

/**
 * Selected caller action methods, including selected event subscriptions.
 */
export type CallerSelectedActionSurface<TContract extends GeneratedCaller> =
  UnionToIntersection<ActionRecord<CallerSelectedAction<TContract>>>;

/**
 * Selected caller invocation and publish methods, excluding event
 * subscriptions.
 */
export type CallerHandlerActionSurface<TContract extends GeneratedCaller> =
  UnionToIntersection<
    ActionRecord<
      Extract<
        CallerSelectedAction<TContract>,
        | { kind: "rpc" | "operation" | "live" }
        | { kind: "event"; direction: "publish" }
      >
    >
  >;

/**
 * Returns the transport error for a selected action whose optional capability
 * alternatives are denied by current authority. Missing live authority leaves
 * classification to the ordinary bounded transport acquisition.
 */
export function selectedActionAvailabilityError(
  connection: TrellisConnection,
  action: RuntimeSelectedAction,
): TransportError | undefined {
  return action.optionalCapabilities.length > 0 &&
    connection.allowsAction(
      action.api.identity,
      action.descriptor.kind,
      action.descriptor.descriptorName.slice(action.descriptor.kind.length + 1),
      action.direction,
    ) === false
    ? createActionUnavailableError(action.name, action.optionalCapabilities)
    : undefined;
}

/** Flat caller surface inferred from a generated participant's selected actions. */
export type CallerRuntime<TContract extends GeneratedCaller> =
  CallerSelectedActionSurface<TContract> & {
    readonly connection: TrellisConnection;
    availability(): ParticipantAvailability<TContract>;
    watchAvailability(): AsyncIterable<ParticipantAvailability<TContract>>;
    readonly state: StateFacade<RuntimeStateStoresForContract<TContract>>;
    readonly kv: KvFacadeFor<TContract>;
    readonly store: StoreFacadeFor<TContract>;
    publishPrepared(event: PreparedTrellisEvent): AsyncResult<void, BaseError>;
    transfer: Trellis["transfer"];
    wait(): AsyncResult<void, BaseError>;
  };

type OptionalCapability<TContract extends GeneratedCaller> =
  TContract["uses"][number]["optionalCapabilities"][number];
type OptionalResource<TContract extends GeneratedCaller> = {
  [
    Name in keyof CallerResources<TContract>
  ]: CallerResources<TContract>[Name] extends { availability: "optional" }
    ? Name
    : never;
}[keyof CallerResources<TContract>];
type ParticipantAvailability<TContract extends GeneratedCaller> = Readonly<{
  capabilities: Readonly<Record<OptionalCapability<TContract>, boolean>>;
  resources: Readonly<Record<OptionalResource<TContract>, boolean>>;
}>;

/** Projects a private Trellis session into the selected flat caller vocabulary. */
export function createCallerRuntime<TContract extends GeneratedCaller>(
  session: object,
  contract: TContract,
  resources: { kv?: object; store?: object } = {},
): CallerRuntime<TContract> {
  const runtime = session as Trellis;
  const selections = getCallerRuntime(contract).actions;
  if (runtime.ownAuthority)
    runtime.connection.bindAuthorityAvailability(
      runtime.ownAuthority,
      (snapshot, installed) => {
        const capabilities: Record<string, boolean> = {};
        for (const selection of contract.uses) {
          for (const capability of selection.optionalCapabilities) {
            capabilities[capability] =
              snapshot !== undefined &&
              snapshot.capabilities.some(
                (granted) => granted.capabilityId === capability,
              ) &&
              selections
                .filter((action) =>
                  action.optionalCapabilities.includes(capability),
                )
                .every((action) =>
                  snapshot.actions.some(
                    (allowed) =>
                      allowed.apiId === action.api.identity &&
                      allowed.action.kind === action.descriptor.kind &&
                      allowed.action.name ===
                        action.descriptor.descriptorName.slice(
                          action.descriptor.kind.length + 1,
                        ) &&
                      allowed.action.direction === action.direction,
                  ),
                );
          }
        }
        return {
          capabilities,
          resources: Object.fromEntries(
            Object.entries(installed.resources).map(([name, available]) => [
              name,
              snapshot !== undefined && available,
            ]),
          ),
        };
      },
    );
  const caller: Record<string, unknown> = {
    connection: runtime.connection,
    availability: () => runtime.connection.availability(),
    watchAvailability: () => runtime.connection.watchAvailability(),
    state: runtime.state,
    kv: resources.kv ?? {},
    store: resources.store ?? {},
    publishPrepared: runtime.publishPrepared.bind(runtime),
    transfer: runtime.transfer.bind(runtime),
    wait: runtime.wait.bind(runtime),
  };

  for (const action of selections) {
    const unavailable = () =>
      selectedActionAvailabilityError(runtime.connection, action);
    switch (action.descriptor.kind) {
      case "rpc":
        {
          const request: ((
            input: unknown,
            opts?: RequestOpts,
          ) => AsyncResult<unknown, BaseError>) & {
            pages?: (
              input?: unknown,
              opts?: RequestOpts,
            ) => AsyncIterable<unknown>;
            items?: (
              input?: unknown,
              opts?: RequestOpts,
            ) => AsyncIterable<unknown>;
          } = (input: unknown, opts?: RequestOpts) => {
            const error = unavailable();
            return error
              ? AsyncResult.from(Promise.resolve(err(error)))
              : runtime.request(action.name, input, opts);
          };
          if (action.descriptor.pagination === "cursor") {
            request.pages = (input?: unknown, opts?: RequestOpts) =>
              cursorPages(request, input, opts);
            request.items = (input?: unknown, opts?: RequestOpts) =>
              cursorItems(request, input, opts);
          }
          caller[action.connectedName] = request;
        }
        break;
      case "operation":
        {
          const operation = runtime.operationHandle(action.name, unavailable);
          const invoke = (input: unknown) => operation.input(input);
          invoke.resume = operation.resume.bind(operation);
          caller[action.connectedName] = invoke;
        }
        break;
      case "live":
        caller[action.connectedName] = (
          input: unknown,
          opts?: LiveSubscribeOpts,
        ) => {
          const error = unavailable();
          return error
            ? AsyncResult.from(Promise.resolve(err(error)))
            : runtime.liveHandle(action.name).input(input).subscribe(opts);
        };
        break;
      case "event":
        if (action.direction === "publish") {
          const publish = (event: Record<string, unknown>) => {
            const error = unavailable();
            return error
              ? AsyncResult.from(Promise.resolve(err(error)))
              : runtime.publish(action.name, event);
          };
          publish.prepare = (event: Record<string, unknown>) =>
            runtime.prepare(action.name, event);
          caller[action.connectedName] = publish;
        } else {
          caller[action.connectedName] = (
            handler: (
              event: unknown,
              context: EventListenerContext,
            ) => unknown | Promise<unknown>,
            opts?: EventOpts,
          ) => {
            const error = unavailable();
            return error
              ? AsyncResult.from(Promise.resolve(err(error)))
              : runtime.listenEvent(
                  action.name,
                  {},
                  async (event, context) => {
                    await handler(event, context);
                    return ok(undefined);
                  },
                  opts,
                );
          };
        }
        break;
    }
  }

  return caller as CallerRuntime<TContract>;
}
