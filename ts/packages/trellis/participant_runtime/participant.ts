import type { BaseError } from "@oatscenter/result";
import type { Codec } from "../generated.ts";
import type {
  EventDesc,
  FeedDesc,
  OperationDesc,
  PermissionAtom,
  RPCDesc,
  RuntimeApi,
  RuntimeRpcErrorDesc,
  SchemaLike,
} from "./api.ts";
import {
  boundApiSubject,
  eventDescriptorIdentity,
  eventSubject,
} from "./api.ts";
import { lowerCamelSurfaceName, pascalSurfaceName } from "./surface_names.ts";
import type { TrellisAvailability } from "../connection.ts";

type GeneratedCodec = Readonly<{
  decode(value: unknown): unknown;
  encode(value: never): unknown;
}>;

export type GeneratedActionDescriptor = Readonly<{
  kind: "rpc" | "operation" | "event" | "feed";
  descriptorName: `${"rpc" | "operation" | "event" | "feed"}:${string}`;
  input?: GeneratedCodec;
  output?: GeneratedCodec;
  payload?: GeneratedCodec;
  event?: GeneratedCodec;
  progress?: GeneratedCodec;
  errors?: readonly RuntimeRpcErrorClass[];
  signals?: Readonly<Record<string, GeneratedCodec>>;
  parameters?: readonly (readonly string[])[];
  upload?: boolean;
  download?: boolean;
  pagination?: "cursor";
}>;

export type GeneratedApiDescriptor = Readonly<{
  identity: string;
  actions: Readonly<Record<string, GeneratedActionDescriptor>>;
}>;

export type GeneratedActionSelection = Readonly<{
  api: GeneratedApiDescriptor;
  actions: readonly Readonly<{
    descriptorName: string;
    direction: "call" | "invoke" | "publish" | "subscribe";
  }>[];
  optionalCapabilities: readonly string[];
}>;

export type GeneratedResourceDescriptor =
  & Readonly<Record<string, unknown>>
  & Readonly<{ availability: "required" | "optional" }>
  & (
    | Readonly<{
      kind: "state" | "kv";
      codec: GeneratedCodec;
      version: number;
      migrations: Readonly<Record<number, GeneratedCodec>>;
    }>
    | Readonly<{ kind: "store" | "job" | "consumer" }>
  );

/** Generated participant descriptor accepted by Trellis runtimes. */
export type GeneratedParticipant = Readonly<{
  kind: "service" | "device" | "app" | "agent";
  id: string;
  identity: string;
  path: string;
  implements: readonly GeneratedApiDescriptor[];
  uses: readonly GeneratedActionSelection[];
  actionNames: Readonly<Record<string, string>>;
  resources: Readonly<Record<string, GeneratedResourceDescriptor>>;
  companion?: Readonly<{
    participant: GeneratedParticipant;
    availability: "required" | "optional";
  }>;
  packageEvidence: unknown;
}>;

type GeneratedSchema<T> = T extends { decode(value: unknown): infer V }
  ? Codec<V>
  : Extract<T, SchemaLike>;

type GeneratedDescriptorRuntime<T> = T extends
  { kind: "rpc"; input: infer I; output: infer O }
  ? RPCDesc<GeneratedSchema<I>, GeneratedSchema<O>>
  : T extends {
    kind: "operation";
    input: infer I;
    output?: infer O;
    progress?: infer P;
  } ?
      & OperationDesc<
        GeneratedSchema<I>,
        GeneratedSchema<P>,
        GeneratedSchema<O>
      >
      & (T extends { upload: true } ? {
          transfer: { direction: "send" };
        }
        : {})
  : T extends { kind: "event"; payload: infer E }
    ? EventDesc<GeneratedSchema<E>>
  : T extends { kind: "feed"; input: infer I; event: infer E }
    ? FeedDesc<GeneratedSchema<I>, GeneratedSchema<E>>
  : never;

type GeneratedRuntimeEntries<
  TApi,
  TNames extends Readonly<Record<string, string>>,
  TKind extends GeneratedActionDescriptor["kind"],
> = TApi extends {
  identity: string;
  actions: Readonly<Record<string, unknown>>;
} ? {
    [K in keyof TApi["actions"]]: TApi["actions"][K] extends infer TAction
      ? TAction extends {
        kind: TKind;
        descriptorName: infer TDescriptorName extends string;
      } ? {
          [
            N in
              & TNames[`${TApi["identity"]}:${TDescriptorName}`]
              & string
          ]: GeneratedDescriptorRuntime<TAction>;
        }
      : never
      : never;
  }[keyof TApi["actions"]]
  : never;

