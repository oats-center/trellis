import {
  type Authenticator,
  type NatsConnection,
  wsconnect,
} from "@nats-io/nats-core";
import {
  AsyncResult,
  BaseError,
  Result,
  UnexpectedError,
} from "@oatscenter/result";
import { type StaticDecode, Type } from "typebox";
import { Value } from "typebox/value";
import { ulid } from "ulid";

import {
  AuthorizationContextBundleSchema,
  AuthorizationContextCache,
  AuthorizationContextRefreshError,
  AuthorizationProviderCache,
  refreshAuthorizationContextWithMetadata,
  startAuthorizationContextRefresh,
} from "./auth/authorization_context.ts";
import {
  base64urlDecode,
  base64urlEncode,
  BrowserSessionStore,
  toArrayBuffer,
} from "./auth/browser.ts";
import {
  browserInstallationScope,
  type BrowserSessionCredential,
} from "./auth/browser/storage.ts";
import { decodeTrellisHttpError } from "./auth/http_error.ts";
import {
  importEd25519PrivateKeyFromSeedBase64url,
  publicKeyBase64urlFromSeed,
} from "./auth/keys.ts";
import { createAuth, type TrellisAuth } from "./auth/session_auth.ts";
import { estimateMidpointClockOffsetMs } from "./auth/time.ts";
import { type CallerRuntime, createCallerRuntime } from "./caller.ts";
import type { ClientOpts } from "./client.ts";
import {
  installConnectionAvailability,
  observeNatsTrellisConnection,
  type TrellisConnection,
} from "./connection.ts";
import {
  bindApiRoutes,
  type GeneratedParticipant,
  getParticipantRuntime,
  participantAvailability,
  refreshApiRoutes,
} from "./participant_runtime/participant.ts";
import type { ContractResourceBindings } from "./participant_runtime/schemas.ts";
import type { RuntimeApi } from "./participant_runtime/api.ts";
import { TransportError } from "./errors/index.ts";
import { type ResourceMigrations, TypedKV } from "./kv.ts";
import {
  DEFAULT_RUNTIME_MAX_RECONNECT_ATTEMPTS,
  type RuntimeTransport,
} from "./runtime_transport.ts";
import {
  ResourceUnavailableError,
  type RuntimeStateStores,
  Trellis,
  type TrellisOpts,
} from "./session.ts";
import { TypedStore } from "./store.ts";
import { recordTrellisDuration } from "./telemetry/mod.ts";

type ClientContract = GeneratedParticipant;

type ResourceMigrationsFor<
  TContract extends ClientContract,
  TKind extends "state" | "kv",
> =
  & Readonly<
    Partial<
      {
        [
          Name in keyof TContract[
            "resources"
          ] as TContract["resources"][Name] extends { kind: TKind } ? Name
            : never
        ]: TContract["resources"][Name] extends {
          codec: { decode(value: unknown): infer TValue };
        } ? ResourceMigrations<TValue>
          : never;
      }
    >
  >
  & Readonly<Record<string, ResourceMigrations<unknown> | undefined>>;

/** Direct State and KV migrations supplied by the application at connection time. */
export type ClientResourceMigrations<
  TContract extends ClientContract = ClientContract,
> = Readonly<{
  state?: ResourceMigrationsFor<TContract, "state">;
  kv?: ResourceMigrationsFor<TContract, "kv">;
}>;

type InstalledClientResources = Readonly<{
  generation: number;
  signature: string;
  active: { value: boolean };
  kv: Readonly<Record<string, TypedKV<unknown> | undefined>>;
  store: Readonly<Record<string, TypedStore | undefined>>;
}>;

type ClientResourceState = { current: InstalledClientResources };

/** Browser caller runtime whose lifecycle owner can revoke and end its session. */
export type ConnectedTrellisClient<TContract extends ClientContract> =
  & CallerRuntime<TContract>
  & { logout(): Promise<void> };

function createConnectedClient(args: {
  name: string;
  nc: NatsConnection;
  connection: TrellisConnection;
  inboxPrefix: string;
  sessionKey: string;
  sign(data: Uint8Array): Promise<Uint8Array>;
  contextDigest: string | (() => string);
  authorizationProviderCache: AuthorizationProviderCache;
  opts: {
    log: ClientOpts["log"];
    timeout: ClientOpts["timeout"];
    stream: ClientOpts["stream"];
    noResponderRetry: ClientOpts["noResponderRetry"];
    api: RuntimeApi;
    state: TrellisOpts<RuntimeApi>["state"];
    stateMigrations: TrellisOpts<RuntimeApi>["stateMigrations"];
    resourceGeneration: TrellisOpts<RuntimeApi>["resourceGeneration"];
    resourceAvailability: TrellisOpts<RuntimeApi>["resourceAvailability"];
    onSessionNotFound?: TrellisOpts<RuntimeApi>["onSessionNotFound"];
  };
}): Trellis<RuntimeApi, "client", RuntimeStateStores> {
  const trellis = new Trellis<RuntimeApi, "client", RuntimeStateStores>(
    args.name,
    args.nc,
    {
      sessionKey: args.sessionKey,
      sign: args.sign,
      contextDigest: args.contextDigest,
      authorizationProviderCache: args.authorizationProviderCache,
    },
    {
      ...args.opts,
      connection: args.connection,
    },
    args.inboxPrefix,
  );

  return trellis;
}

function clientConnectResult<T>(
  promise: Promise<T>,
): AsyncResult<T, TransportError | UnexpectedError | ClientAuthHandledError> {
  return AsyncResult.from(
    promise.then(
      (value): Result<
        T,
        TransportError | UnexpectedError | ClientAuthHandledError
      > => Result.ok(value),
      (
        cause,
      ): Result<T, TransportError | UnexpectedError | ClientAuthHandledError> =>
        Result.err(
          cause instanceof TransportError ||
            cause instanceof ClientAuthHandledError
            ? cause
            : new UnexpectedError({ cause }),
        ),
    ),
  );
}

type BrowserClientAuthOptions = {
  mode?: "browser";
  provider?: string;
  redirectTo?: string | (() => string);
  landingPath?: string;
  context?: unknown;
  currentUrl?: URL | string | (() => URL | string);
  flowId?: string;
  persistence?: "remembered" | "temporary";
};

type SessionKeyClientAuthOptionsBase = {
  mode: "session_key";
  sessionKeySeed: string;
  sessionId?: string;
  provider?: string;
  redirectTo: string;
  currentUrl?: URL | string | (() => URL | string);
  context?: unknown;
  flowId?: string;
};

