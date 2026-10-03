import {
  type ClientAuthContinuation,
  type ClientAuthRequiredContext,
  type ClientOpts,
  TrellisClient,
} from "@oatscenter/trellis";
import { recordTrellisDuration } from "@oatscenter/trellis/telemetry";
import { dirname, join } from "@std/path";

import { NativeTransportGate, type SerialWriter } from "./native_gate.ts";
import { NatsFrameParser } from "./nats_wire.ts";

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

const WORKDIR_PREFIX = "trellis-testkit-";
const WORKDIR_OWNER_MARKER = ".trellis-testkit-owner";

/** Serializes writes to one destination so their ordering is preserved. */
class SerializedWriter implements SerialWriter {
  #chain: Promise<void> = Promise.resolve();
  readonly #writer: WritableStreamDefaultWriter<Uint8Array>;

  constructor(stream: WritableStream<Uint8Array>) {
    this.#writer = stream.getWriter();
  }

  write(bytes: Uint8Array): Promise<void> {
    const result = this.#chain.then(() => this.#writer.write(bytes));
    // Keep a rejected write from poisoning later ordered operations; the caller
    // still observes the failure through the returned promise.
    this.#chain = result.catch(() => undefined);
    return result;
  }
}

/** Transparent TCP proxy used to interrupt and observe the native path. @internal */
export class TcpProxy {
  readonly url: string;
  readonly #listener: Deno.TcpListener;
  readonly #target: Deno.ConnectOptions;
  readonly #connections = new Set<Deno.Conn>();
  readonly #gate: NativeTransportGate | undefined;
  #interrupted = false;

  private constructor(
    listener: Deno.TcpListener,
    target: Deno.ConnectOptions,
    advertisedHost: string,
    scheme: string,
    gate?: NativeTransportGate,
  ) {
    this.#listener = listener;
    this.#target = target;
    this.#gate = gate;
    this.url = `${scheme}://${advertisedHost}:${listener.addr.port}`;
    this.#accept();
  }

  static start(
    targetUrl: string,
    options: {
      bindHostname?: string;
      advertisedHost?: string;
      scheme?: string;
      gate?: NativeTransportGate;
    } = {},
  ): TcpProxy {
    const target = new URL(targetUrl);
    return new TcpProxy(
      Deno.listen({ hostname: options.bindHostname ?? "127.0.0.1", port: 0 }),
      {
        transport: "tcp",
        hostname: target.hostname,
        port: Number(target.port),
      },
      options.advertisedHost ?? "127.0.0.1",
      options.scheme ?? "ws",
      options.gate,
    );
  }

