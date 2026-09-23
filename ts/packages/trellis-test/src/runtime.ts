import {
  type ClientAuthContinuation,
  type ClientAuthRequiredContext,
  type ClientOpts,
  TrellisClient,
} from "@oatscenter/trellis";
import { recordTrellisDuration } from "@oatscenter/trellis/telemetry";
import { dirname, join } from "@std/path";

import { TrellisTestAdminAutomation } from "./admin_client.ts";
import { ADMIN_USERNAME } from "./admin/methods.ts";
import type { AdminRpc, AdminRpcInput } from "./admin/methods.ts";
import {
  removeStaleMarkedDirectories,
  writeTrellisTestOwnerMarker,
} from "./cleanup.ts";
import {
  buildControlPlaneConfig,
  generateSessionSeed,
  type ReservedPort,
  reserveLocalPort,
  writeTrellisConfig,
} from "./control_plane_config.ts";
import { NatsTestContainer } from "./nats_container.ts";
import { sqliteMemoryUrl as sqliteMemoryUrlHelper } from "./temp.ts";
import {
  startTrellisProcess,
  type TrellisProcessHandle,
} from "./trellis_process.ts";
import type {
  TrellisTestClientAuth,
  TrellisTestClientKey,
  TrellisTestClientParticipant,
  TrellisTestConnectedClient,
  TrellisTestParticipantApplyResult,
  TrellisTestParticipantApproval,
  TrellisTestParticipantLike,
  TrellisTestRuntimeStartOptions,
  TrellisTestServiceKey,
  WaitForOptions,
} from "./types.ts";
import { waitFor as waitForHelper } from "./wait.ts";

type ConnectedClient = { connection: { close(): Promise<void> } };
type RuntimeTimeouts = {
  startupMs: number;
  waitForMs: number;
  shutdownMs: number;
};

const WORKDIR_PREFIX = "trellis-test-";
const WORKDIR_OWNER_MARKER = ".trellis-test-owner";

class TcpProxy {
  readonly url: string;
  readonly #listener: Deno.TcpListener;
  readonly #target: Deno.ConnectOptions;
  readonly #connections = new Set<Deno.Conn>();

  private constructor(listener: Deno.TcpListener, target: Deno.ConnectOptions) {
    this.#listener = listener;
    this.#target = target;
    this.url = `ws://127.0.0.1:${listener.addr.port}`;
    this.#accept();
  }

  static start(targetUrl: string): TcpProxy {
    const target = new URL(targetUrl);
    return new TcpProxy(
      Deno.listen({ hostname: "127.0.0.1", port: 0 }),
      {
        transport: "tcp",
        hostname: target.hostname,
        port: Number(target.port),
      },
    );
  }

  async #accept(): Promise<void> {
    try {
      for await (const client of this.#listener) {
        this.#connections.add(client);
        void this.#forward(client);
      }
    } catch (error) {
      if (!(error instanceof Deno.errors.BadResource)) throw error;
    }
  }

  async #forward(client: Deno.Conn): Promise<void> {
    try {
      const upstream = await Deno.connect(this.#target);
      this.#connections.add(upstream);
      await Promise.allSettled([
        client.readable.pipeTo(upstream.writable),
        upstream.readable.pipeTo(client.writable),
      ]);
      this.#connections.delete(upstream);
      try {
        upstream.close();
      } catch {
        // The stream may already have closed the connection.
      }
    } finally {
      this.#connections.delete(client);
      try {
        client.close();
      } catch {
        // The stream may already have closed the connection.
      }
    }
  }

  stop(): void {
    this.#listener.close();
    for (const connection of this.#connections) {
      try {
        connection.close();
      } catch {
        // The peer may have closed first.
      }
    }
    this.#connections.clear();
  }
}