type SessionKeyClientAuthOptions = SessionKeyClientAuthOptionsBase;

export type ClientAuthOptions =
  | BrowserClientAuthOptions
  | SessionKeyClientAuthOptions;

export type ClientAuthRequiredContext = {
  loginUrl: string;
  sessionKey: string;
  mode: "browser" | "session_key";
};

export type ClientAuthContinuation =
  | { status: "bound"; flowId: string }
  | { status: "handled" }
  | void;

export type ClientAuthHandledErrorData = {
  id: string;
  type: "ClientAuthHandledError";
  message: string;
  context?: Record<string, unknown>;
  traceId?: string;
};

/**
 * Error raised when client authentication was delegated to caller-owned routing.
 */
export class ClientAuthHandledError
  extends BaseError<ClientAuthHandledErrorData> {
  override readonly name = "ClientAuthHandledError" as const;

  constructor() {
    super("Client authentication was handled by the caller");
  }

  override toSerializable(): ClientAuthHandledErrorData {
    return this.baseSerializable() as ClientAuthHandledErrorData;
  }
}

type ClientConnectArgsFor<TContract extends ClientContract> =
  & ClientOpts
  & {
    trellisUrl: string;
    participant: TContract;
    auth?: ClientAuthOptions;
    resourceMigrations?: ClientResourceMigrations<TContract>;
    onAuthRequired?: (
      ctx: ClientAuthRequiredContext,
    ) => Promise<ClientAuthContinuation> | ClientAuthContinuation;
  };

async function resolveClientResources(args: {
  nc: NatsConnection;
  participant: ClientContract;
  participantDigest: string;
  bindings: ContractResourceBindings;
  previous?: InstalledClientResources;
  migrations?: ClientResourceMigrations;
}): Promise<InstalledClientResources> {
  const signature = JSON.stringify({
    participantDigest: args.participantDigest,
    bindings: args.bindings,
  });
  if (args.previous?.signature === signature) return args.previous;

  const generation = (args.previous?.generation ?? 0) + 1;
  const active = { value: true };
  const isCurrent = () => active.value;
  const kv: Record<string, TypedKV<unknown> | undefined> = {};
  const store: Record<string, TypedStore | undefined> = {};
  for (const [name, descriptor] of Object.entries(args.participant.resources)) {
    if (descriptor.kind === "kv") {
      const binding = args.bindings.kv?.[name];
      if (!binding) {
        if (descriptor.availability === "required") {
          throw new Error(`Required KV resource '${name}' is unavailable`);
        }
        kv[name] = undefined;
        continue;
      }
      kv[name] = await TypedKV.open(
        args.nc,
        binding.bucket,
        descriptor,
        {
          bindOnly: true,
          history: binding.history,
          ttl: binding.ttlMs,
          maxValueBytes: binding.maxValueBytes,
          migrations: args.migrations?.kv?.[name],
          isCurrent,
        },
      ).orThrow();
    } else if (descriptor.kind === "store") {
      const binding = args.bindings.store?.[name];
      if (!binding) {
        if (descriptor.availability === "required") {
          throw new Error(`Required Store resource '${name}' is unavailable`);
        }
        store[name] = undefined;
        continue;
      }
      store[name] = await TypedStore.open(args.nc, binding.name, {
        bindOnly: true,
        ttlMs: binding.ttlMs,
        maxObjectBytes: binding.maxObjectBytes,
        maxTotalBytes: binding.maxTotalBytes,
        isCurrent,
      }).orThrow();
    }
  }
  return { generation, signature, active, kv, store };
}

function clientResourceFacades(state: ClientResourceState) {
  const facade = (kind: "kv" | "store") => {
    const result: Record<string, unknown> = {};
    for (const name of Object.keys(state.current[kind])) {
      Object.defineProperty(result, name, {
        enumerable: true,
        get: () => {
          const handle = state.current[kind][name];
          if (!handle) throw new ResourceUnavailableError(kind, name);
          return handle;
        },
      });
    }
    return result;
  };
  return { kv: facade("kv"), store: facade("store") };
}

export type TrellisClientConnectArgs<
  TContract extends ClientContract = ClientContract,
> = ClientConnectArgsFor<TContract>;

type ClientRuntimeIdentity = {
  mode: "browser" | "session_key";
  sessionKey: string;
  sessionNkey: string;
  seed: Uint8Array;
  sessionId?: string;
  browserCredential?: BrowserSessionCredential;
  auth: TrellisAuth;
  sign(data: Uint8Array): Promise<Uint8Array>;
};

const ClientTransportEndpointsSchema = Type.Object({
  natsServers: Type.Array(Type.String({ minLength: 1 }), { minItems: 1 }),
});

const ClientTransportsSchema = Type.Object({
  native: Type.Optional(ClientTransportEndpointsSchema),
  websocket: Type.Optional(ClientTransportEndpointsSchema),
});
type RuntimeTransports = StaticDecode<typeof ClientTransportsSchema>;

type ClientConnectDeps = {
  loadTransport(): Promise<RuntimeTransport>;
  now(): number;
  initialBootstrap?: ClientBootstrapReady;
  runtimeSessionKeySeed?: string;
};

const ClientBootstrapReadySchema = Type.Object({
  status: Type.Literal("ready"),
  serverNow: Type.Integer(),
  connectInfo: Type.Object({
    connectionId: Type.Optional(Type.String({ minLength: 1 })),
    sessionId: Type.String({ minLength: 1 }),
    participantId: Type.String({ minLength: 1 }),
    participantDigest: Type.String({ minLength: 1 }),
    transports: ClientTransportsSchema,
    transport: Type.Object({
      inboxPrefix: Type.String({ minLength: 1 }),
      jwt: Type.String({ minLength: 1 }),
      jwtExpiresAt: Type.Integer({ minimum: 1 }),
    }),
    authorizationContext: AuthorizationContextBundleSchema,
  }),
}, { additionalProperties: false });

type ClientBootstrapReady = StaticDecode<typeof ClientBootstrapReadySchema> & {
  readonly serverClockOffsetMs?: number;
  readonly apiBindings: Readonly<Record<string, unknown>>;
  readonly resourceBindings: ContractResourceBindings;
};
type ClientBootstrapAuthRequired = {
  status: "auth_required";
  serverNow: number;
};
type ClientBootstrapNotReady = {
  status: "not_ready";
  reason: string;
  serverNow: number;
};
type ClientBootstrapResponse =
  | ClientBootstrapReady
  | ClientBootstrapAuthRequired
  | ClientBootstrapNotReady;