  #drop(connection: Deno.Conn): void {
    // Send FIN before closing a resource with a pending read.
    void connection.closeWrite().catch(() => undefined);
    try {
      connection.close();
    } catch {
      // The peer may have closed first.
    }
  }

  /**
   * Interrupt the path: drop every live connection and refuse new ones while
   * keeping the listener bound, so an ordinary reconnect cannot succeed until
   * {@link restore}. This models a real transport outage rather than a logical
   * event synthesized inside the client.
   */
  interrupt(): void {
    this.#interrupted = true;
    for (const connection of [...this.#connections]) {
      this.#drop(connection);
    }
    this.#connections.clear();
  }

  /** Restore the path after {@link interrupt}; new connections forward again. */
  restore(): void {
    this.#interrupted = false;
  }

  async #accept(): Promise<void> {
    try {
      for await (const client of this.#listener) {
        if (this.#interrupted) {
          try {
            client.close();
          } catch {
            // The peer may have closed first.
          }
          continue;
        }
        this.#connections.add(client);
        // A refused upstream is local to this connection; forwarding owns cleanup.
        void this.#forward(client).catch(() => undefined);
      }
    } catch (error) {
      if (!(error instanceof Deno.errors.BadResource)) throw error;
    }
  }

  async #forward(client: Deno.Conn): Promise<void> {
    let upstream: Deno.Conn | undefined;
    let connectionId: number | undefined;
    let clientReader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    let upstreamReader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    const pumps: Promise<void>[] = [];
    try {
      // Check before opening the upstream and again immediately after, so a
      // partition that lands in that interval closes both sides instead of
      // silently forwarding through the outage.
      if (this.#interrupted) return;
      upstream = await Deno.connect(this.#target);
      if (this.#interrupted) return;
      this.#connections.add(upstream);
      const toClient = new SerializedWriter(client.writable);
      const toUpstream = new SerializedWriter(upstream.writable);
      connectionId = this.#gate?.connectionOpened(toClient);
      // Only the gate-enabled path needs protocol framing; without a gate the
      // pump forwards raw bytes so binary payloads are never held or reframed.
      const gating = this.#gate !== undefined && connectionId !== undefined
        ? { gate: this.#gate, connectionId }
        : undefined;
      clientReader = client.readable.getReader();
      upstreamReader = upstream.readable.getReader();
      pumps.push(
        this.#pump(clientReader, toUpstream, "c2s", gating),
        this.#pump(upstreamReader, toClient, "s2c", gating),
      );
      await Promise.race(pumps);
    } finally {
      // FIN must precede read cancellation: cancelling first can leave the
      // underlying socket open while its peer waits forever for EOF.
      await Promise.allSettled([
        client.closeWrite(),
        ...(upstream === undefined ? [] : [upstream.closeWrite()]),
      ]);
      await Promise.allSettled([
        ...(clientReader === undefined ? [] : [clientReader.cancel()]),
        ...(upstreamReader === undefined ? [] : [upstreamReader.cancel()]),
      ]);
      if (connectionId !== undefined) {
        this.#gate?.connectionClosed(connectionId);
      }
      if (upstream !== undefined) {
        this.#connections.delete(upstream);
        this.#drop(upstream);
      }
      this.#connections.delete(client);
      this.#drop(client);
      await Promise.allSettled(pumps);
    }
  }

  async #pump(
    reader: ReadableStreamDefaultReader<Uint8Array>,
    sink: SerializedWriter,
    direction: "c2s" | "s2c",
    gating: { gate: NativeTransportGate; connectionId: number } | undefined,
  ): Promise<void> {
    // A gate-free proxy must be byte-transparent: forward each raw chunk
    // unchanged so binary payloads without a CRLF (WebSocket, TLS, arbitrary
    // data) are neither held waiting for a frame boundary nor split into
    // meaningless frames. Framing is only needed to observe/withhold NATS
    // protocol traffic for the readiness gate.
    const parser = gating === undefined ? undefined : new NatsFrameParser();
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (parser === undefined || gating === undefined) {
          await sink.write(value);
          continue;
        }
        for (const frame of parser.push(value)) {
          if (direction === "s2c") {
            if (
              gating.gate.onServerFrame(
                gating.connectionId,
                frame.op,
                frame.raw,
              )
            ) {
              continue;
            }
          } else {
            if (
              gating.gate.onClientFrame(
                gating.connectionId,
                frame.op,
                frame.raw,
              )
            ) {
              continue;
            }
          }
          await sink.write(frame.raw);
        }
      }
    } catch (error) {
      if (
        !(error instanceof Deno.errors.BadResource) &&
        !(error instanceof Deno.errors.BrokenPipe) &&
        !(error instanceof Deno.errors.ConnectionReset)
      ) {
        throw error;
      }
    }
  }

  stop(): void {
    this.#listener.close();
    for (const connection of this.#connections) {
      this.#drop(connection);
    }
    this.#connections.clear();
  }
}