/** Runs an isolated Trellis control plane and NATS server for integration tests. */
export class TrellisTestRuntime implements AsyncDisposable {
  readonly trellisUrl: string;
  readonly natsUrl: string;
  readonly workdir: string;
  /** Local test-admin username used by the harness bootstrap. */
  readonly adminUsername = ADMIN_USERNAME;
  /** Local test-admin password configured for this runtime. */
  readonly adminPassword: string;
  readonly deployments: {
    create(
      args: {
        id?: string;
        kind?: "service" | "device";
        reviewMode?: "none" | "required";
      },
    ): Promise<void>;
  };
  readonly contracts: {
    apply(
      args: {
        deployment?: string;
        contract: TrellisTestParticipantLike;
      },
    ): Promise<TrellisTestParticipantApproval>;
    install(
      args: { contract: TrellisTestParticipantLike },
    ): Promise<TrellisTestParticipantApproval>;
    requestApply(
      args: { deployment?: string; contract: TrellisTestParticipantLike },
    ): Promise<TrellisTestParticipantApplyResult>;
    approveApply(pendingId: string): Promise<TrellisTestParticipantApproval>;
  };
  readonly services: {
    createInstance(args: {
      deployment?: string;
      name: string;
      contract: TrellisTestParticipantLike;
    }): Promise<TrellisTestServiceKey>;
    provisionInstanceOnly(
      args: { deployment?: string },
    ): Promise<{ seed: string; sessionKey: string }>;
    disableInstance(
      input:
        import("../trellis/index.js").apis.auth.ServiceInstancesDisableInput,
    ): Promise<
      import("../trellis/index.js").apis.auth.ServiceInstancesDisableOutput
    >;
  };
  readonly devices: {
    provision(
      input: import("../trellis/index.js").apis.auth.DevicesProvisionInput,
    ): Promise<
      import("../trellis/index.js").apis.auth.DevicesProvisionOutput
    >;
  };
  readonly state: {
    resourcesInspect(
      input: import("../trellis/index.js").apis.state.ResourcesInspectInput,
    ): Promise<import("../trellis/index.js").apis.state.ResourcesInspectOutput>;
    resourcesQuery(
      input: import("../trellis/index.js").apis.state.ResourcesQueryInput,
    ): Promise<import("../trellis/index.js").apis.state.ResourcesQueryOutput>;
  };
  readonly events: {
    consumersQuery(
      input: AdminRpcInput<"eventsConsumersQuery">,
    ): Promise<AdminRpc["eventsConsumersQuery"]["output"]>;
    deadLettersQuery(
      input: AdminRpcInput<"eventsDeadLettersQuery">,
    ): Promise<AdminRpc["eventsDeadLettersQuery"]["output"]>;
    deadLettersInspect(
      input: AdminRpcInput<"eventsDeadLettersInspect">,
    ): Promise<AdminRpc["eventsDeadLettersInspect"]["output"]>;
    deadLettersReplay(
      input: AdminRpcInput<"eventsDeadLettersReplay">,
    ): Promise<AdminRpc["eventsDeadLettersReplay"]["output"]>;
  };
  #controlPlane: TrellisProcessHandle | undefined;
  #nats: NatsTestContainer;
  #admin: TrellisTestAdminAutomation;
  #configPath: string | undefined;
  #config: ReturnType<typeof buildControlPlaneConfig> | undefined;
  #websocketProxy: TcpProxy | undefined;
  #trellisOptions: TrellisTestRuntimeStartOptions["trellis"] | undefined;
  #keepWorkdir: boolean;
  #ownsWorkdir: boolean;
  #deployment: string;
  #getBootstrapUrl: () => Promise<string>;
  #timeouts: RuntimeTimeouts;
  #clients = new Set<ConnectedClient>();
  #stopped = false;

  /** Returns the one-time first-administrator bootstrap URL for this runtime. */
  async bootstrapUrl(): Promise<string> {
    return await this.#getBootstrapUrl();
  }