type ClockOffsetState = { serverClockOffsetMs: number };
type BrowserGlobalThis = typeof globalThis & {
  document?: unknown;
  window?: unknown;
};

function isBrowserRuntime(): boolean {
  const browserGlobal = globalThis as BrowserGlobalThis;
  return typeof browserGlobal.window !== "undefined" &&
    typeof browserGlobal.document !== "undefined";
}

function selectClientRuntimeTransportServers(
  transports: RuntimeTransports,
): string[] {
  if (isBrowserRuntime()) {
    if (transports.websocket?.natsServers?.length) {
      return transports.websocket.natsServers;
    }
    throw new Error(
      "Browser authorization runtime has no WebSocket NATS endpoints",
    );
  }
  if (transports.native?.natsServers?.length) {
    return transports.native.natsServers;
  }

  throw new Error("Authorization runtime has no native NATS endpoints");
}

const defaultDeps: ClientConnectDeps = {
  loadTransport: async () => {
    if (isBrowserRuntime()) {
      return { connect: wsconnect };
    }

    const mod = await import("./runtime_transport.ts");
    return await mod.loadDefaultRuntimeTransport();
  },
  now: () => Date.now(),
};

function transportCauseContext(cause: unknown): Record<string, unknown> {
  if (cause instanceof Error) {
    return { causeName: cause.name, causeMessage: cause.message };
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

const BindWireSchema = Type.Object({
  serverNow: Type.Integer(),
  session: Type.Object({
    sessionId: Type.String({ minLength: 1 }),
    principalId: Type.String({ minLength: 1 }),
    participantId: Type.String({ minLength: 1 }),
    sessionKey: Type.String({ minLength: 1 }),
    expiresAt: Type.Union([Type.Integer(), Type.Null()]),
  }),
});

type BrowserBindResult = {
  sessionId: string;
  expiresAt: number | null;
  serverNow: number;
  serverClockOffsetMs: number;
};

async function readJsonResponse(
  response: Response,
  args: {
    code: string;
    message: string;
    hint: string;
    context?: Record<string, unknown>;
  },
): Promise<unknown> {
  try {
    return await response.json();
  } catch (cause) {
    throw createTransportError({
      ...args,
      cause,
    });
  }
}

function normalizeTrellisUrl(trellisUrl: string): string {
  return new URL(trellisUrl).toString().replace(/\/$/, "");
}

function resolveCurrentUrl(
  auth?: { currentUrl?: URL | string | (() => URL | string) },
): URL | null {
  const currentUrl = typeof auth?.currentUrl === "function"
    ? auth.currentUrl()
    : auth?.currentUrl;
  if (currentUrl instanceof URL) return currentUrl;
  if (typeof currentUrl === "string") return new URL(currentUrl);
  return typeof globalThis.location === "undefined"
    ? null
    : new URL(globalThis.location.href);
}

function resolveRedirectTo(
  auth: BrowserClientAuthOptions,
  currentUrl: URL,
): string {
  const redirectTo = typeof auth.redirectTo === "function"
    ? auth.redirectTo()
    : auth.redirectTo;
  if (redirectTo) {
    return new URL(redirectTo, currentUrl.origin).toString();
  }

  const queryRedirect = currentUrl.searchParams.get("redirectTo");
  if (queryRedirect) {
    return new URL(queryRedirect, currentUrl.origin).toString();
  }

  if (auth.landingPath) {
    return new URL(auth.landingPath, currentUrl.origin).toString();
  }

  return currentUrl.toString();
}

function resolveConfiguredRedirectTo(
  redirectTo: string | (() => string) | undefined,
): string | undefined {
  return typeof redirectTo === "function" ? redirectTo() : redirectTo;
}

async function createSessionKeyRuntimeIdentity(
  sessionKeySeed: string,
  sessionId?: string,
  mode: "browser" | "session_key" = "session_key",
  browserCredential?: BrowserSessionCredential,
  runtimeSessionKeySeed?: string,
): Promise<ClientRuntimeIdentity> {
  const seed = base64urlDecode(sessionKeySeed);
  const runtimeSeed = runtimeSessionKeySeed
    ? base64urlDecode(runtimeSessionKeySeed)
    : seed;
  const privateKey = await importEd25519PrivateKeyFromSeedBase64url(
    base64urlEncode(runtimeSeed),
  );
  const sessionKey = publicKeyBase64urlFromSeed(runtimeSeed);
  const runtimeAuth = await createAuth({
    sessionKeySeed: base64urlEncode(runtimeSeed),
  });
  const sign = async (data: Uint8Array): Promise<Uint8Array> => {
    const signature = await crypto.subtle.sign(
      "Ed25519",
      privateKey,
      toArrayBuffer(data),
    );
    return new Uint8Array(signature);
  };

  const identity: ClientRuntimeIdentity = {
    mode,
    sessionKey,
    sessionNkey: runtimeAuth.sessionNkey,
    seed,
    auth: runtimeAuth,
    sessionId,
    ...(browserCredential === undefined ? {} : { browserCredential }),
    sign,
  };
  return identity;
}

async function resolveClientIdentity(
  auth: ClientAuthOptions | undefined,
  installation?: BrowserSessionStore,
): Promise<ClientRuntimeIdentity> {
  if (auth?.mode === "session_key") {
    return await createSessionKeyRuntimeIdentity(
      auth.sessionKeySeed,
      auth.sessionId,
    );
  }

  if (!installation) {
    throw new Error("browser installation storage is unavailable");
  }
  const stored = await installation.readLogin() ??
    await installation.getOrCreateCredential();
  return await createSessionKeyRuntimeIdentity(
    base64urlEncode(stored.seed),
    stored.loginSessionId,
    "browser",
    stored,
  );
}

async function clearBrowserLogin(
  installation: BrowserSessionStore | undefined,
  identity: ClientRuntimeIdentity,
): Promise<boolean> {
  if (!installation || !identity.browserCredential) return false;
  return await installation.clearLogin({
    generation: identity.browserCredential.generation,
    sessionKey: identity.browserCredential.sessionKey,
    ...(identity.sessionId === undefined
      ? {}
      : { loginSessionId: identity.sessionId }),
  });
}

async function bindClientFlow(args: {
  trellisUrl: string;
  origin: string;
  flowId: string;
  identity: ClientRuntimeIdentity;
  participant: ClientConnectArgsFor<ClientContract>["participant"];
}): Promise<BrowserBindResult> {
  const startedAt = performance.now();
  const requestStartedAtMs = Date.now();
  const requestId = ulid();
  const issuedAt = Date.now();
  const unsigned = {
    requestId,
    issuedAt,
  };
  const url = `${args.trellisUrl}/auth/flow/${
    encodeURIComponent(args.flowId)
  }/bind`;
  const init: RequestInit = {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Origin: args.origin,
    },
    body: JSON.stringify({
      ...unsigned,
      proof: await args.identity.auth.signSessionProof({
        purpose: "userAuthBind",
        origin: args.trellisUrl,
        flowId: args.flowId,
        sessionPublicKey: args.identity.sessionKey,
        unsignedRequest: unsigned,
      }),
    }),
  };
  let response = await fetch(url, init);
  for (const delay of [100, 200, 400, 800]) {
    if (response.status !== 503) break;
    const error = await decodeTrellisHttpError(response.clone());
    if (error.code !== "authorization_pending") break;
    await new Promise((resolve) => setTimeout(resolve, delay));
    response = await fetch(url, init);
  }
  if (!response.ok) {
    const error = await decodeTrellisHttpError(response);
    throw createTransportError({
      code: error.code,
      message: "Trellis could not finish the sign-in step.",
      hint: "Start the sign-in flow again.",
      cause: error,
      context: { status: error.status, trellisUrl: args.trellisUrl },
    });
  }

  const payload = await readJsonResponse(response, {
    code: "bind_invalid_response",
    message: "Trellis returned an invalid sign-in response.",
    hint: "Start the sign-in flow again.",
    context: { flowId: args.flowId },
  });
  let parsed: StaticDecode<typeof BindWireSchema>;
  try {
    parsed = Value.Parse(BindWireSchema, payload) as StaticDecode<
      typeof BindWireSchema
    >;
  } catch (cause) {
    throw createTransportError({
      code: "bind_invalid_response",
      message: "Trellis returned an invalid sign-in response.",
      hint: "Start the sign-in flow again.",
      cause,
      context: { flowId: args.flowId },
    });
  }
  if (
    parsed.session.sessionKey !== args.identity.sessionKey ||
    parsed.session.participantId !== args.participant.identity
  ) {
    throw new Error("Trellis returned a login for another installation");
  }
  const serverClockOffsetMs = estimateMidpointClockOffsetMs({
    requestStartedAtMs,
    responseReceivedAtMs: Date.now(),
    serverNowSeconds: parsed.serverNow / 1_000,
  });
  recordTrellisDuration(
    "trellis.connect.duration",
    performance.now() - startedAt,
    {
      phase: "bootstrap",
      participantKind: "client",
      outcome: "ok",
    },
  );
  return {
    sessionId: parsed.session.sessionId,
    expiresAt: parsed.session.expiresAt,
    serverNow: parsed.serverNow / 1_000,
    serverClockOffsetMs,
  };
}

async function recoverClientBootstrapWithRetry(args: {
  trellisUrl: string;
  identity: ClientRuntimeIdentity;
  cache: AuthorizationContextCache;
  deps: ClientConnectDeps;
  offsetState: ClockOffsetState;
  onTerminalSession?: () => Promise<void>;
}): Promise<ClientBootstrapResponse> {
  if (!args.identity.sessionId) {
    return {
      status: "auth_required",
      serverNow: args.deps.now() / 1_000,
    };
  }

  for (let attempt = 0; attempt < 10; attempt += 1) {
    const attemptStartedAt = performance.now();
    const requestStartedAtMs = args.deps.now();
    try {
      const result = await refreshAuthorizationContextWithMetadata({
        trellisUrl: args.trellisUrl,
        sessionId: args.identity.sessionId,
        auth: args.identity.auth,
        sessionKey: args.identity.sessionKey,
        cache: args.cache,
        requiredTransport: "websocket",
      });
      const responseReceivedAtMs = args.deps.now();
      const serverClockOffsetMs = estimateMidpointClockOffsetMs({
        requestStartedAtMs,
        responseReceivedAtMs,
        serverNowSeconds: result.response.serverNow / 1_000,
      });
      args.offsetState.serverClockOffsetMs = serverClockOffsetMs;
      const session = result.response.runtime;
      const nats = result.response;
      if (
        !nats.transports.native && !nats.transports.websocket
      ) {
        throw createTransportError({
          code: "trellis.bootstrap.invalid_response",
          message: "Trellis returned incomplete client recovery metadata.",
          hint: "Retry the connection. If it keeps happening, check Trellis.",
          context: { trellisUrl: args.trellisUrl },
        });
      }
      recordTrellisDuration(
        "trellis.connect.duration",
        performance.now() - attemptStartedAt,
        {
          phase: "bootstrap",
          participantKind: "client",
          outcome: "ok",
        },
      );
      return {
        status: "ready",
        serverNow: result.response.serverNow / 1_000,
        apiBindings: result.response.apiBindings,
        resourceBindings: result.response.authorization.resourceRuntime,
        connectInfo: {
          sessionId: session.loginSessionId!,
          participantId: session.participantId,
          participantDigest: result.response.authorization.participantDigest,
          transports: nats.transports,
          transport: {
            inboxPrefix: session.inboxPrefix,
            jwt: nats.routing.bootstrapJwt,
            jwtExpiresAt: nats.routing.bootstrapJwtExpiresAt,
          },
          authorizationContext: result.response.authorizationContext,
        },
      };
    } catch (error) {
      if (
        error instanceof AuthorizationContextRefreshError &&
        error.terminal
      ) {
        await args.onTerminalSession?.();
        return {
          status: "auth_required",
          serverNow: args.deps.now() / 1_000,
        };
      }
      if (
        error instanceof AuthorizationContextRefreshError &&
        attempt < 9
      ) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        continue;
      }
      if (attempt === 0) {
        continue;
      }
      throw error;
    }
  }

  throw createTransportError({
    code: "trellis.bootstrap.time_sync_failed",
    message: "Trellis could not confirm the client time window.",
    hint:
      "Retry the connection. If it keeps happening, check the client and Trellis clocks.",
    context: { trellisUrl: args.trellisUrl },
  });
}