type UnionToIntersection<T> = (
  T extends unknown ? (value: T) => void : never
) extends (value: infer I) => void ? I : never;

type GeneratedRuntimeFamily<
  TApi,
  TNames extends Readonly<Record<string, string>>,
  TKind extends GeneratedActionDescriptor["kind"],
> = [GeneratedRuntimeEntries<TApi, TNames, TKind>] extends [never] ? {}
  : UnionToIntersection<GeneratedRuntimeEntries<TApi, TNames, TKind>>;

/** Exact runtime API type projected from canonical generated action descriptors. */
export type RuntimeApiFromGenerated<
  TApi,
  TNames extends Readonly<Record<string, string>>,
> = {
  rpc: GeneratedRuntimeFamily<TApi, TNames, "rpc">;
  operations: GeneratedRuntimeFamily<TApi, TNames, "operation">;
  events: GeneratedRuntimeFamily<TApi, TNames, "event">;
  feeds: GeneratedRuntimeFamily<TApi, TNames, "feed">;
  subjects: Record<string, unknown>;
};

/** Exact job metadata projected from generated participant resources. */
export type ParticipantJobsFromResources<
  TResources extends GeneratedParticipant["resources"],
> = {
  [
    K in keyof TResources as TResources[K] extends { kind: "job" } ? K
      : never
  ]: TResources[K] extends {
    payload: Codec<infer P>;
    result?: Codec<infer R>;
  } ? { payload: P; result: R }
    : never;
};

/** Exact KV metadata projected from generated participant resources. */
export type ParticipantKvFromResources<
  TResources extends GeneratedParticipant["resources"],
> = {
  [
    K in keyof TResources as TResources[K] extends { kind: "kv" } ? K
      : never
  ]: TResources[K] extends {
    availability: infer A;
    codec: { decode(value: unknown): unknown };
    version: infer Version;
    migrations: infer Migrations;
  } ? {
      required: A extends "required" ? true : false;
      value: ReturnType<TResources[K]["codec"]["decode"]>;
      schema: {
        codec: TResources[K]["codec"];
        version: Version;
        migrations: Migrations;
      };
    }
    : never;
};

type RuntimeRpcErrorClass = Readonly<{
  type: string;
  fromSerializable(data: unknown): BaseError;
}>;

export type RuntimeSelectedAction = Readonly<{
  api: GeneratedApiDescriptor;
  descriptor: GeneratedActionDescriptor;
  direction: "call" | "invoke" | "publish" | "subscribe";
  name: string;
  connectedName: string;
  optional: boolean;
  optionalCapabilities: readonly string[];
}>;

type RuntimeStateDescriptor = Readonly<{
  kind: "value";
  value: unknown;
  codec: Codec<unknown>;
  version: number;
  migrations: Readonly<Record<number, Codec<unknown>>>;
}>;

export type ParticipantRuntime = Readonly<{
  ownedApi: RuntimeApi;
  usedApi: RuntimeApi;
  api: RuntimeApi;
  actions: readonly RuntimeSelectedAction[];
  state: Readonly<Record<string, RuntimeStateDescriptor>>;
  kv: Readonly<Record<string, Readonly<Record<string, unknown>>>>;
  jobs: Readonly<Record<string, Readonly<Record<string, unknown>>>>;
  eventConsumers: Readonly<Record<string, Readonly<Record<string, unknown>>>>;
}>;

function actionName(descriptorName: string): string {
  return descriptorName.slice(descriptorName.indexOf(":") + 1);
}

function permission(
  apiId: string,
  descriptor: GeneratedActionDescriptor,
  action: PermissionAtom["action"],
): PermissionAtom {
  const match = /^(.+)@v([1-9][0-9]*)$/.exec(apiId);
  if (!match) throw new Error(`Invalid generated API identity '${apiId}'`);
  return {
    apiId: match[1],
    apiVersion: `v${match[2]}` as `v${number}`,
    surfaceKind: descriptor.kind,
    surfaceName: actionName(descriptor.descriptorName),
    action,
  };
}