/** Runs an isolated Trellis control plane and NATS server for integration tests. */
export class TrellisTestRuntime implements AsyncDisposable {
  readonly trellisUrl: string;
  /** Public browser origin; loopback unless a non-loopback browser host was requested. */
  readonly publicOrigin: string;
  readonly natsUrl: string;
  /** NATS WebSocket endpoint for exercising the same broker with browser clients. */
  readonly natsWebsocketUrl: string;
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
    approveApply(
      pendingId: string,
      opts?: {
        excludeResources?: readonly string[];
        excludeCapabilities?: readonly string[];
      },
    ): Promise<TrellisTestParticipantApproval>;
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
  #nativeProxy: TcpProxy | undefined;
  #nativeGate: NativeTransportGate | undefined;
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
    publicOrigin: string;
    workdir: string;
    deployment: string;
    keepWorkdir: boolean;
    timeouts: RuntimeTimeouts;
    adminPassword: string;
    getBootstrapUrl: () => Promise<string>;
    configPath?: string;
    config?: ReturnType<typeof buildControlPlaneConfig>;
    websocketProxy?: TcpProxy;
    nativeProxy?: TcpProxy;
    nativeGate?: NativeTransportGate;
    trellisOptions?: TrellisTestRuntimeStartOptions["trellis"];
    nats: NatsTestContainer;
    controlPlane?: TrellisProcessHandle;
    admin: TrellisTestAdminAutomation;
    ownsWorkdir?: boolean;
  }) {
    this.trellisUrl = args.trellisUrl;
    this.publicOrigin = args.publicOrigin;
    this.natsUrl = args.nats.natsUrl;
    this.natsWebsocketUrl = args.websocketProxy?.url ?? args.nats.websocketUrl;
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
    this.#nativeProxy = args.nativeProxy;
    this.#nativeGate = args.nativeGate;
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
      approveApply: (pendingId, opts) =>
        this.#admin.approveParticipantApply(pendingId, opts),
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
    let nativeProxy: TcpProxy | undefined;
    let nativeGate: NativeTransportGate | undefined;
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
      const browserHost = options.browserHost;
      if (options.rotatableWebsocketProxy || browserHost) {
        websocketProxy = TcpProxy.start(nats.websocketUrl, {
          ...(browserHost
            ? { bindHostname: "0.0.0.0", advertisedHost: browserHost }
            : {}),
        });
      }
      if (options.interruptibleNativeProxy) {
        nativeGate = new NativeTransportGate();
        nativeProxy = TcpProxy.start(nats.natsUrl, {
          scheme: "nats",
          gate: nativeGate,
        });
      }
      portLease = reserveLocalPort();
      const port = portLease.port;
      const trellisUrl = browserHost
        ? `http://${browserHost}:${port}`
        : `http://localhost:${port}`;
      const publicOrigin = trellisUrl;
      const config = buildControlPlaneConfig({
        workdir,
        natsUrl: nats.natsUrl,
        websocketUrl: websocketProxy?.url ?? nats.websocketUrl,
        nativeNatsServers: nativeProxy?.url,
        manifest: nats.manifest,
        port,
        publicOrigin,
        oauthProviders: options.oauthProviders,
        webOrigins: options.webOrigins,
        webSource: options.webSource,
        portalSource: options.portalSource,
        consoleSource: options.consoleSource,
        ttlMs: options.ttlMs,
        authorization: options.authorization,
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
        `trellis-testkit-${generateSessionSeed()}`;
      controlPlane = startedControlPlane;
      const getBootstrapUrl = (): Promise<string> =>
        startedControlPlane.waitForBootstrapUrl(timeouts.startupMs);
      const admin = new TrellisTestAdminAutomation({
        trellisUrl: startedControlPlane.trellisUrl,
        adminPassword,
        defaultDeployment: deployment,
        getBootstrapUrl,
        resourceReadyTimeoutMs: timeouts.startupMs,
      });
      return new TrellisTestRuntime({
        trellisUrl: startedControlPlane.trellisUrl,
        publicOrigin,
        workdir,
        deployment,
        keepWorkdir: options.keepWorkdir ?? false,
        timeouts,
        adminPassword,
        getBootstrapUrl,
        configPath,
        config,
        websocketProxy,
        nativeProxy,
        nativeGate,
        trellisOptions: options.trellis,
        nats,
        controlPlane: startedControlPlane,
        admin,
      });
    } catch (error) {
      portLease?.release();
      await controlPlane?.stop().catch(() => undefined);
      websocketProxy?.stop();
      nativeProxy?.stop();
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

  /** Connects an app/client via generated APIs, waiting for required startup resources. */
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
    let staleAttempts = 0;
    while (!client) {
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
          typeof cause === "object" && cause !== null &&
          "status" in cause && "code" in cause &&
          cause.status === 409 &&
          [
            "authority_changed",
            "consent_decision_stale",
            "consent_view_changed",
          ]
            .includes(String(cause.code)) &&
          staleAttempts++ < 2
        ) continue;
        throw error;
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

  /**
   * Interrupts the advertised native NATS path: every native client drops and
   * cannot reconnect until {@link restoreNativeTransport}. Only available when
   * the runtime was started with `interruptibleNativeProxy`.
   */
  interruptNativeTransport(): void {
    if (this.#nativeProxy === undefined) {
      throw new Error("Runtime was not started with interruptibleNativeProxy");
    }
    this.#nativeProxy.interrupt();
  }

  /** Restores the native NATS path interrupted by {@link interruptNativeTransport}. */
  restoreNativeTransport(): void {
    if (this.#nativeProxy === undefined) {
      throw new Error("Runtime was not started with interruptibleNativeProxy");
    }
    this.#nativeProxy.restore();
  }

  /**
   * Observation and readiness barrier for the native NATS path. The barrier can
   * hold one generation's post-subscription readiness flush and the gate records
   * the exact deliveries and outbound authorization contexts a test needs to
   * attribute a real served RPC to a physical generation. Only available when
   * the runtime was started with `interruptibleNativeProxy`.
   */
  nativeTransportGate(): NativeTransportGate {
    if (this.#nativeGate === undefined) {
      throw new Error("Runtime was not started with interruptibleNativeProxy");
    }
    return this.#nativeGate;
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
    this.#nativeProxy?.stop();
    try {
      await this.#nats.stop();
    } catch (error) {
      failures.push(error);
    }
    if (this.#ownsWorkdir && !this.#keepWorkdir) {
      try {
        await Deno.remove(this.workdir, { recursive: true });
      } catch (error) {
        // An already-absent workdir is a satisfied cleanup, not a failure.
        if (!(error instanceof Deno.errors.NotFound)) failures.push(error);
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
