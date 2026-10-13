import type { BaseError } from "@oatscenter/result";
import type { Codec } from "../generated.ts";
import type {
  EventDesc,
  LiveDesc,
  OperationDesc,
  PermissionAtom,
  RPCDesc,
  RuntimeApi,
  RuntimeRpcErrorDesc,
  SchemaLike,
} from "./api.ts";
import { eventDescriptorIdentity } from "./api.ts";
import { lowerCamelSurfaceName, pascalSurfaceName } from "./surface_names.ts";
import type { TrellisAvailability } from "../connection.ts";
import type { AppRequestDescriptor } from "../generated_support.ts";

/** Source-owned API selections for either a provisioned participant or an OAuth application. */
export type GeneratedCaller = GeneratedParticipant | AppRequestDescriptor;

type GeneratedCodec = Readonly<{
  decode(value: unknown): unknown;
  encode(value: never): unknown;
}>;

export type GeneratedActionDescriptor = Readonly<{
  /** Compiler-owned canonical API/action subject, including event parameters. */
  subject: string;
  kind: "rpc" | "operation" | "event" | "live";
  descriptorName: `${"rpc" | "operation" | "event" | "live"}:${string}`;
  input?: GeneratedCodec;
  output?: GeneratedCodec;
  payload?: GeneratedCodec;
  event?: GeneratedCodec;
  progress?: GeneratedCodec;
  /** Operation's transient-update codec, distinct from durable progress. */
  update?: GeneratedCodec;
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
    /**
     * Optional capabilities that can each independently make this action
     * callable. Empty when any non-optional path covers the action.
     */
    optionalCapabilities: readonly string[];
  }>[];
  optionalCapabilities: readonly string[];
}>;

/** One exact permission atom as emitted by the IDL compiler. */
export type GeneratedPermissionAtom = Readonly<{
  action: string;
  target: Readonly<Record<string, unknown>>;
}>;

export type GeneratedResourceDescriptor = Readonly<Record<string, unknown>> &
  Readonly<{ availability: "required" | "optional" }> &
  (
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
  /** Compiler-owned semantic digest of the participant's authority needs. */
  digest: string;
  path: string;
  implements: readonly GeneratedApiDescriptor[];
  uses: readonly GeneratedActionSelection[];
  /**
   * Exact optional grant bundles keyed by capability or resource identity.
   * Availability derives from the current signed application grant matching
   * these atoms, never from provider-binding presence alone.
   * Native service descriptors omit this OAuth application projection.
   */
  optionalGrants?: Readonly<Record<string, readonly GeneratedPermissionAtom[]>>;
  /** OAuth application authority used to detect a remembered-login upgrade. */
  requiredGrants?: readonly GeneratedPermissionAtom[];
  /** Native capability IDs or OAuth consent-bearing capability selections. */
  requiredCapabilities: readonly (
    string | Readonly<{ id: string; consentDigest: string }>
  )[];
  actionNames: Readonly<Record<string, string>>;
  resources: Readonly<Record<string, GeneratedResourceDescriptor>>;
  companion?: Readonly<{
    request: AppRequestDescriptor;
    availability: "required" | "optional";
  }>;
  packageEvidence: unknown;
}>;

type GeneratedSchema<T> = T extends { decode(value: unknown): infer V }
  ? Codec<V>
  : Extract<T, SchemaLike>;

type GeneratedDescriptorRuntime<T> = T extends {
  kind: "rpc";
  input: infer I;
  output: infer O;
}
  ? RPCDesc<GeneratedSchema<I>, GeneratedSchema<O>>
  : T extends {
        kind: "operation";
        input: infer I;
        output?: infer O;
        progress?: infer P;
      }
    ? Omit<
        OperationDesc<
          GeneratedSchema<I>,
          GeneratedSchema<P>,
          GeneratedSchema<O>
        >,
        "update" | "runtimeErrors"
      > &
        (T extends { update: infer U }
          ? { update: GeneratedSchema<U> }
          : unknown) &
        (T extends { errors: infer E } ? { runtimeErrors: E } : unknown) &
        (T extends { upload: true }
          ? {
              transfer: { direction: "send" };
            }
          : unknown)
    : T extends { kind: "event"; payload: infer E }
      ? EventDesc<GeneratedSchema<E>>
      : T extends { kind: "live"; input: infer I; event: infer E }
        ? LiveDesc<GeneratedSchema<I>, GeneratedSchema<E>>
        : never;