function runtimeErrors(
  errors: readonly RuntimeRpcErrorClass[] | undefined,
): readonly RuntimeRpcErrorDesc[] | undefined {
  return errors?.map((error) => ({
    type: error.type,
    fromSerializable: error.fromSerializable,
  }));
}

function subject(
  api: GeneratedApiDescriptor,
  descriptor: GeneratedActionDescriptor,
): string {
  const name = actionName(descriptor.descriptorName);
  const prefix = descriptor.kind === "event"
    ? "events"
    : descriptor.kind === "operation"
    ? "operations"
    : descriptor.kind;
  const parameters =
    descriptor.parameters?.map((path) => `.{/${path.join("/")}}`).join("") ??
      "";
  if (descriptor.kind === "event") {
    return `${eventSubject(api.identity, name)}${parameters}`;
  }
  const apiName = api.identity.split(".").at(-1)?.split("@v")[0];
  if (!apiName) {
    throw new Error(`Invalid generated API identity '${api.identity}'`);
  }
  return `${prefix}.v1.${apiName}.${name}`;
}

function runtimeDescriptor(
  api: GeneratedApiDescriptor,
  descriptor: GeneratedActionDescriptor,
): RPCDesc | OperationDesc | EventDesc | FeedDesc {
  const transportSubject = subject(api, descriptor);
  const errors = descriptor.errors?.map((error) => error.type);
  const declaredErrors = runtimeErrors(descriptor.errors);
  switch (descriptor.kind) {
    case "rpc":
      return {
        subject: transportSubject,
        input: descriptor.input as Codec<unknown>,
        output: descriptor.output as Codec<unknown>,
        permission: permission(api.identity, descriptor, "call"),
        callerCapabilities: [],
        ...(descriptor.download ? { transfer: { direction: "receive" } } : {}),
        ...(errors ? { errors, declaredErrorTypes: errors } : {}),
        ...(declaredErrors ? { runtimeErrors: declaredErrors } : {}),
      };
    case "operation":
      return {
        subject: transportSubject,
        input: descriptor.input as Codec<unknown>,
        output: descriptor.output,
        progress: descriptor.progress,
        update: descriptor.progress,
        permissions: {
          invoke: permission(api.identity, descriptor, "invoke"),
          observe: permission(api.identity, descriptor, "observe"),
          cancel: permission(api.identity, descriptor, "cancel"),
          control: Object.fromEntries(
            Object.keys(descriptor.signals ?? {}).map((name) => {
              const control = permission(api.identity, descriptor, "control");
              return [
                name,
                {
                  ...control,
                  surfaceName: `${control.surfaceName}.${name}`,
                },
              ];
            }),
          ),
        },
        signals: Object.fromEntries(
          Object.entries(descriptor.signals ?? {}).map(([name, input]) => [
            name,
            { input },
          ]),
        ),
        callerCapabilities: [],
        observeCapabilities: [],
        cancelCapabilities: [],
        controlCapabilities: [],
        ...(descriptor.upload ? { transfer: { direction: "send" } } : {}),
        ...(errors ? { errors, declaredErrorTypes: errors } : {}),
        ...(declaredErrors ? { runtimeErrors: declaredErrors } : {}),
      };
    case "event": {
      const eventName = actionName(descriptor.descriptorName);
      return {
        subject: transportSubject,
        descriptorIdentity: eventDescriptorIdentity(
          api.identity,
          eventName,
          descriptor.parameters?.length ?? 0,
        ),
        params: descriptor.parameters?.map((path) =>
          `/${path.join("/")}` as `/${string}`
        ),
        event: descriptor.payload as Codec<unknown>,
        publishPermission: permission(api.identity, descriptor, "publish"),
        subscribePermission: permission(api.identity, descriptor, "subscribe"),
        publishCapabilities: [],
        subscribeCapabilities: [],
      };
    }
    case "feed":
      return {
        subject: transportSubject,
        input: descriptor.input as Codec<unknown>,
        event: descriptor.event as Codec<unknown>,
        permission: permission(api.identity, descriptor, "subscribe"),
        subscribeCapabilities: [],
      };
  }
}

