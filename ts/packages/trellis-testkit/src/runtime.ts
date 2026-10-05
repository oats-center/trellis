import {
  type ClientAuthContinuation,
  type ClientAuthRequiredContext,
  type ClientOpts,
  TrellisClient,
} from "@oatscenter/trellis";
import { recordTrellisDuration } from "@oatscenter/trellis/telemetry";
import { dirname, join } from "@std/path";
import type { ConnectionOptions, NatsConnection } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { z } from "zod";

import { NativeTransportGate, type SerialWriter } from "./native_gate.ts";
import { NatsFrameParser } from "./nats_wire.ts";

import { TrellisTestAdminAutomation } from "./admin_client.ts";
import { ADMIN_USERNAME } from "./admin/methods.ts";
import { generateSessionSeed } from "./auth/random.ts";
import type { AdminRpc, AdminRpcInput } from "./admin/methods.ts";
import {
  removeStaleMarkedDirectories,
  writeTrellisTestOwnerMarker,
} from "./cleanup.ts";
import { type ReservedPort, reserveLocalPort } from "./ports.ts";
import {
  resolveNativeDistribution,
  trellisTestCacheDir,
} from "./native_distribution.ts";
import { sqliteMemoryUrl as sqliteMemoryUrlHelper } from "./temp.ts";
import {
  type ResolvedTrellisProcessCommand,
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

type LaunchPlan = {
  server: string;
  configPath: string;
  credsPath: string;
  environment: Record<string, string>;
  mode: string;
  ports: readonly number[];
  cache: string;
  advertisedNative?: string;
  advertisedWebsocket?: string;
};

function serverCommand(plan: LaunchPlan): ResolvedTrellisProcessCommand {
  const args = [
    "--config",
    plan.configPath,
    "--nats-download",
    "--isolated-process-group",
    `--local-nats-ports=${plan.ports.slice(1).join(",")}`,
    `--local-nats-cache=${plan.cache}`,
  ];
  if (plan.advertisedNative) {
    args.push("--advertise-nats-server", plan.advertisedNative);
  }
  if (plan.advertisedWebsocket) {
    args.push("--advertise-nats-websocket", plan.advertisedWebsocket);
  }
  args.push(plan.mode);
  return {
    cmd: plan.server,
    args,
    env: plan.environment,
    isolatedProcessGroup: true,
  };
}

async function runBootstrapCommand(
  cmd: string,
  args: string[],
  environment: Record<string, string>,
  timeoutMs: number,
  input?: string,
): Promise<Uint8Array> {
  const child = new Deno.Command(cmd, {
    args,
    env: environment,
    clearEnv: true,
    stdin: input === undefined ? "null" : "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const output = child.output();
  const timeout = setTimeout(() => {
    try {
      child.kill("SIGKILL");
    } catch { /* already exited */ }
  }, timeoutMs);
  try {
    if (input !== undefined) {
      const writer = child.stdin.getWriter();
      await writer.write(new TextEncoder().encode(input));
      await writer.close();
    }
    const result = await output;
    if (!result.success) {
      throw new Error(
        `Production bootstrap failed (exit ${result.code}): ${
          new TextDecoder().decode(result.stderr).slice(-8192)
        }`,
      );
    }
    return result.stdout;
  } finally {
    clearTimeout(timeout);
    try {
      child.kill("SIGTERM");
    } catch { /* already exited */ }
    await output.catch(() => undefined);
  }
}

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
  #nc: NatsConnection;
  #observations = new Set<NatsConnection>();
  #admin: TrellisTestAdminAutomation;
  #launch: LaunchPlan;
  #directWebsocket: string;
  #websocketProxy: TcpProxy | undefined;
  #nativeProxy: TcpProxy | undefined;
  #nativeGate: NativeTransportGate | undefined;
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
    launch: LaunchPlan;
    natsUrl: string;
    websocketUrl: string;
    websocketProxy?: TcpProxy;
    nativeProxy?: TcpProxy;
    nativeGate?: NativeTransportGate;
    nc: NatsConnection;
    controlPlane?: TrellisProcessHandle;
    admin: TrellisTestAdminAutomation;
    ownsWorkdir?: boolean;
  }) {
    this.trellisUrl = args.trellisUrl;
    this.publicOrigin = args.publicOrigin;
    this.natsUrl = args.natsUrl;
    this.natsWebsocketUrl = args.websocketProxy?.url ?? args.websocketUrl;
    this.workdir = args.workdir;
    this.#deployment = args.deployment;
    this.#keepWorkdir = args.keepWorkdir;
    this.#ownsWorkdir = args.ownsWorkdir ?? true;
    this.#getBootstrapUrl = args.getBootstrapUrl;
    this.adminPassword = args.adminPassword;
    this.#timeouts = args.timeouts;
    this.#nc = args.nc;
    this.#controlPlane = args.controlPlane;
    this.#launch = args.launch;
    this.#directWebsocket = args.websocketUrl;
    this.#websocketProxy = args.websocketProxy;
    this.#nativeProxy = args.nativeProxy;
    this.#nativeGate = args.nativeGate;
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
    options: TrellisTestRuntimeStartOptions = {},
  ): Promise<TrellisTestRuntime> {
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
    const native = await resolveNativeDistribution(options.trellis?.source);
    const workdir = await Deno.makeTempDir({ prefix: WORKDIR_PREFIX });
    await Deno.chmod(workdir, 0o700);
    await writeTrellisTestOwnerMarker(workdir, WORKDIR_OWNER_MARKER);
    await removeStaleMarkedDirectories({
      parent: dirname(workdir),
      prefix: WORKDIR_PREFIX,
      markerName: WORKDIR_OWNER_MARKER,
    });
    let nc: NatsConnection | undefined;
    let controlPlane: TrellisProcessHandle | undefined;
    const portLeases: ReservedPort[] = [];
    let websocketProxy: TcpProxy | undefined;
    let nativeProxy: TcpProxy | undefined;
    let nativeGate: NativeTransportGate | undefined;
    try {
      const timeouts = {
        startupMs: options.timeouts?.startupMs ?? 30_000,
        waitForMs: options.timeouts?.waitForMs ?? 5_000,
        shutdownMs: options.timeouts?.shutdownMs ?? 5_000,
      };
      const policy = options.trellis?.environment;
      const environment = policy?.inherit === false ? {} : Deno.env.toObject();
      for (const name of policy?.unset ?? []) delete environment[name];
      Object.assign(environment, policy?.set);
      for (
        const [variable, directory] of Object.entries({
          HOME: "home",
          XDG_CONFIG_HOME: "config",
          XDG_DATA_HOME: "data",
          XDG_STATE_HOME: "state",
          XDG_RUNTIME_DIR: "runtime",
        })
      ) {
        const path = join(workdir, directory);
        await Deno.mkdir(path, { mode: 0o700 });
        environment[variable] = path;
      }
      environment.NO_COLOR = "1";
      environment.TOKIO_WORKER_THREADS ??= "2";
      environment.TRELLIS_CACHE_DIR = trellisTestCacheDir();
      environment.TRELLIS_CONFIG = join(
        workdir,
        "config",
        "trellis",
        "config.toml",
      );
      for (let index = 0; index < 4; index++) {
        portLeases.push(reserveLocalPort());
      }
      const [port, nativePort, monitorPort, websocketPort] = portLeases.map((
        lease,
      ) => lease.port);
      const natsUrl = `nats://127.0.0.1:${nativePort}`;
      const websocketUrl = `ws://127.0.0.1:${websocketPort}`;
      const browserHost = options.browserHost;
      if (options.rotatableWebsocketProxy || browserHost) {
        websocketProxy = TcpProxy.start(websocketUrl, {
          ...(browserHost
            ? { bindHostname: "0.0.0.0", advertisedHost: browserHost }
            : {}),
        });
      }
      if (options.interruptibleNativeProxy) {
        nativeGate = new NativeTransportGate();
        nativeProxy = TcpProxy.start(natsUrl, {
          scheme: "nats",
          gate: nativeGate,
        });
      }
      const trellisUrl = browserHost
        ? `http://${browserHost}:${port}`
        : `http://localhost:${port}`;
      const publicOrigin = trellisUrl;
      const initArgs = [
        "--format",
        "json",
        "init",
        "config",
        "--out",
        join(workdir, "config", "trellis"),
        "--trellis-port",
        String(port),
        "--nats-port",
        String(nativePort),
        "--nats-monitor-port",
        String(monitorPort),
        "--nats-ws-port",
        String(websocketPort),
        "--nats-server-url",
        natsUrl,
        "--nats-websocket-url",
        websocketUrl,
        "--public-origin",
        publicOrigin,
        "--bind-address",
        browserHost ? "0.0.0.0" : "127.0.0.1",
        "--rate-limit-max",
        "0",
      ];
      for (const origin of options.webOrigins ?? []) {
        initArgs.push("--extra-origin", origin);
      }
      for (
        const [surface, source] of [["web", options.webSource], [
          "portal",
          options.portalSource,
        ], ["console", options.consoleSource]] as const
      ) {
        if (source) {
          initArgs.push(
            `--${surface}-${"directory" in source ? "directory" : "proxy"}`,
            "directory" in source ? source.directory : source.proxy,
          );
        }
      }
      for (
        const [flag, value] of [
          ["platform-sessions-ttl-ms", options.ttlMs?.sessions],
          ["platform-oauth-ttl-ms", options.ttlMs?.oauth],
          ["platform-device-flow-ttl-ms", options.ttlMs?.deviceFlow],
          ["platform-pending-auth-ttl-ms", options.ttlMs?.pendingAuth],
          [
            "auth-context-lifetime-seconds",
            options.authorization?.contextLifetimeSeconds,
          ],
          [
            "auth-refresh-lead-seconds",
            options.authorization?.refreshLeadSeconds,
          ],
          [
            "auth-refresh-jitter-seconds",
            options.authorization?.refreshJitterSeconds,
          ],
          [
            "auth-minimum-context-lifetime-seconds",
            options.authorization?.minimumContextLifetimeSeconds,
          ],
        ] as const
      ) if (value !== undefined) initArgs.push(`--${flag}`, String(value));
      if (
        options.oauthProviders && Object.keys(options.oauthProviders).length
      ) {
        const inputPath = join(workdir, "config", "oauth-providers.json");
        await Deno.writeTextFile(
          inputPath,
          JSON.stringify(options.oauthProviders),
          { mode: 0o600 },
        );
        initArgs.push("--oauth-providers-file", inputPath);
      }
      const output = await runBootstrapCommand(
        native.cli,
        initArgs,
        environment,
        timeouts.startupMs,
      );
      const bootstrap = z.object({
        trellisConfig: z.string(),
        trellisRuntimeCreds: z.string(),
      }).parse(JSON.parse(new TextDecoder().decode(output)));
      environment.TRELLIS_CONFIG = bootstrap.trellisConfig;
      const adminPassword = options.adminPassword ??
        `trellis-testkit-${crypto.randomUUID()}`;
      const seeded = (options.firstAdmin ?? "seeded") === "seeded";
      if (seeded) {
        await runBootstrapCommand(
          native.server,
          [
            "--config",
            bootstrap.trellisConfig,
            "bootstrap-admin",
            "--username",
            ADMIN_USERNAME,
            "--password-stdin",
          ],
          environment,
          timeouts.startupMs,
          `${adminPassword}\n`,
        );
      }
      const launch: LaunchPlan = {
        server: native.server,
        configPath: bootstrap.trellisConfig,
        credsPath: bootstrap.trellisRuntimeCreds,
        environment,
        mode: options.trellis?.mode ?? "all",
        ports: [port, nativePort, monitorPort, websocketPort],
        cache: trellisTestCacheDir(),
        advertisedNative: nativeProxy?.url,
        advertisedWebsocket: websocketProxy?.url,
      };
      const startedControlPlane = await startTrellisProcess({
        trellisUrl,
        configPath: launch.configPath,
        command: serverCommand(launch),
        startupTimeoutMs: timeouts.startupMs,
        shutdownTimeoutMs: timeouts.shutdownMs,
        portLeases,
      });
      controlPlane = startedControlPlane;
      nc = await connect({
        servers: natsUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(launch.credsPath),
        ),
        timeout: timeouts.startupMs,
      });
      const deployment = options.deployment ?? "test";
      const getBootstrapUrl = (): Promise<string> => {
        if (seeded) {
          return Promise.reject(
            new Error(
              "bootstrapUrl() is only available with firstAdmin: browser-flow",
            ),
          );
        }
        return startedControlPlane.waitForBootstrapUrl(timeouts.startupMs);
      };
      const admin = new TrellisTestAdminAutomation({
        trellisUrl: startedControlPlane.trellisUrl,
        adminPassword,
        defaultDeployment: deployment,
        getBootstrapUrl,
        resourceReadyTimeoutMs: timeouts.startupMs,
        bootstrapComplete: seeded,
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
        launch,
        natsUrl,
        websocketUrl,
        websocketProxy,
        nativeProxy,
        nativeGate,
        nc,
        controlPlane: startedControlPlane,
        admin,
      });
    } catch (error) {
      for (const lease of portLeases) lease.release();
      await nc?.close().catch(() => undefined);
      await controlPlane?.stop().catch(() => undefined);
      websocketProxy?.stop();
      nativeProxy?.stop();
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
    await this.#nc.flush();
  }

  /** Connect an isolated privileged broker observer for real adapter integration.
   * This connection does not own the broker. It closes on stop; reconnect after
   * restart. Service-author tests should normally use registerService/connectClient.
   */
  async connectNats(
    options: Pick<
      ConnectionOptions,
      "servers" | "timeout" | "maxReconnectAttempts" | "reconnectTimeWait"
    > = {},
  ): Promise<NatsConnection> {
    const connection = await connect({
      servers: this.natsUrl,
      timeout: this.#timeouts.startupMs,
      ...options,
      authenticator: credsAuthenticator(
        await Deno.readFile(this.#launch.credsPath),
      ),
    });
    if (this.#stopped) {
      await connection.close();
      throw new Error("Trellis test runtime is stopped");
    }
    this.#observations.add(connection);
    return connection;
  }

  /** Drains the underlying NATS connection. */
  async drain(): Promise<void> {
    await this.#nc.drain();
  }

  /** Restarts the production host and managed broker, preserving SQLite and NATS storage. */
  async restart(): Promise<void> {
    if (this.#stopped) {
      throw new Error("Cannot restart a stopped Trellis test runtime");
    }
    await this.#admin.prepareForControlPlaneRestart();
    await Promise.all(
      [...this.#observations].map((connection) => connection.close()),
    );
    this.#observations.clear();
    await this.#nc.close();
    await this.#controlPlane?.stop();
    const portLeases: ReservedPort[] = [];
    try {
      for (const port of this.#launch.ports) {
        portLeases.push(reserveLocalPort(port));
      }
      this.#controlPlane = await startTrellisProcess({
        trellisUrl: this.trellisUrl,
        configPath: this.#launch.configPath,
        command: serverCommand(this.#launch),
        startupTimeoutMs: this.#timeouts.startupMs,
        shutdownTimeoutMs: this.#timeouts.shutdownMs,
        portLeases,
      });
      this.#nc = await connect({
        servers: this.natsUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(this.#launch.credsPath),
        ),
        timeout: this.#timeouts.startupMs,
      });
    } catch (error) {
      await this.#controlPlane?.stop();
      throw error;
    } finally {
      for (const lease of portLeases) lease.release();
    }
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

  /** Returns the advertised native proxy endpoint for raw transport clients. */
  nativeProxyUrl(): string {
    if (!this.#nativeProxy) {
      throw new Error("Runtime was not started with interruptibleNativeProxy");
    }
    return this.#nativeProxy.url;
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
      this.#websocketProxy === undefined
    ) {
      throw new Error("Runtime was not started with rotatableWebsocketProxy");
    }
    const retired = this.#websocketProxy;
    const replacement = TcpProxy.start(this.#directWebsocket);
    const previous = this.#launch.advertisedWebsocket;
    this.#launch.advertisedWebsocket = replacement.url;
    try {
      await this.restart();
    } catch (error) {
      replacement.stop();
      this.#launch.advertisedWebsocket = previous;
      throw error;
    }
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

  /**
   * Stops clients and requests graceful host shutdown before removing the sandbox.
   * Remaining host/broker processes are force-closed as an isolated group after
   * the shutdown bound or host exit.
   */
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
      await Promise.all(
        [...this.#observations].map((connection) => connection.close()),
      );
      this.#observations.clear();
      await this.#nc.close();
    } catch (error) {
      failures.push(error);
    }
    this.#websocketProxy?.stop();
    this.#nativeProxy?.stop();
    try {
      await this.#controlPlane?.stop();
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