  /** Forwards one validated low-level Auth RPC over the harness admin transport. */
  async callAdminRpc<M extends keyof AdminRpc>(
    method: M,
    input: AdminRpc[M]["input"],
  ): Promise<AdminRpc[M]["output"]> {
    return await this.#admin.callAdminRpc(
      method,
      input,
    ) as AdminRpc[M]["output"];
  }

  /** Completes the first-administrator bootstrap through the harness automation. */
  async ensureAdmin(): Promise<void> {
    await this.#admin.completeBootstrap();
  }

  private constructor(args: {
    trellisUrl: string;
    workdir: string;
    deployment: string;
    keepWorkdir: boolean;
    timeouts: RuntimeTimeouts;
    adminPassword: string;
    getBootstrapUrl: () => Promise<string>;
    configPath?: string;
    config?: ReturnType<typeof buildControlPlaneConfig>;
    websocketProxy?: TcpProxy;
    trellisOptions?: TrellisTestRuntimeStartOptions["trellis"];
    nats: NatsTestContainer;
    controlPlane?: TrellisProcessHandle;
    admin: TrellisTestAdminAutomation;
    ownsWorkdir?: boolean;
  }) {
    this.trellisUrl = args.trellisUrl;
    this.natsUrl = args.nats.natsUrl;
    this.workdir = args.workdir;
    this.#deployment = args.deployment;
    this.#keepWorkdir = args.keepWorkdir;
    this.#ownsWorkdir = args.ownsWorkdir ?? true;
    this.#getBootstrapUrl = args.getBootstrapUrl;
    this.adminPassword = args.adminPassword;
    this.#timeouts = args.timeouts;
    this.#nats = args.nats;
    this.#controlPlane = args.controlPlane;
    this.#configPath = args.configPath;
    this.#config = args.config;
    this.#websocketProxy = args.websocketProxy;
    this.#trellisOptions = args.trellisOptions;
    this.#admin = args.admin;
    this.deployments = {
      create: ({ id, kind, reviewMode }) =>
        this.#admin.createDeployment({
          deployment: id ?? this.#deployment,
          kind,
          reviewMode,
        }),
    };
    this.contracts = {
      apply: ({ deployment, contract }) =>
        this.#admin.applyParticipant({
          deployment: deployment ?? this.#deployment,
          contract,
        }),
      install: ({ contract }) => this.#admin.installParticipant({ contract }),
      requestApply: ({ deployment, contract }) =>
        this.#admin.requestParticipantApply({
          deployment: deployment ?? this.#deployment,
          contract,
        }),
      approveApply: (pendingId) =>
        this.#admin.approveParticipantApply(pendingId),
    };
    this.services = {
      createInstance: ({ deployment, contract }) =>
        this.#admin.provisionServiceInstance({
          deployment: deployment ?? this.#deployment,
          contract,
        }),
      provisionInstanceOnly: ({ deployment }) =>
        this.#admin.provisionServiceInstanceOnly({
          deployment: deployment ?? this.#deployment,
        }),
      disableInstance: (input) => this.#admin.disableServiceInstance(input),
    };
    this.devices = {
      provision: (input) => this.#admin.provisionDevice(input),
    };
    this.state = {
      resourcesInspect: (input) => this.#admin.stateResourcesInspect(input),
      resourcesQuery: (input) => this.#admin.stateResourcesQuery(input),
    };
    this.events = {
      consumersQuery: (input) => this.#admin.eventsConsumersQuery(input),
      deadLettersQuery: (input) => this.#admin.eventsDeadLettersQuery(input),
      deadLettersInspect: (input) =>
        this.#admin.eventsDeadLettersInspect(input),
      deadLettersReplay: (input) => this.#admin.eventsDeadLettersReplay(input),
    };
  }

  /** Starts an isolated Trellis test runtime. */
  static async start(
    options: TrellisTestRuntimeStartOptions,
  ): Promise<TrellisTestRuntime> {
    if (options?.trellis?.command === undefined) {
      throw new Error("TrellisTestRuntime.start requires trellis.command");
    }
    for (let attempt = 1;; attempt++) {
      try {
        return await TrellisTestRuntime.#startOnce(options);
      } catch (error) {
        if (
          attempt >= 3 ||
          !String(error).toLowerCase().includes("address already in use")
        ) {
          throw error;
        }
      }
    }
  }

  static async #startOnce(
    options: TrellisTestRuntimeStartOptions,
  ): Promise<TrellisTestRuntime> {
    const workdir = await Deno.makeTempDir({ prefix: WORKDIR_PREFIX });
    await writeTrellisTestOwnerMarker(workdir, WORKDIR_OWNER_MARKER);
    await removeStaleMarkedDirectories({
      parent: dirname(workdir),
      prefix: WORKDIR_PREFIX,
      markerName: WORKDIR_OWNER_MARKER,
    });
    let nats: NatsTestContainer | undefined;
    let controlPlane: TrellisProcessHandle | undefined;
    let portLease: ReservedPort | undefined;
    let websocketProxy: TcpProxy | undefined;
    try {
      const timeouts = {
        startupMs: options.timeouts?.startupMs ?? 30_000,
        waitForMs: options.timeouts?.waitForMs ?? 5_000,
        shutdownMs: options.timeouts?.shutdownMs ?? 5_000,
      };
      await Deno.mkdir(join(workdir, "trellis"), { recursive: true });
      nats = await NatsTestContainer.start(workdir, {
        startupMs: timeouts.startupMs,
      });
      if (options.rotatableWebsocketProxy) {
        websocketProxy = TcpProxy.start(nats.websocketUrl);
      }
      portLease = reserveLocalPort();
      const port = portLease.port;
      const trellisUrl = `http://localhost:${port}`;
      const config = buildControlPlaneConfig({
        workdir,
        natsUrl: nats.natsUrl,
        websocketUrl: websocketProxy?.url ?? nats.websocketUrl,
        manifest: nats.manifest,
        port,
        oauthProviders: options.oauthProviders,
        webOrigins: options.webOrigins,
        webSource: options.webSource,
        portalSource: options.portalSource,
        consoleSource: options.consoleSource,
        ttlMs: options.ttlMs,
      });
      const configPath = await writeTrellisConfig({ workdir, config });
      const startedControlPlane = await startTrellisProcess({
        trellisUrl,
        configPath,
        options: options.trellis,
        startupTimeoutMs: timeouts.startupMs,
        shutdownTimeoutMs: timeouts.shutdownMs,
        portLease,
      });
      portLease = undefined;
      const deployment = options.deployment ?? "test";
      const adminPassword = options.adminPassword ??
        `trellis-test-${generateSessionSeed()}`;
      controlPlane = startedControlPlane;
      const getBootstrapUrl = (): Promise<string> =>
        startedControlPlane.waitForBootstrapUrl(timeouts.startupMs);
      const admin = new TrellisTestAdminAutomation({
        trellisUrl: startedControlPlane.trellisUrl,
        adminPassword,
        defaultDeployment: deployment,
        getBootstrapUrl,
      });
      return new TrellisTestRuntime({
        trellisUrl: startedControlPlane.trellisUrl,
        workdir,
        deployment,
        keepWorkdir: options.keepWorkdir ?? false,
        timeouts,
        adminPassword,
        getBootstrapUrl,
        configPath,
        config,
        websocketProxy,
        trellisOptions: options.trellis,
        nats,
        controlPlane: startedControlPlane,
        admin,
      });
    } catch (error) {
      portLease?.release();
      await controlPlane?.stop().catch(() => undefined);
      websocketProxy?.stop();
      await nats?.stop().catch(() => undefined);
      if (!options.keepWorkdir) {
        await Deno.remove(workdir, { recursive: true }).catch(() => undefined);
      }
      throw error;
    }
  }

  /** Registers a service contract and creates a service instance key. */
  async registerService(args: {
    name: string;
    contract: TrellisTestParticipantLike;
    deployment?: string;
  }): Promise<TrellisTestServiceKey> {
    return await this.#admin.registerService({
      deployment: args.deployment ?? this.#deployment,
      contract: args.contract,
    });
  }

  /** Creates app/client session-key material for public `TrellisClient.connect` calls. */
  async registerClient(args: {
    name: string;
    contract: TrellisTestClientParticipant;
  }): Promise<TrellisTestClientKey> {
    const participantId = args.contract.identity;
    if (!participantId.startsWith("trellis.")) {
      await this.#admin.installParticipant({ contract: args.contract });
    }
    const seed = generateSessionSeed();
    return {
      seed,
      participantId,
    };
  }

  /**
   * Returns auth options and admin-backed auth continuation for a registered
   * app/client participant. Spread the result into `TrellisClient.connect(...)`.
   */
  clientAuth(key: TrellisTestClientKey): TrellisTestClientAuth {
    return {
      auth: {
        mode: "session_key",
        sessionKeySeed: key.seed,
        redirectTo: `${this.trellisUrl}/_trellis/test/client-auth`,
      },
      onAuthRequired: (ctx) => this.#admin.completeClientAuth(ctx),
    };
  }

  /**
   * Completes a test app/client auth flow through runtime admin automation.
   */
  async completeClientAuth(
    ctx: ClientAuthRequiredContext,
  ): Promise<ClientAuthContinuation> {
    return await this.#admin.completeClientAuth(ctx);
  }

  /** Configures the built-in test portal's consent ceiling for a participant. */
  async ensurePortalConsentPolicy(
    participantId: string,
    selectionIds: readonly string[],
  ): Promise<void> {
    await this.#admin.ensurePortalConsentPolicy(participantId, selectionIds);
  }

  /** Connects an app/client participant through the public generated client surface. */
  async connectClient<
    TContract extends TrellisTestClientParticipant,
  >(
    args: ClientOpts & {
      name: string;
      contract: TContract;
    },
  ): Promise<TrellisTestConnectedClient<TContract>> {
    const startedAt = performance.now();
    const key = await this.registerClient(args);
    const auth = this.clientAuth(key);
    let client: TrellisTestConnectedClient<TContract> | undefined;
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        client = await TrellisClient.connect({
          ...args,
          trellisUrl: this.trellisUrl,
          participant: args.contract,
          auth: auth.auth,
          onAuthRequired: auth.onAuthRequired,
        }).orThrow() as TrellisTestConnectedClient<TContract>;
        break;
      } catch (error) {
        let cause = error;
        while (
          typeof cause === "object" && cause !== null && "cause" in cause &&
          cause.cause !== undefined
        ) cause = cause.cause;
        if (
          attempt === 2 || typeof cause !== "object" || cause === null ||
          !("status" in cause) || cause.status !== 409 ||
          !("code" in cause) ||
          ![
            "authority_changed",
            "consent_decision_stale",
            "consent_view_changed",
          ]
            .includes(String(cause.code))
        ) {
          throw error;
        }
      }
    }
    if (!client) throw new Error("Client authentication did not complete");
    this.#clients.add(client);
    recordTrellisDuration(
      "trellis.connect.duration",
      performance.now() - startedAt,
      { participantKind: "client", phase: "total" },
    );
    return client as TrellisTestConnectedClient<TContract>;
  }

  /** Polls until `fn` returns a truthy value. */
  waitFor<T>(
    fn: () =>
      | T
      | null
      | undefined
      | false
      | Promise<T | null | undefined | false>,
    opts?: WaitForOptions,
  ): Promise<T> {
    return waitForHelper(fn, {
      timeoutMs: opts?.timeoutMs ?? this.#timeouts.waitForMs,
      intervalMs: opts?.intervalMs,
    });
  }

  /** Flushes the underlying NATS connection. */
  async flush(): Promise<void> {
    await this.#nats.nc.flush();
  }

  /** Drains the underlying NATS connection. */
  async drain(): Promise<void> {
    await this.#nats.nc.drain();
  }

  /** Restarts only the Trellis control-plane process, preserving workdir, SQLite state, and NATS. */
  async restartControlPlane(): Promise<void> {
    if (this.#stopped) {
      throw new Error("Cannot restart a stopped Trellis test runtime");
    }
    if (
      this.#controlPlane === undefined || this.#configPath === undefined ||
      this.#trellisOptions === undefined
    ) {
      throw new Error("Cannot restart an attached Trellis test runtime");
    }

    await this.#admin.prepareForControlPlaneRestart();
    await this.#controlPlane.stop();
    const port = Number(new URL(this.trellisUrl).port);
    const portLease = reserveLocalPort(port);
    this.#controlPlane = await startTrellisProcess({
      trellisUrl: this.trellisUrl,
      configPath: this.#configPath,
      options: this.#trellisOptions,
      startupTimeoutMs: this.#timeouts.startupMs,
      shutdownTimeoutMs: this.#timeouts.shutdownMs,
      portLease,
    });
  }

  /** Replaces the browser WebSocket endpoint and retires the prior listener. */
  async rotateWebsocketProxy(): Promise<[string, string]> {
    if (
      this.#websocketProxy === undefined || this.#config === undefined ||
      this.#configPath === undefined
    ) {
      throw new Error("Runtime was not started with rotatableWebsocketProxy");
    }
    const retired = this.#websocketProxy;
    const replacement = TcpProxy.start(this.#nats.websocketUrl);
    this.#config.client.natsServers = [replacement.url];
    this.#config.web.allowInsecureOrigins = [
      ...this.#config.web.allowInsecureOrigins.filter((origin) =>
        origin !== retired.url
      ),
      replacement.url,
    ];
    await writeTrellisConfig({
      workdir: this.workdir,
      config: this.#config,
      configPath: this.#configPath,
    });
    await this.restartControlPlane();
    this.#websocketProxy = replacement;
    retired.stop();
    return [retired.url, replacement.url];
  }

  /** Returns a service-owned SQLite path under this runtime workdir. */
  async tempSqlitePath(name = "test.sqlite"): Promise<string> {
    const dir = join(this.workdir, "sqlite");
    await Deno.mkdir(dir, { recursive: true });
    return join(dir, name);
  }

  /** Returns the SQLite in-memory URL used by service-owned tests. */
  sqliteMemoryUrl(): string {
    return sqliteMemoryUrlHelper();
  }

  /** Stops clients, control plane, NATS, and the temp directory. */
  async stop(): Promise<void> {
    if (this.#stopped) return;
    this.#stopped = true;
    const failures: unknown[] = [];
    for (const client of this.#clients) {
      try {
        await client.connection.close();
      } catch (error) {
        failures.push(error);
      }
    }
    try {
      await this.#admin.close();
    } catch (error) {
      failures.push(error);
    }
    try {
      await this.#controlPlane?.stop();
    } catch (error) {
      failures.push(error);
    }
    this.#websocketProxy?.stop();
    try {
      await this.#nats.stop();
    } catch (error) {
      failures.push(error);
    }
    if (this.#ownsWorkdir && !this.#keepWorkdir) {
      try {
        await Deno.remove(this.workdir, { recursive: true });
      } catch (error) {
        failures.push(error);
      }
    }
    if (failures.length > 0) {
      throw new AggregateError(
        failures,
        `Failed to clean up ${failures.length} Trellis test runtime resource(s)`,
      );
    }
  }

  /** @internal Returns recent control-plane process output for test failures. */
  controlPlaneOutput(): string {
    return this.#controlPlane?.outputTails() ?? "";
  }

  [Symbol.asyncDispose](): Promise<void> {
    return this.stop();
  }
}