/** Applies bootstrap-selected provider deployments to request route subjects. */
export function bindApiRoutes(
  api: RuntimeApi,
  apiBindings: Readonly<Record<string, unknown>>,
): RuntimeApi {
  const bind = <T extends RPCDesc | OperationDesc | FeedDesc>(
    descriptor: T,
    family: "rpc" | "operation" | "feed",
  ): T => {
    const permission = "permissions" in descriptor
      ? descriptor.permissions.invoke
      : descriptor.permission;
    const apiId = `${permission.apiId}@${permission.apiVersion}`;
    const binding = apiBindings[apiId];
    if (
      !binding || typeof binding !== "object" ||
      typeof Reflect.get(binding, "providerDeploymentId") !== "string"
    ) {
      throw new Error(
        `Bootstrap did not bind API '${apiId}' to a provider deployment`,
      );
    }
    return {
      ...descriptor,
      subject: boundApiSubject(
        family,
        apiId,
        Reflect.get(binding, "providerDeploymentId"),
        permission.surfaceName,
      ),
    };
  };
  return {
    ...api,
    rpc: Object.fromEntries(
      Object.entries(api.rpc).map(([name, descriptor]) => [
        name,
        bind(descriptor, "rpc"),
      ]),
    ),
    operations: Object.fromEntries(
      Object.entries(api.operations).map(([name, descriptor]) => [
        name,
        bind(descriptor, "operation"),
      ]),
    ),
    feeds: Object.fromEntries(
      Object.entries(api.feeds ?? {}).map(([name, descriptor]) => [
        name,
        bind(descriptor, "feed"),
      ]),
    ),
  };
}

/** Replaces request subjects after an authorization binding refresh. */
export function refreshApiRoutes(
  api: RuntimeApi,
  apiBindings: Readonly<Record<string, unknown>>,
): void {
  const next = bindApiRoutes(api, apiBindings);
  for (const family of ["rpc", "operations", "feeds"] as const) {
    for (const [name, descriptor] of Object.entries(next[family] ?? {})) {
      const current = api[family]?.[name];
      if (current) current.subject = descriptor.subject;
    }
  }
}

function addAction(
  target: RuntimeApi,
  api: GeneratedApiDescriptor,
  descriptor: GeneratedActionDescriptor,
  name: string,
): void {
  const runtime = runtimeDescriptor(api, descriptor);
  if (descriptor.kind === "rpc") target.rpc[name] = runtime as RPCDesc;
  else if (descriptor.kind === "operation") {
    target.operations[name] = runtime as OperationDesc;
  } else if (descriptor.kind === "event") {
    target.events[name] = runtime as EventDesc;
  } else {
    const feeds = target.feeds ?? {};
    feeds[name] = runtime as FeedDesc;
    target.feeds = feeds;
  }
}

function generatedActionName(
  participant: GeneratedParticipant,
  api: GeneratedApiDescriptor,
  descriptor: GeneratedActionDescriptor,
): string {
  const name = participant.actionNames[
    `${api.identity}:${descriptor.descriptorName}`
  ];
  if (!name) throw new Error("Generated participant action name is missing");
  return name;
}

function emptyApi(): RuntimeApi {
  return { rpc: {}, operations: {}, events: {}, feeds: {}, subjects: {} };
}