async function createRuntimeUserAuthenticator(args: {
  identity: ClientRuntimeIdentity;
  sessionId: string;
  contextDigest: string | (() => string);
  jwt: string | (() => string);
  authorizationUsable?: () => boolean;
}): Promise<{ authenticators: Authenticator[]; stop: () => void }> {
  const options = await args.identity.auth.natsConnectOptions({
    sessionId: args.sessionId,
    contextDigest: args.contextDigest,
    jwt: args.jwt,
    authorizationUsable: args.authorizationUsable,
  });
  return {
    authenticators: Array.isArray(options.authenticator)
      ? options.authenticator
      : [options.authenticator],
    stop: () => {},
  };
}

function cleanupBrowserCallbackUrl(currentUrl: URL): void {
  if (!isBrowserRuntime()) return;
  if (
    !currentUrl.searchParams.has("flowId") &&
    !currentUrl.searchParams.has("authError")
  ) {
    return;
  }

  currentUrl.searchParams.delete("flowId");
  currentUrl.searchParams.delete("authError");
  globalThis.history.replaceState(
    {},
    "",
    currentUrl.pathname + currentUrl.search + currentUrl.hash,
  );
}

function isExpiredBindError(error: unknown): boolean {
  return error instanceof TransportError &&
    error.code === "flow_expired";
}