type GeneratedRuntimeEntries<
  TApi,
  TNames extends Readonly<Record<string, string>>,
  TKind extends GeneratedActionDescriptor["kind"],
> = TApi extends {
  identity: string;
  actions: Readonly<Record<string, unknown>>;
}
  ? {
      [K in keyof TApi["actions"]]: TApi["actions"][K] extends infer TAction
        ? TAction extends {
            kind: TKind;
            descriptorName: infer TDescriptorName extends string;
          }
          ? {
              [
                N in TNames[`${TApi["identity"]}:${TDescriptorName}`] & string
              ]: GeneratedDescriptorRuntime<TAction>;
            }
          : never
        : never;
    }[keyof TApi["actions"]]
  : never;

type UnionToIntersection<T> = (
  T extends unknown ? (value: T) => void : never
) extends (value: infer I) => void
  ? I
  : never;

type GeneratedRuntimeFamily<
  TApi,
  TNames extends Readonly<Record<string, string>>,
  TKind extends GeneratedActionDescriptor["kind"],
> = [GeneratedRuntimeEntries<TApi, TNames, TKind>] extends [never]
  ? Record<never, never>
  : UnionToIntersection<GeneratedRuntimeEntries<TApi, TNames, TKind>>;

/** Exact runtime API type projected from canonical generated action descriptors. */
export type RuntimeApiFromGenerated<
  TApi,
  TNames extends Readonly<Record<string, string>>,
> = {
  rpc: GeneratedRuntimeFamily<TApi, TNames, "rpc">;
  operations: GeneratedRuntimeFamily<TApi, TNames, "operation">;
  events: GeneratedRuntimeFamily<TApi, TNames, "event">;
  lives: GeneratedRuntimeFamily<TApi, TNames, "live">;
  subjects: Record<string, unknown>;
};

/** Exact job metadata projected from generated participant resources. */
export type ParticipantJobsFromResources<
  TResources extends GeneratedParticipant["resources"],
> = {
  [
    K in keyof TResources as TResources[K] extends { kind: "job" } ? K : never
  ]: TResources[K] extends { payload: Codec<infer P> }
    ? {
        payload: P;
        result: TResources[K] extends { result: Codec<infer R> }
          ? R
          : undefined;
        update: TResources[K] extends { update: Codec<infer U> } ? U : never;
      }
    : never;
};

/** Exact KV metadata projected from generated participant resources. */
export type ParticipantKvFromResources<
  TResources extends GeneratedParticipant["resources"],