/** Projects generated descriptors into the participant runtime. */
export function getParticipantRuntime(
  participant: GeneratedParticipant,
): ParticipantRuntime {
  const ownedApi = emptyApi();
  const usedApi = emptyApi();
  const actions: RuntimeSelectedAction[] = [];
  const state: Record<string, RuntimeStateDescriptor> = {};
  const kv: Record<string, Readonly<Record<string, unknown>>> = {};
  const jobs: Record<string, Readonly<Record<string, unknown>>> = {};
  const eventConsumers: Record<string, Readonly<Record<string, unknown>>> = {};

  for (const api of participant.implements) {
    for (const descriptor of Object.values(api.actions)) {
      addAction(
        ownedApi,
        api,
        descriptor,
        generatedActionName(participant, api, descriptor),
      );
    }
  }
  for (const selection of participant.uses) {
    for (const selected of selection.actions) {
      const descriptor = selection.api.actions[selected.descriptorName];
      if (!descriptor) {
        throw new Error(
          `Generated action '${selected.descriptorName}' is absent from '${selection.api.identity}'`,
        );
      }
      const name = generatedActionName(participant, selection.api, descriptor);
      addAction(usedApi, selection.api, descriptor, name);
      actions.push({
        api: selection.api,
        descriptor,
        direction: selected.direction,
        name,
        connectedName: descriptor.kind === "event"
          ? `${selected.direction === "publish" ? "publish" : "on"}${
            pascalSurfaceName(name)
          }`
          : lowerCamelSurfaceName(name),
        optional: selection.optionalCapabilities.length > 0,
        optionalCapabilities: selection.optionalCapabilities,
      });
    }
  }

  for (const [name, resource] of Object.entries(participant.resources)) {
    if (resource.kind === "state") {
      state[name] = {
        kind: "value",
        value: undefined,
        codec: resource.codec,
        version: resource.version,
        migrations: resource.migrations,
      };
    } else if (resource.kind === "kv") {
      kv[name] = {
        required: resource.availability === "required",
        schema: resource,
      };
    } else if (resource.kind === "job") {
      jobs[name] = {
        payload: resource.payload,
        update: resource.update,
        updateSchema: resource.update,
        result: resource.result,
      };
    } else if (resource.kind === "consumer") {
      const uses: Record<string, string[]> = {};
      for (
        const [api, event] of resource.events as readonly (
          readonly [string, string]
        )[]
      ) {
        const events = uses[api] ?? [];
        events.push(event);
        uses[api] = events;
      }
      eventConsumers[name] = {
        uses,
        replay: resource.replay,
        concurrency: resource.concurrency,
      };
    }
  }

  return {
    ownedApi,
    usedApi,
    api: {
      rpc: { ...ownedApi.rpc, ...usedApi.rpc },
      operations: { ...ownedApi.operations, ...usedApi.operations },
      events: { ...ownedApi.events, ...usedApi.events },
      feeds: { ...ownedApi.feeds, ...usedApi.feeds },
      subjects: {},
    },
    actions,
    state,
    kv,
    jobs,
    eventConsumers,
  };
}

/** Projects installed API and resource bindings into one immutable participant snapshot. */
export function participantAvailability(
  participant: GeneratedParticipant,
  apiBindings: Readonly<Record<string, unknown>>,
  resourceBindings: Readonly<{
    kv?: Readonly<Record<string, unknown>>;
    store?: Readonly<Record<string, unknown>>;
    jobs?: Readonly<{ queues: Readonly<Record<string, unknown>> }>;
    eventConsumers?: Readonly<Record<string, unknown>>;
  }>,
  permissions?: readonly Readonly<{
    target: Readonly<{
      kind: string;
      participant?: string;
      resource?: string;
      name?: string;
    }>;
  }>[],
): TrellisAvailability {
  const capabilities: Record<string, boolean> = {};
  for (const selection of participant.uses) {
    const available = selection.api.identity in apiBindings;
    for (const capability of selection.optionalCapabilities) {
      capabilities[capability] = available;
    }
  }

  const resources: Record<string, boolean> = {};
  const granted = (name: string, kind: string) =>
    permissions?.some((permission) =>
      permission.target.kind === "participantResource" &&
      permission.target.participant === participant.identity &&
      permission.target.resource === kind &&
      permission.target.name === name
    ) ?? true;
  for (const [name, descriptor] of Object.entries(participant.resources)) {
    if (descriptor.availability !== "optional") continue;
    switch (descriptor.kind) {
      case "state":
        resources[name] = granted(name, "state");
        break;
      case "kv":
        resources[name] = resourceBindings.kv?.[name] !== undefined &&
          granted(name, "kv");
        break;
      case "store":
        resources[name] = resourceBindings.store?.[name] !== undefined &&
          granted(name, "store");
        break;
      case "job":
        resources[name] = resourceBindings.jobs?.queues[name] !== undefined;
        break;
      case "consumer":
        resources[name] = resourceBindings.eventConsumers?.[name] !== undefined;
        break;
    }
  }

  return Object.freeze({
    capabilities: Object.freeze(capabilities),
    resources: Object.freeze(resources),
  });
}

/** Returns the proof-bound package evidence envelope without interpreting source. */
export function participantEvidence(participant: GeneratedParticipant): {
  packageEvidence: unknown;
  participantPath: string;
  packageDigest: string;
} {
  const evidence = participant.packageEvidence;
  const packageDigest = evidence && typeof evidence === "object"
    ? Reflect.get(evidence, "rootDigest")
    : undefined;
  if (typeof packageDigest !== "string" || packageDigest.length === 0) {
    throw new Error("Generated participant has invalid package evidence");
  }
  return {
    packageEvidence: evidence,
    participantPath: participant.path,
    packageDigest,
  };
}