function needsReauth(
  bootstrap: ClientBootstrapResponse,
): bootstrap is
  | Extract<ClientBootstrapResponse, { status: "auth_required" }>
  | Extract<
    ClientBootstrapResponse,
    {
      status: "not_ready";
      reason: "contract_not_active" | "insufficient_permissions";
    }
  > {
  return bootstrap.status === "auth_required" ||
    (
      bootstrap.status === "not_ready" &&
      (bootstrap.reason === "insufficient_permissions" ||
        bootstrap.reason === "contract_not_active")
    );
}

function bootstrapTargetsRequestedContract<
  TContract extends ClientContract,
>(
  bootstrap: ClientBootstrapResponse,
  args: ClientConnectArgsFor<TContract>,
): boolean {
  return bootstrap.status === "ready" &&
    bootstrap.connectInfo.participantId === args.participant.identity;
}

async function buildSessionKeyLoginUrl(args: {
  trellisUrl: string;
  redirectTo: string;
  identity: ClientRuntimeIdentity;
  participant: ClientConnectArgsFor<ClientContract>["participant"];
}): Promise<
  { status: "auth_required"; flowId: string; loginUrl: string }
> {
  const startedAt = performance.now();
  const requestId = ulid();
  const issuedAt = Date.now();
  const unsigned = {
    requestId,
    issuedAt,
    sessionPublicKey: args.identity.sessionKey,
    participantId: args.participant.identity,
    redirectTarget: args.redirectTo,
  };
  const response = await fetch(`${args.trellisUrl}/auth/requests`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      ...unsigned,
      proof: await args.identity.auth.signSessionProof({
        purpose: "userAuthRequest",
        origin: args.trellisUrl,
        unsignedRequest: unsigned,
      }),
    }),
  });
  if (!response.ok) {
    const error = await decodeTrellisHttpError(response);
    throw createTransportError({
      code: error.code,
      message: "Trellis could not start sign-in.",
      hint:
        "Retry sign-in. If it keeps failing, check Trellis availability and access.",
      cause: error,
      context: { status: error.status, trellisUrl: args.trellisUrl },
    });
  }

  const payload = await readJsonResponse(response, {
    code: "auth_request_invalid_response",
    message: "Trellis returned an invalid sign-in response.",
    hint: "Retry sign-in. If it keeps happening, start the sign-in flow again.",
    context: { trellisUrl: args.trellisUrl },
  });
  const start = Value.Parse(
    Type.Object({
      flowId: Type.String({ minLength: 1 }),
      loginUrl: Type.String({ minLength: 1 }),
    }, { additionalProperties: false }),
    payload,
  );
  if (start) {
    recordTrellisDuration(
      "trellis.connect.duration",
      performance.now() - startedAt,
      {
        phase: "bootstrap",
        participantKind: "client",
        outcome: "ok",
      },
    );
    return {
      status: "auth_required",
      flowId: start.flowId,
      loginUrl: start.loginUrl,
    };
  }
  throw createTransportError({
    code: "auth_request_invalid_response",
    message: "Trellis returned an invalid sign-in response.",
    hint: "Retry sign-in. If it keeps happening, start the sign-in flow again.",
    context: { trellisUrl: args.trellisUrl },
  });
}

export async function connectClientWithDeps<
  TContract extends ClientContract,