> = {
  [
    K in keyof TResources as TResources[K] extends { kind: "kv" } ? K : never
  ]: TResources[K] extends {
    availability: infer A;
    codec: { decode(value: unknown): unknown };
    version: infer Version;
    migrations: infer Migrations;
  }
    ? {
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

/** Project generated action coordinates; this metadata does not grant authority. */
export function permission(
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

function runtimeDescriptor(
  api: GeneratedApiDescriptor,
  descriptor: GeneratedActionDescriptor,
): RPCDesc | OperationDesc | EventDesc | LiveDesc {
  const transportSubject = descriptor.subject;
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
        update: descriptor.update,
        permissions: {
          invoke: permission(api.identity, descriptor, "invoke"),
          observe: permission(api.identity, descriptor, "observe"),
          cancel: permission(api.identity, descriptor, "cancel"),
          control: Object.fromEntries(
            Object.keys(descriptor.signals ?? {}).map((name) => [
              name,
              permission(api.identity, descriptor, "control"),
            ]),
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
        params: descriptor.parameters?.map(
          (path) => `/${path.join("/")}` as `/${string}`,
        ),
        event: descriptor.payload as Codec<unknown>,
        publishPermission: permission(api.identity, descriptor, "publish"),
        subscribePermission: permission(api.identity, descriptor, "subscribe"),
        publishCapabilities: [],
        subscribeCapabilities: [],
      };
    }
    case "live":
      return {
        subject: transportSubject,
        input: descriptor.input as Codec<unknown>,
        event: descriptor.event as Codec<unknown>,
        permission: permission(api.identity, descriptor, "subscribe"),
        subscribeCapabilities: [],
      };
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
    const lives = target.lives ?? {};
    lives[name] = runtime as LiveDesc;
    target.lives = lives;
  }
}

function generatedActionName(
  participant: GeneratedCaller,
  api: GeneratedApiDescriptor,
  descriptor: GeneratedActionDescriptor,
): string {
  const name =
    participant.actionNames[`${api.identity}:${descriptor.descriptorName}`];
  if (!name) throw new Error("Generated participant action name is missing");
  return name;
}

function emptyApi(): RuntimeApi {
  return { rpc: {}, operations: {}, events: {}, lives: {}, subjects: {} };
}

/** Project a generated API's descriptors without asserting participant implementation or authority. */
export function getApiRuntime(api: GeneratedApiDescriptor): RuntimeApi {
  const runtime = emptyApi();
  for (const descriptor of Object.values(api.actions)) {
    addAction(
      runtime,
      api,
      descriptor,
      `${api.identity}:${descriptor.descriptorName}`,
    );
  }
  return runtime;
}

/** Projects generated descriptors into the participant runtime. */
export function getParticipantRuntime(
  participant: GeneratedParticipant,
): ParticipantRuntime {
  return getCallerRuntime(participant);
}

/** Project callable descriptors and local codecs without inventing participant identity. */
export function getCallerRuntime(
  participant: GeneratedCaller,
): ParticipantRuntime {
  const ownedApi = emptyApi();
  const usedApi = emptyApi();
  const actions: RuntimeSelectedAction[] = [];
  const state: Record<string, RuntimeStateDescriptor> = {};
  const kv: Record<string, Readonly<Record<string, unknown>>> = {};
  const jobs: Record<string, Readonly<Record<string, unknown>>> = {};
  const eventConsumers: Record<string, Readonly<Record<string, unknown>>> = {};

  for (const api of "implements" in participant ? participant.implements : []) {
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
        connectedName:
          descriptor.kind === "event"
            ? `${selected.direction === "publish" ? "publish" : "on"}${pascalSurfaceName(
                name,
              )}`
            : lowerCamelSurfaceName(name),
        optional: selected.optionalCapabilities.length > 0,
        optionalCapabilities: selected.optionalCapabilities,
      });
    }
  }

  if ("states" in participant) {
    for (const [name, stateDescriptor] of Object.entries(participant.states)) {
      state[name] = { kind: "value", value: undefined, ...stateDescriptor };
    }
  }
  for (const [name, resource] of Object.entries(
    "resources" in participant ? participant.resources : {},
  )) {
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
      for (const [api, event] of resource.events as readonly (readonly [
        string,
        string,
      ])[]) {
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
      lives: { ...ownedApi.lives, ...usedApi.lives },
      subjects: {},
    },
    actions,
    state,
    kv,
    jobs,
    eventConsumers,
  };
}

/** Projects authenticated native resource bindings; caller action availability is supplied by the live authority view. */
export function participantAvailability(
  participant: GeneratedParticipant,
  resourceBindings: Readonly<{
    state?: Readonly<Record<string, string>>;
    kv?: Readonly<Record<string, unknown>>;
    store?: Readonly<Record<string, unknown>>;
    jobs?: Readonly<{ queues: Readonly<Record<string, unknown>> }>;
    eventConsumers?: Readonly<Record<string, unknown>>;
  }>,
): TrellisAvailability {
  const capabilities: Record<string, boolean> = {};
  for (const selection of participant.uses) {
    for (const capability of selection.optionalCapabilities) {
      capabilities[capability] = false;
    }
  }

  const resources: Record<string, boolean> = {};
  for (const [name, descriptor] of Object.entries(participant.resources)) {
    if (descriptor.availability !== "optional") continue;
    switch (descriptor.kind) {
      case "state":
        resources[name] = resourceBindings.state?.[name] !== undefined;
        break;
      case "kv":
        resources[name] = resourceBindings.kv?.[name] !== undefined;
        break;
      case "store":
        resources[name] = resourceBindings.store?.[name] !== undefined;
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
  const packageDigest =
    evidence && typeof evidence === "object"
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