>(
  args: ClientConnectArgsFor<TContract>,
  deps: ClientConnectDeps,
): Promise<ConnectedTrellisClient<TContract>> {
  const totalStartedAt = performance.now();
  const trellisUrl = normalizeTrellisUrl(args.trellisUrl);
  const trustScope = browserInstallationScope(
    trellisUrl,
    args.participant.identity,
  );
  const browserInstallation = args.auth?.mode === "session_key"
    ? undefined
    : new BrowserSessionStore(
      trustScope,
      args.auth?.persistence ?? "remembered",
    );
  let identity = deps.runtimeSessionKeySeed && args.auth?.mode === "session_key"
    ? await createSessionKeyRuntimeIdentity(
      args.auth.sessionKeySeed,
      args.auth.sessionId,
      "session_key",
      undefined,
      deps.runtimeSessionKeySeed,
    )
    : await resolveClientIdentity(args.auth, browserInstallation);
  const currentUrl = resolveCurrentUrl(args.auth);
  const browserAuth = args.auth?.mode === "session_key" ? undefined : args.auth;
  const callbackFlowId = args.auth?.mode === "session_key"
    ? args.auth.flowId
    : browserAuth?.flowId ?? currentUrl?.searchParams.get("flowId") ??
      undefined;
  const callbackAuthError = args.auth?.mode === "session_key"
    ? undefined
    : currentUrl?.searchParams.get("authError") ?? undefined;
  const offsetState: ClockOffsetState = { serverClockOffsetMs: 0 };

  const authorizationContexts = new AuthorizationContextCache(
    trellisUrl,
    (input, init) => globalThis.fetch(input, init),
    deps.now,
  );

  if (callbackAuthError) {
    if (currentUrl) cleanupBrowserCallbackUrl(currentUrl);
    throw createTransportError({
      code: callbackAuthError,
      message: "Trellis sign-in did not complete.",
      hint: "Start sign-in again if you want to approve access.",
      context: { reason: callbackAuthError, trellisUrl },
    });
  }

  let callbackBootstrap: ClientBootstrapResponse | undefined;
  if (callbackFlowId) {
    try {
      const bound = await bindClientFlow({
        trellisUrl,
        origin: currentUrl?.origin ?? new URL(trellisUrl).origin,
        flowId: callbackFlowId,
        identity,
        participant: args.participant,
      });
      if (browserInstallation) {
        const current = identity.browserCredential;
        if (
          !current || !await browserInstallation.completeBind({
            generation: current.generation,
            sessionKey: current.sessionKey,
            pendingFlowId: callbackFlowId,
          }, {
            loginSessionId: bound.sessionId,
            expiresAt: bound.expiresAt,
          })
        ) {
          globalThis.location?.reload();
          throw new Error("browser installation changed in another tab");
        }
        identity.browserCredential = {
          generation: current.generation,
          seed: current.seed,
          sessionKey: current.sessionKey,
          loginSessionId: bound.sessionId,
          expiresAt: bound.expiresAt,
        };
      }
      identity.sessionId = bound.sessionId;
      offsetState.serverClockOffsetMs = bound.serverClockOffsetMs;
      callbackBootstrap = await recoverClientBootstrapWithRetry({
        trellisUrl,
        identity,
        cache: authorizationContexts,
        deps,
        offsetState,
      });
    } catch (error) {
      if (currentUrl && isExpiredBindError(error)) {
        cleanupBrowserCallbackUrl(currentUrl);
      } else {
        throw error;
      }
    }
  }

  const initialBootstrapStartedAt = performance.now();
  const initialBootstrap = deps.initialBootstrap ?? callbackBootstrap ??
    await recoverClientBootstrapWithRetry({
      trellisUrl,
      identity,
      cache: authorizationContexts,
      deps,
      offsetState,
      onTerminalSession: async () => {
        if (
          browserInstallation &&
          !await clearBrowserLogin(browserInstallation, identity)
        ) {
          globalThis.location?.reload();
          throw new Error("browser installation changed in another tab");
        }
      },
    });
  recordTrellisDuration(
    "trellis.connect.duration",
    performance.now() - initialBootstrapStartedAt,
    {
      phase: "bootstrap",
      participantKind: "client",
      outcome: "ok",
    },
  );

  const authStartedAt = performance.now();
  let bootstrap = initialBootstrap;
  if (
    needsReauth(initialBootstrap) ||
    !bootstrapTargetsRequestedContract(initialBootstrap, args)
  ) {
    if (
      initialBootstrap.status === "ready" &&
      !bootstrapTargetsRequestedContract(initialBootstrap, args)
    ) {
      if (
        browserInstallation &&
        !await clearBrowserLogin(browserInstallation, identity)
      ) {
        globalThis.location?.reload();
        throw new Error("browser installation changed in another tab");
      }
    }
    if (browserInstallation) {
      identity = await resolveClientIdentity(args.auth, browserInstallation);
    }
    bootstrap = await resolveAuthRequired(
      args,
      identity,
      browserInstallation,
      currentUrl,
      authorizationContexts,
      deps,
      offsetState,
    );
  }
  recordTrellisDuration(
    "trellis.connect.duration",
    performance.now() - authStartedAt,
    {
      phase: "auth_resolution",
      participantKind: "client",
      outcome: "ok",
    },
  );

  if (bootstrap.status !== "ready") {
    if (bootstrap.status === "not_ready") {
      throw createTransportError({
        code: "trellis.bootstrap.not_ready",
        message: "Trellis is not ready to connect this client.",
        hint:
          "Wait for the requested app access to become available, then try again.",
        context: { reason: bootstrap.reason },
      });
    }
    throw createTransportError({
      code: "trellis.bootstrap.auth_required",
      message: "Trellis still requires sign-in before connecting this client.",
      hint: "Complete sign-in, then try again.",
    });
  }

  const transport = await deps.loadTransport();
  identity.auth.setServerClockOffsetMs(
    (bootstrap.serverClockOffsetMs ?? offsetState.serverClockOffsetMs) +
      deps.now() - Date.now(),
  );
  offsetState.serverClockOffsetMs = bootstrap.serverClockOffsetMs ??
    offsetState.serverClockOffsetMs;
  authorizationContexts.setServerClockOffsetMs(
    offsetState.serverClockOffsetMs,
  );
  if (deps.initialBootstrap) {
    await authorizationContexts.install(
      bootstrap.connectInfo.authorizationContext,
      {
        bootstrapJwt: bootstrap.connectInfo.transport.jwt,
        bootstrapJwtExpiresAt: bootstrap.connectInfo.transport.jwtExpiresAt,
      },
      bootstrap.serverNow,
      undefined,
      {
        connectionId: bootstrap.connectInfo.connectionId!,
        loginSessionId: bootstrap.connectInfo.sessionId,
        participantId: bootstrap.connectInfo.participantId,
        inboxPrefix: bootstrap.connectInfo.transport.inboxPrefix,
        transports: bootstrap.connectInfo.transports,
      },
    );
  }
  selectClientRuntimeTransportServers(bootstrap.connectInfo.transports);
  identity.sessionId = bootstrap.connectInfo.sessionId;
  if (callbackFlowId && currentUrl) cleanupBrowserCallbackUrl(currentUrl);
  const runtimeState = {
    participantDigest: bootstrap.connectInfo.participantDigest,
    sessionId: bootstrap.connectInfo.sessionId,
    jwt: () => authorizationContexts.transportRoutingJwt(),
    contextDigest: () => authorizationContexts.transportCurrent().contextDigest,
  };
  let endingSession = false;
  const handleSessionNotFound = identity.mode === "browser"
    ? async () => {
      if (endingSession) return;
      if (!await clearBrowserLogin(browserInstallation, identity)) {
        if (nc && !nc.isClosed()) await nc.close();
        globalThis.location?.reload();
        return;
      }
      const replacementIdentity = await resolveClientIdentity(
        browserAuth,
        browserInstallation,
      );
      const latestCurrentUrl = resolveCurrentUrl(browserAuth);
      try {
        await resolveAuthRequired(
          args,
          replacementIdentity,
          browserInstallation,
          latestCurrentUrl,
          authorizationContexts,
          deps,
          offsetState,
        );
      } catch (error) {
        if (error instanceof ClientAuthHandledError) {
          return;
        }
        throw error;
      }
    }
    : undefined;
  const runtimeAuth = await createRuntimeUserAuthenticator({
    identity,
    sessionId: runtimeState.sessionId,
    contextDigest: runtimeState.contextDigest,
    jwt: runtimeState.jwt,
    authorizationUsable: () =>
      authorizationProviderCache?.transportUsable() ?? true,
  });
  let nc: NatsConnection | undefined;
  let authorizationProviderCache: AuthorizationProviderCache | undefined;
  try {
    const natsStartedAt = performance.now();
    nc = await transport.connect({
      servers: selectClientRuntimeTransportServers(
        bootstrap.connectInfo.transports,
      ),
      maxReconnectAttempts: DEFAULT_RUNTIME_MAX_RECONNECT_ATTEMPTS,
      ignoreAuthErrorAbort: true,
      timeout: args.timeout ?? 10_000,
      inboxPrefix: bootstrap.connectInfo.transport.inboxPrefix,
      authenticator: runtimeAuth.authenticators,
    });
    const connectedNats = nc;
    authorizationProviderCache = await AuthorizationProviderCache.attach(
      connectedNats,
      authorizationContexts.bundle().authorizationRegistry,
      bootstrap.connectInfo.transport.inboxPrefix,
      authorizationContexts,
    );
    authorizationProviderCache.start();
    await authorizationProviderCache.waitReady();
    await authorizationProviderCache.retainOwnContext();
    void connectedNats.closed().then(
      () => authorizationProviderCache?.stop(),
      () => authorizationProviderCache?.stop(),
    );
    recordTrellisDuration(
      "trellis.connect.duration",
      performance.now() - natsStartedAt,
      {
        phase: "nats_connect",
        participantKind: "client",
        outcome: "ok",
      },
    );
  } catch (error) {
    authorizationProviderCache?.stop();
    if (nc && !nc.isClosed()) await nc.close();
    runtimeAuth.stop();
    throw createTransportError({
      code: "trellis.runtime.connect_failed",
      message: "Trellis could not open the runtime connection.",
      hint:
        "Retry the connection. If it keeps failing, check Trellis transport availability.",
      cause: error,
      context: { trellisUrl },
    });
  }
  if (!nc || !authorizationProviderCache) {
    throw new Error("Trellis client runtime connection was not established");
  }
  void nc.closed().then(() => runtimeAuth.stop(), () => runtimeAuth.stop());

  const clientOpts: ClientOpts = {
    ...(typeof args.name === "string" ? { name: args.name } : {}),
    ...(args.log ? { log: args.log } : {}),
    ...(typeof args.timeout === "number" ? { timeout: args.timeout } : {}),
    ...(typeof args.stream === "string" ? { stream: args.stream } : {}),
    ...(args.noResponderRetry
      ? { noResponderRetry: args.noResponderRetry }
      : {}),
  };
  const connection = observeNatsTrellisConnection({
    kind: "client",
    nc,
    log: false,
    availability: participantAvailability(
      args.participant,
      bootstrap.apiBindings,
      bootstrap.resourceBindings,
      authorizationContexts.current().context.grants.permissions,
    ),
    onTransportEvent: (event) =>
      authorizationProviderCache?.observeTransportEvent(event),
    ...(args.log
      ? {
        lifecycleLog: {
          log: args.log,
          context: { client: clientOpts.name ?? "client" },
        },
      }
      : {}),
  });
  connection.subscribe((status) =>
    authorizationProviderCache.observeConnectionPhase(status.phase)
  );
  const api = bindApiRoutes(
    getParticipantRuntime(args.participant).usedApi,
    bootstrap.apiBindings,
  ) as RuntimeApi;
  const resourceState: ClientResourceState = {
    current: await resolveClientResources({
      nc,
      participant: args.participant,
      participantDigest: runtimeState.participantDigest,
      bindings: bootstrap.resourceBindings,
      migrations: args.resourceMigrations,
    }),
  };
  const resourceFacades = clientResourceFacades(resourceState);
  let installedAvailability = participantAvailability(
    args.participant,
    bootstrap.apiBindings,
    bootstrap.resourceBindings,
    authorizationContexts.current().context.grants.permissions,
  );
  authorizationProviderCache.onOwnInvalidated(() => {
    resourceState.current.active.value = false;
    installConnectionAvailability(
      connection,
      participantAvailability(args.participant, {}, {}, []),
    );
  });
  authorizationProviderCache.onOwnResumed(() => {
    resourceState.current.active.value = true;
    installConnectionAvailability(connection, installedAvailability);
  });
  const stopContextRefresh = startAuthorizationContextRefresh({
    trellisUrl: args.trellisUrl,
    sessionId: runtimeState.sessionId,
    auth: identity.auth,
    sessionKey: identity.sessionKey,
    cache: authorizationContexts,
    refresh: async (shouldInstall) => {
      const result = await refreshAuthorizationContextWithMetadata({
        trellisUrl: args.trellisUrl,
        sessionId: runtimeState.sessionId,
        auth: identity.auth,
        sessionKey: identity.sessionKey,
        cache: authorizationContexts,
        shouldInstall,
        prepareOnly: true,
        prepareInstall: async (response) => {
          const nextResources = await resolveClientResources({
            nc,
            participant: args.participant,
            participantDigest: response.authorization.participantDigest,
            bindings: response.authorization.resourceRuntime,
            previous: resourceState.current,
            migrations: args.resourceMigrations,
          });
          return (verified) => {
            if (nextResources !== resourceState.current) {
              resourceState.current.active.value = false;
              resourceState.current = nextResources;
            } else {
              resourceState.current.active.value = true;
            }
            installedAvailability = participantAvailability(
              args.participant,
              response.apiBindings,
              response.authorization.resourceRuntime,
              verified.context.grants.permissions,
            );
            installConnectionAvailability(connection, installedAvailability);
            refreshApiRoutes(api, response.apiBindings);
          };
        },
      });
      return result.context;
    },
    onRefresh: async (context) => {
      nc.setServers(
        selectClientRuntimeTransportServers(
          authorizationContexts.transportRuntimeBinding().transports,
        ),
      );
      if (connection.status.phase === "connected") await nc.reconnect();
      await authorizationProviderCache.waitReady({ timeoutMs: 30_000 });
      const generation = authorizationProviderCache.connectionGeneration();
      await authorizationProviderCache.retainOwnCandidate(
        context.contextDigest,
        generation,
      );
      authorizationProviderCache.promoteOwnCandidate(
        context.contextDigest,
        generation,
      );
    },
    onTerminalFailure: async (error) => {
      if (!nc.isClosed()) {
        try {
          await nc.close();
        } catch {
          await nc.closed().catch(() => undefined);
        }
      }
      // A revoked or expired authority context is not a durable login failure.
      if (
        error instanceof AuthorizationContextRefreshError && !error.loginInvalid
      ) return;
      await handleSessionNotFound?.();
    },
  });
  void nc.closed().then(stopContextRefresh, stopContextRefresh);

  const state = getParticipantRuntime(args.participant).state as TrellisOpts<
    RuntimeApi
  >["state"];

  const client = createConnectedClient({
    name: clientOpts.name ?? "client",
    nc,
    connection,
    inboxPrefix: bootstrap.connectInfo.transport.inboxPrefix,
    sessionKey: identity.sessionKey,
    sign: identity.sign,
    contextDigest: () => authorizationContexts.current().contextDigest,
    authorizationProviderCache,
    opts: {
      log: clientOpts.log,
      timeout: clientOpts.timeout,
      stream: clientOpts.stream,
      noResponderRetry: clientOpts.noResponderRetry,
      api,
      state,
      stateMigrations: args.resourceMigrations?.state,
      resourceGeneration: () => resourceState.current.generation,
      resourceAvailability: (name) =>
        connection.availability().resources[name] ?? true,
      onSessionNotFound: handleSessionNotFound,
    },
  });
  recordTrellisDuration(
    "trellis.connect.duration",
    performance.now() - totalStartedAt,
    {
      phase: "total",
      participantKind: "client",
      outcome: "ok",
    },
  );
  const caller = createCallerRuntime(client, args.participant, resourceFacades);
  return Object.assign(caller, {
    logout: async () => {
      endingSession = true;
      try {
        const logout = Reflect.get(caller, "sessionsLogout");
        if (typeof logout !== "function") {
          throw new Error(
            "the participant contract must use Auth.Sessions.Logout",
          );
        }
        const result = Reflect.apply(logout, caller, [{}]);
        if (!(result instanceof AsyncResult)) {
          throw new Error("Auth.Sessions.Logout returned an invalid result");
        }
        const operationCompleted = await Promise.race([
          result.orThrow().then(() => true),
          nc.closed().then(() => false),
        ]);
        if (!operationCompleted) {
          try {
            await refreshAuthorizationContextWithMetadata({
              trellisUrl,
              sessionId: identity.sessionId!,
              auth: identity.auth,
              cache: authorizationContexts,
              requiredTransport: "websocket",
            });
            throw new Error("logout did not revoke the current session");
          } catch (error) {
            if (
              !(error instanceof AuthorizationContextRefreshError) ||
              !error.terminal
            ) {
              throw error;
            }
          }
        }
      } finally {
        try {
          await clearBrowserLogin(browserInstallation, identity);
        } finally {
          try {
            if (!nc.isClosed()) await nc.close();
          } finally {
            endingSession = false;
          }
        }
      }
    },
  });
}

async function resolveAuthRequired<
  TContract extends ClientContract,
>(
  args: ClientConnectArgsFor<TContract>,
  identity: ClientRuntimeIdentity,
  browserInstallation: BrowserSessionStore | undefined,
  currentUrl: URL | null,
  cache: AuthorizationContextCache,
  deps: ClientConnectDeps,
  offsetState: ClockOffsetState,
): Promise<ClientBootstrapResponse> {
  const browserAuth: BrowserClientAuthOptions =
    args.auth?.mode === "session_key" ? {} : args.auth ?? {};
  const redirectTo = args.auth?.mode === "session_key"
    ? args.auth.redirectTo
    : currentUrl
    ? resolveRedirectTo(browserAuth, currentUrl)
    : resolveConfiguredRedirectTo(browserAuth.redirectTo);
  if (!redirectTo) {
    throw new Error("Client authentication requires a redirectTo URL");
  }

  const authStart = await buildSessionKeyLoginUrl({
    trellisUrl: normalizeTrellisUrl(args.trellisUrl),
    redirectTo,
    identity,
    participant: args.participant,
  });
  if (browserInstallation) {
    const current = identity.browserCredential;
    if (
      !current || !await browserInstallation.rememberFlow({
        generation: current.generation,
        sessionKey: current.sessionKey,
      }, authStart.flowId)
    ) {
      globalThis.location?.reload();
      throw new Error("browser installation changed in another tab");
    }
    identity.browserCredential = {
      ...current,
      pendingFlowId: authStart.flowId,
    };
  }

  const loginUrl = authStart.loginUrl;

  const continuationStartedAt = performance.now();
  const continuation = await args.onAuthRequired?.({
    loginUrl,
    sessionKey: identity.sessionKey,
    mode: identity.mode,
  });
  recordTrellisDuration(
    "trellis.connect.duration",
    performance.now() - continuationStartedAt,
    {
      phase: "bootstrap",
      participantKind: "client",
      outcome: "ok",
    },
  );
  if (continuation && continuation.status === "handled") {
    throw new ClientAuthHandledError();
  }

  if (continuation && continuation.status === "bound") {
    const bindStartedAt = performance.now();
    const bound = await bindClientFlow({
      trellisUrl: normalizeTrellisUrl(args.trellisUrl),
      origin: new URL(redirectTo).origin,
      flowId: continuation.flowId,
      identity,
      participant: args.participant,
    });
    if (browserInstallation) {
      const current = identity.browserCredential;
      if (
        !current || !await browserInstallation.completeBind({
          generation: current.generation,
          sessionKey: current.sessionKey,
          pendingFlowId: continuation.flowId,
        }, {
          loginSessionId: bound.sessionId,
          expiresAt: bound.expiresAt,
        })
      ) {
        throw new Error("browser installation changed in another tab");
      }
      identity.browserCredential = {
        generation: current.generation,
        seed: current.seed,
        sessionKey: current.sessionKey,
        loginSessionId: bound.sessionId,
        expiresAt: bound.expiresAt,
      };
    }
    identity.sessionId = bound.sessionId;
    recordTrellisDuration(
      "trellis.connect.duration",
      performance.now() - bindStartedAt,
      {
        phase: "bootstrap",
        participantKind: "client",
        outcome: "ok",
      },
    );
    return recoverClientBootstrapWithRetry({
      trellisUrl: normalizeTrellisUrl(args.trellisUrl),
      identity,
      cache,
      deps,
      offsetState,
    });
  }

  if (isBrowserRuntime()) {
    globalThis.location.href = loginUrl;
    throw new ClientAuthHandledError();
  }

  throw new Error(
    "Client authentication required and no auth continuation was provided",
  );
}

/** Connects user-facing participants to the Trellis caller runtime. */
export class TrellisClient {
  static connect<
    TContract extends ClientContract,
  >(
    args: ClientConnectArgsFor<TContract>,
  ): AsyncResult<
    ConnectedTrellisClient<TContract>,
    TransportError | UnexpectedError | ClientAuthHandledError
  >;
  static connect(
    args: TrellisClientConnectArgs,
  ): AsyncResult<
    unknown,
    TransportError | UnexpectedError | ClientAuthHandledError
  > {
    return clientConnectResult(connectClientWithDeps(args, defaultDeps));
  }
}
