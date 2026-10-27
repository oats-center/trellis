import type { ConnectionOptions, NatsConnection } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { ClientOpts } from "@oatscenter/trellis";
import {
  type NativeOAuthSession,
  connectNativeClient,
} from "@oatscenter/trellis/native";
import { recordTrellisDuration } from "@oatscenter/trellis/telemetry";
import { execFile } from "node:child_process";
import { once } from "node:events";
import { chmod, mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import {
  type Server,
  type Socket,
  createConnection,
  createServer,
  isIP,
} from "node:net";
import { connect as tlsConnect, TLSSocket } from "node:tls";
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { ulid } from "ulid";
import { z } from "zod";

import { ADMIN_USERNAME } from "./admin/methods.ts";
import type { AdminRpc, AdminRpcInput } from "./admin/methods.ts";
import {
  approveNativeDeviceCode,
  loginNativeTestClient,
} from "./admin/native_auth.ts";
import { TrellisTestAdminAutomation } from "./admin_client.ts";
import { trustInstallationCa } from "./tls_trust.ts";
import {
  removeStaleMarkedDirectories,
  writeTrellisTestOwnerMarker,
} from "./cleanup.ts";
import {
  resolveNativeDistribution,
  trellisTestCacheDir,
} from "./native_distribution.ts";
import { NativeTransportGate, type SerialWriter } from "./native_gate.ts";
import { NatsFrameParser } from "./nats_wire.ts";
import { type ReservedPort, reserveLocalPort } from "./ports.ts";
import { sqliteMemoryUrl as sqliteMemoryUrlHelper } from "./temp.ts";
import {
  type ResolvedTrellisProcessCommand,
  type TrellisProcessHandle,
  startTrellisProcess,
} from "./trellis_process.ts";
import type {
  TrellisTestClientParticipant,
  TrellisTestConnectedClient,
  TrellisTestParticipantApproval,
  TrellisTestParticipantLike,
  TrellisTestRuntimeStartOptions,
  TrellisTestServiceKey,
  TrellisTestUser,
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
    `--local-nats-ports=${plan.ports.slice(1, 4).join(",")}`,
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
  return await new Promise<Uint8Array>((resolve, reject) => {
    const child = execFile(
      cmd,
      args,
      {
        env: environment,
        encoding: "buffer",
        timeout: timeoutMs,
        killSignal: "SIGKILL",
      },
      (error, stdout, stderr) => {
        if (error) {
          reject(
            new Error(
              `Production bootstrap failed (exit ${error.code}): ${stderr.toString("utf8").slice(-8192)}`,
              { cause: error },
            ),
          );
        } else resolve(stdout);
      },
    );
    child.stdin?.on("error", reject);
    child.stdin?.end(input);
  });
}

const WORKDIR_PREFIX = "trellis-testkit-";
const WORKDIR_OWNER_MARKER = ".trellis-testkit-owner";

/** Serializes writes to one destination so their ordering is preserved. */
class SerializedWriter implements SerialWriter {
  #chain: Promise<void> = Promise.resolve();
  readonly #writer: Socket;

  constructor(stream: Socket) {
    this.#writer = stream;
  }

  write(bytes: Uint8Array): Promise<void> {
    const result = this.#chain.then(
      () =>
        new Promise<void>((resolve, reject) => {
          this.#writer.write(bytes, (error) =>
            error ? reject(error) : resolve(),
          );
        }),
    );
    // Keep a rejected write from poisoning later ordered operations; the caller
    // still observes the failure through the returned promise.
    this.#chain = result.catch(() => undefined);
    return result;
  }
}

/** PEM certificate and key a gated native proxy presents to clients. @internal */
export type NativeProxyTls = { certificate: string; privateKey: string };

/** Reads the bundle's broker certificate and key for a gated native proxy. */
function readBrokerTls(workdir: string): NativeProxyTls {
  const directory = join(workdir, "config", "trellis", "nats", "tls");
  return {
    certificate: readFileSync(join(directory, "nats.crt"), "utf8"),
    privateKey: readFileSync(join(directory, "nats.key"), "utf8"),
  };
}

/**
 * Terminates a NATS connection's TLS in a test proxy so it can read frames.
 *
 * NATS sends `INFO` in plaintext and both sides then upgrade the same socket.
 * The proxy forwards that `INFO`, presents the broker's own certificate to the
 * client, and opens TLS to the broker verified against the process's trusted
 * CAs. Interrupting `signal` or closing either peer abandons the upgrade.
 * @internal
 */
export async function upgradeNatsTls(
  client: Socket,
  upstream: Socket,
  host: string,
  broker: NativeProxyTls,
  interrupted: AbortSignal,
): Promise<[TLSSocket, TLSSocket]> {
  const upgrade = new AbortController();
  const abort = () => upgrade.abort();
  interrupted.addEventListener("abort", abort, { once: true });
  client.once("close", abort);
  upstream.once("close", abort);
  const signal = upgrade.signal;
  client.write(await readProtocolLine(upstream, signal));
  const secureClient = new TLSSocket(client, {
    isServer: true,
    cert: broker.certificate,
    key: broker.privateKey,
  });
  const secureUpstream = tlsConnect({
    socket: upstream,
    host,
    ...(isIP(host) ? {} : { servername: host }),
  });
  for (const socket of [secureClient, secureUpstream])
    socket.on("error", () => socket.destroy());
  await Promise.all([
    once(secureClient, "secure", { signal }),
    once(secureUpstream, "secureConnect", { signal }),
  ]);
  return [secureClient, secureUpstream];
}

/** Reads one CRLF-terminated protocol line without consuming later bytes. */
async function readProtocolLine(
  socket: Socket,
  signal: AbortSignal,
): Promise<Buffer> {
  let buffered = Buffer.alloc(0);
  for (;;) {
    const end = buffered.indexOf("\r\n");
    if (end >= 0) {
      const rest = buffered.subarray(end + 2);
      if (rest.length > 0) socket.unshift(rest);
      return buffered.subarray(0, end + 2);
    }
    const chunk: Buffer | null = socket.read();
    if (chunk !== null) {
      buffered = Buffer.concat([buffered, chunk]);
      continue;
    }
    if (socket.readableEnded || socket.destroyed)
      throw new Error("broker closed before sending INFO");
    await once(socket, "readable", { signal });
  }
}

/** Transparent TCP proxy used to interrupt and observe the native path. @internal */
export class TcpProxy {
  readonly url: string;
  readonly #listener: Server;
  readonly #target: { host: string; port: number };
  readonly #connections = new Set<Socket>();
  readonly #forwarding = new Set<Promise<void>>();
  readonly #gate: NativeTransportGate | undefined;
  readonly #tls: (() => NativeProxyTls) | undefined;
  #interrupted = false;
  #outage = new AbortController();
  #stopping: Promise<void> | undefined;

  private constructor(
    listener: Server,
    target: { host: string; port: number },
    advertisedHost: string,
    scheme: string,
    gate?: NativeTransportGate,
    tls?: () => NativeProxyTls,
  ) {
    this.#listener = listener;
    this.#target = target;
    this.#gate = gate;
    this.#tls = tls;
    const address = listener.address();
    if (!address || typeof address === "string")
      throw new Error("TCP proxy has no address");
    this.url = `${scheme}://${advertisedHost}:${address.port}`;
    listener.on("connection", (client) => {
      client.on("error", () => client.destroy());
      if (this.#interrupted) {
        client.destroy();
        return;
      }
      this.#connections.add(client);
      // A refused upstream is local to this connection; forwarding owns cleanup.
      const forwarding = this.#forward(client).catch(() => undefined);
      this.#forwarding.add(forwarding);
      void forwarding.finally(() => this.#forwarding.delete(forwarding));
    });
  }

  static async start(
    targetUrl: string,
    options: {
      scheme?: string;
      gate?: NativeTransportGate;
      /**
       * Broker certificate the gated proxy presents. NATS upgrades to TLS after
       * a plaintext INFO, so observing frames requires terminating TLS here and
       * re-encrypting to the broker, which is verified against trusted CAs.
       */
      tls?: () => NativeProxyTls;
    } = {},
  ): Promise<TcpProxy> {
    const target = new URL(targetUrl);
    if (
      options.gate !== undefined &&
      options.tls === undefined &&
      target.protocol === "tls:"
    ) {
      // NATS upgrades to TLS after INFO; a gate cannot read encrypted frames
      // and would wait forever for a frame boundary.
      throw new Error(
        "a gated proxy to a TLS broker needs the broker certificate (tls)",
      );
    }
    const listener = createServer();
    await new Promise<void>((resolve, reject) => {
      listener.once("error", reject);
      listener.listen(0, "127.0.0.1", () => {
        listener.removeListener("error", reject);
        resolve();
      });
    });
    return new TcpProxy(
      listener,
      {
        host: target.hostname,
        port: Number(target.port),
      },
      "127.0.0.1",
      options.scheme ?? "wss",
      options.gate,
      options.tls,
    );
  }

  #drop(connection: Socket): void {
    connection.destroy();
  }

  /**
   * Interrupt the path: drop every live connection and refuse new ones while
   * keeping the listener bound, so an ordinary reconnect cannot succeed until
   * {@link restore}. This models a real transport outage rather than a logical
   * event synthesized inside the client.
   */
  interrupt(): void {
    this.#interrupted = true;
    this.#outage.abort();
    for (const connection of [...this.#connections]) {
      this.#drop(connection);
    }
    this.#connections.clear();
  }

  /** Restore the path after {@link interrupt}; new connections forward again. */
  restore(): void {
    if (!this.#interrupted) return;
    this.#outage = new AbortController();
    this.#interrupted = false;
  }

  async #forward(client: Socket): Promise<void> {
    let upstream: Socket | undefined;
    let connectionId: number | undefined;
    const pumps: Promise<void>[] = [];
    try {
      // Check before opening the upstream and again immediately after, so a
      // partition that lands in that interval closes both sides instead of
      // silently forwarding through the outage.
      if (this.#interrupted) return;
      upstream = createConnection({
        ...this.#target,
        signal: this.#outage.signal,
      });
      upstream.on("error", () => upstream?.destroy());
      this.#connections.add(upstream);
      await once(upstream, "connect");
      if (this.#interrupted) return;
      let clientStream: Socket = client;
      let upstreamStream: Socket = upstream;
      if (this.#gate !== undefined && this.#tls !== undefined) {
        const upgraded = await upgradeNatsTls(
          client,
          upstream,
          this.#target.host,
          this.#tls(),
          this.#outage.signal,
        );
        for (const socket of upgraded) this.#connections.add(socket);
        if (this.#interrupted) return;
        [clientStream, upstreamStream] = upgraded;
      }
      const toClient = new SerializedWriter(clientStream);
      const toUpstream = new SerializedWriter(upstreamStream);
      connectionId = this.#gate?.connectionOpened(toClient);
      // Only the gate-enabled path needs protocol framing; without a gate the
      // pump forwards raw bytes so binary payloads are never held or reframed.
      const gating =
        this.#gate !== undefined && connectionId !== undefined
          ? { gate: this.#gate, connectionId }
          : undefined;
      pumps.push(
        this.#pump(clientStream, toUpstream, "c2s", gating),
        this.#pump(upstreamStream, toClient, "s2c", gating),
      );
      await Promise.race(pumps);
    } finally {
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
    reader: AsyncIterable<Uint8Array>,
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
    for await (const value of reader) {
      if (parser === undefined || gating === undefined) {
        await sink.write(value);
        continue;
      }
      for (const frame of parser.push(value)) {
        if (direction === "s2c") {
          if (
            gating.gate.onServerFrame(gating.connectionId, frame.op, frame.raw)
          ) {
            continue;
          }
        } else {
          if (
            gating.gate.onClientFrame(gating.connectionId, frame.op, frame.raw)
          ) {
            continue;
          }
        }
        await sink.write(frame.raw);
      }
    }
  }

  stop(): Promise<void> {
    this.#stopping ??= (async () => {
      this.interrupt();
      await new Promise<void>((resolve, reject) => {
        this.#listener.close((error) => (error ? reject(error) : resolve()));
      });
      await Promise.all(this.#forwarding);
    })();
    return this.#stopping;
  }
}

/** Runs an isolated Trellis control plane and NATS server for integration tests. */
export class TrellisTestRuntime implements AsyncDisposable {
  readonly trellisUrl: string;
  /** Dedicated authorization server origin for real browser and native OAuth. */
  readonly oauthIssuer: string;
  /** Public HTTPS browser origin of the Console listener. */
  readonly publicOrigin: string;
  readonly natsUrl: string;
  /** NATS WebSocket endpoint for exercising the same broker with browser clients. */
  readonly natsWebsocketUrl: string;
  /** PEM certificate of this runtime's generated installation CA. */
  readonly caCertificate: string;
  readonly workdir: string;
  /** Local test-admin username used by the harness bootstrap. */
  readonly adminUsername = ADMIN_USERNAME;
  /** Local test-admin password configured for this runtime. */
  readonly adminPassword: string;
  readonly deployments: {
    create(args: {
      id?: string;
      contract: TrellisTestParticipantLike;
      /** Confirms this deployment may answer an API another one serves. */
      acknowledgeSharedApis?: boolean;
    }): Promise<void>;
  };
  readonly contracts: {
    apply(args: {
      deployment?: string;
      contract: TrellisTestParticipantLike;
      /** Confirms this deployment may answer an API another one serves. */
      acknowledgeSharedApis?: boolean;
    }): Promise<TrellisTestParticipantApproval>;
    install(args: {
      contract: TrellisTestParticipantLike;
    }): Promise<TrellisTestParticipantApproval>;
  };
  readonly services: {
    createInstance(args: {
      deployment?: string;
      name: string;
      contract: TrellisTestParticipantLike;
    }): Promise<TrellisTestServiceKey>;
    provisionInstanceOnly(args: {
      deployment?: string;
    }): Promise<{ seed: string; sessionKey: string }>;
    disableInstance(
      input: import("../trellis/index.js").apis.auth.ServiceInstancesDisableInput,
    ): Promise<
      import("../trellis/index.js").apis.auth.ServiceInstancesDisableOutput
    >;
  };
  readonly devices: {
    provision(
      input: import("../trellis/index.js").apis.auth.DevicesProvisionInput,
    ): Promise<import("../trellis/index.js").apis.auth.DevicesProvisionOutput>;
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
  #nativeSessions = new Set<NativeOAuthSession>();
  #clientRegistrations = new Map<string, Promise<unknown>>();
  #users = new Map<string, { user: TrellisTestUser; password: string }>();
  #stopped = false;

  /**
   * Broker certificate and key, for a gated {@link TcpProxy} that must
   * terminate the broker's TLS to observe NATS frames. @internal
   */
  brokerTls(): NativeProxyTls {
    return readBrokerTls(this.workdir);
  }

  /** Returns the one-time first-administrator bootstrap URL for this runtime. */
  async bootstrapUrl(): Promise<string> {
    return await this.#getBootstrapUrl();
  }

  /** Forwards one validated low-level Auth RPC over the harness admin transport. */
  async callAdminRpc<M extends keyof AdminRpc>(
    method: M,
    input: AdminRpc[M]["input"],
  ): Promise<AdminRpc[M]["output"]> {
    return (await this.#admin.callAdminRpc(
      method,
      input,
    )) as AdminRpc[M]["output"];
  }

  /** Completes the first-administrator bootstrap through the harness automation. */
  async ensureAdmin(): Promise<void> {
    await this.#admin.completeBootstrap();
  }

  private constructor(args: {
    trellisUrl: string;
    oauthIssuer: string;
    publicOrigin: string;
    caCertificate: string;
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
    this.oauthIssuer = args.oauthIssuer;
    this.publicOrigin = args.publicOrigin;
    this.caCertificate = args.caCertificate;
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
      create: ({ id, contract, acknowledgeSharedApis }) =>
        this.#admin.createDeployment({
          deployment: id ?? this.#deployment,
          contract,
          acknowledgeSharedApis,
        }),
    };
    this.contracts = {
      apply: ({ deployment, contract, acknowledgeSharedApis }) =>
        this.#admin.applyParticipant({
          deployment: deployment ?? this.#deployment,
          contract,
          acknowledgeSharedApis,
        }),
      install: ({ contract }) => this.#admin.installParticipant({ contract }),
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
    for (let attempt = 1; ; attempt++) {
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
    const workdir = await mkdtemp(join(tmpdir(), WORKDIR_PREFIX));
    await chmod(workdir, 0o700);
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
      const environment: Record<string, string> = {};
      if (policy?.inherit !== false) {
        for (const [name, value] of Object.entries(process.env)) {
          if (value !== undefined) environment[name] = value;
        }
      }
      for (const name of policy?.unset ?? []) delete environment[name];
      Object.assign(environment, policy?.set);
      for (const [variable, directory] of Object.entries({
        HOME: "home",
        XDG_CONFIG_HOME: "config",
        XDG_DATA_HOME: "data",
        XDG_STATE_HOME: "state",
        XDG_RUNTIME_DIR: "runtime",
      })) {
        const path = join(workdir, directory);
        await mkdir(path, { mode: 0o700 });
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
      for (let index = 0; index < 5; index++) {
        portLeases.push(await reserveLocalPort());
      }
      const [port, nativePort, monitorPort, websocketPort, oauthPort] =
        portLeases.map((lease) => lease.port);
      // Runtimes serve the default generated installation TLS; this process
      // trusts each runtime's CA explicitly once it is generated.
      const natsUrl = `tls://127.0.0.1:${nativePort}`;
      const websocketUrl = `wss://127.0.0.1:${websocketPort}`;
      if (options.rotatableWebsocketProxy) {
        websocketProxy = await TcpProxy.start(websocketUrl);
      }
      if (options.interruptibleNativeProxy) {
        nativeGate = new NativeTransportGate();
        nativeProxy = await TcpProxy.start(natsUrl, {
          scheme: "tls",
          gate: nativeGate,
          tls: () => readBrokerTls(workdir),
        });
      }
      const trellisUrl = `https://localhost:${port}`;
      const publicOrigin = trellisUrl;
      const oauthIssuer = `https://localhost:${oauthPort}`;
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
        ...(options.sameOriginWebsocket
          ? ["--nats-websocket", "relay"]
          : ["--nats-websocket-url", websocketUrl]),
        "--public-origin",
        publicOrigin,
        "--oauth-origin",
        oauthIssuer,
        "--oauth-port",
        String(oauthPort),
        "--bind-address",
        "127.0.0.1",
      ];
      for (const origin of options.webOrigins ?? []) {
        initArgs.push("--extra-origin", origin);
      }
      for (const [flag, value] of [
        [
          "auth-context-lifetime-seconds",
          options.authorization?.contextLifetimeSeconds,
        ],
        [
          "auth-oauth-admission-lifetime-seconds",
          options.authorization?.oauthAdmissionLifetimeSeconds,
        ],
        // Every process of a test runtime shares one clock, so tolerating
        // installation-wide skew only delays what expiry-driven tests observe.
        [
          "auth-allowed-clock-skew-seconds",
          options.authorization?.allowedClockSkewSeconds ?? 2,
        ],
      ] as const)
        if (value !== undefined) initArgs.push(`--${flag}`, String(value));
      const output = await runBootstrapCommand(
        native.cli,
        initArgs,
        environment,
        timeouts.startupMs,
      );
      const bootstrap = z
        .object({
          trellisConfig: z.string(),
          trellisRuntimeCreds: z.string(),
        })
        .parse(JSON.parse(new TextDecoder().decode(output)));
      environment.TRELLIS_CONFIG = bootstrap.trellisConfig;
      const caCertificate = join(
        dirname(bootstrap.trellisConfig),
        "tls",
        "ca.crt",
      );
      trustInstallationCa(caCertificate);
      const adminPassword =
        options.adminPassword ?? `trellis-testkit-${crypto.randomUUID()}`;
      const seeded = (options.firstAdmin ?? "seeded") === "seeded";
      if (seeded) {
        await runBootstrapCommand(
          native.cli,
          [
            "--format",
            "json",
            "init",
            "admin",
            "--config",
            bootstrap.trellisConfig,
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
        ports: [port, nativePort, monitorPort, websocketPort, oauthPort],
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
        authenticator: credsAuthenticator(await readFile(launch.credsPath)),
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
        oauthIssuer,
        adminPassword,
        defaultDeployment: deployment,
        getBootstrapUrl,
        resourceReadyTimeoutMs: timeouts.startupMs,
        bootstrapComplete: seeded,
      });
      return new TrellisTestRuntime({
        oauthIssuer,
        trellisUrl: startedControlPlane.trellisUrl,
        publicOrigin,
        caCertificate,
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
      for (const lease of portLeases) await lease.release();
      await nc?.close().catch(() => undefined);
      await controlPlane?.stop().catch(() => undefined);
      await Promise.allSettled([websocketProxy?.stop(), nativeProxy?.stop()]);
      if (!options.keepWorkdir) {
        await rm(workdir, { recursive: true }).catch(() => undefined);
      }
      throw error;
    }
  }

  /**
   * Registers a service contract and creates a service instance key. Without
   * `deployment`, a service uses the runtime's default deployment unless a
   * different participant already runs there; then it gets its own deployment.
   * Registering the same participant again adds an instance to its deployment.
   */
  async registerService(args: {
    name: string;
    contract: TrellisTestParticipantLike;
    deployment?: string;
    /** Confirms this service may answer an API another one serves. */
    acknowledgeSharedApis?: boolean;
  }): Promise<TrellisTestServiceKey> {
    return await this.#admin.registerService({
      deployment: args.deployment,
      contract: args.contract,
      acknowledgeSharedApis: args.acknowledgeSharedApis,
    });
  }

  /** Create an ordinary account through Auth; its random password is retained only by this runtime. */
  async createUser(args: {
    name: string;
    email?: string;
  }): Promise<TrellisTestUser> {
    const label =
      args.name
        .toLowerCase()
        .replace(/[^a-z0-9]+/gu, "-")
        .replace(/^-+|-+$/gu, "")
        .slice(0, 32) || "user";
    const username = `${label}-${ulid().toLowerCase()}`;
    const password = Buffer.from(
      crypto.getRandomValues(new Uint8Array(32)),
    ).toString("base64url");
    const { user: created } = await this.callAdminRpc("authUsersCreate", {
      username,
      displayName: args.name,
      ...(args.email !== undefined ? { email: args.email } : {}),
      password,
      idempotencyKey: ulid(),
    });
    const user = Object.freeze({
      principalId: created.principalId,
      username,
    });
    this.#users.set(user.principalId, { user, password });
    return user;
  }

  /** Explicit setup only: grant the requested capabilities without borrowing administrator authority. */
  async grantUserCapabilities(args: {
    user: TrellisTestUser;
    capabilityIds: readonly string[];
  }): Promise<void> {
    this.#credentialsFor(args.user);
    await this.#admin.grantClientCapabilities(
      args.user.principalId,
      args.capabilityIds,
    );
  }

  #credentialsFor(user?: TrellisTestUser): {
    username: string;
    password: string;
  } {
    if (user === undefined)
      return { username: ADMIN_USERNAME, password: this.adminPassword };
    const credentials = this.#users.get(user.principalId);
    if (credentials?.user !== user)
      throw new Error("Test user does not belong to this runtime");
    return { username: user.username, password: credentials.password };
  }

  /** Registers one application, then completes genuine per-user Device Code login and explicit consent. */
  async registerClient(args: {
    name: string;
    contract: TrellisTestClientParticipant;
    user?: TrellisTestUser;
    approvedCapabilities?: readonly string[];
  }): Promise<{ session: NativeOAuthSession; servers: readonly string[] }> {
    const credentials = this.#credentialsFor(args.user);
    const clientId = args.contract.id;
    let registration = this.#clientRegistrations.get(clientId);
    if (!registration) {
      registration = this.callAdminRpc("authOAuthClientsPut", {
        clientId,
        kind: "native",
        title: args.name,
        description: "Native integration caller",
        redirectUris: ["http://127.0.0.1/callback"],
        development: true,
        impliedConsent: [],
        expectedRevision: 0n,
        idempotencyKey: ulid(),
      });
      this.#clientRegistrations.set(clientId, registration);
    }
    await registration;
    if (args.user === undefined)
      await this.#admin.prepareClientCapabilities([
        ...args.contract.requiredCapabilities,
        ...args.contract.optionalCapabilities,
      ]);
    const login = await loginNativeTestClient({
      issuer: this.oauthIssuer,
      clientId,
      request: args.contract,
      ...credentials,
      approvedCapabilities: args.approvedCapabilities,
    });
    this.#nativeSessions.add(login.session);
    return login;
  }

  /** Approves a child-owned native credential through real local login and explicit Device Code consent. */
  async authorizeNativeUserCode(args: {
    clientId: string;
    userCode: string;
    contract: TrellisTestClientParticipant;
    user?: TrellisTestUser;
    approvedCapabilities?: readonly string[];
    signal?: AbortSignal;
  }): Promise<void> {
    const credentials = this.#credentialsFor(args.user);
    if (args.user === undefined)
      await this.#admin.prepareClientCapabilities([
        ...args.contract.requiredCapabilities,
        ...args.contract.optionalCapabilities,
      ]);
    await approveNativeDeviceCode({
      issuer: this.oauthIssuer,
      clientId: args.clientId,
      userCode: args.userCode,
      ...credentials,
      approvedCapabilities: args.approvedCapabilities,
      signal: args.signal,
    });
  }

  /** Connects an app/client via generated APIs, waiting for required startup resources. */
  async connectClient<TContract extends TrellisTestClientParticipant>(
    args: ClientOpts & {
      name: string;
      contract: TContract;
      user?: TrellisTestUser;
      approvedCapabilities?: readonly string[];
    },
  ): Promise<TrellisTestConnectedClient<TContract>> {
    const startedAt = performance.now();
    const login = await this.registerClient(args);
    const client = await connectNativeClient({
      ...args,
      ...login,
      request: args.contract,
    }).orThrow();
    this.#clients.add(client);
    recordTrellisDuration(
      "trellis.connect.duration",
      performance.now() - startedAt,
      { participantKind: "client", phase: "total" },
    );
    return client;
  }

  /** Polls until `fn` returns a truthy value. */
  waitFor<T>(
    fn: () =>
      T | null | undefined | false | Promise<T | null | undefined | false>,
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
      authenticator: credsAuthenticator(await readFile(this.#launch.credsPath)),
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
        // After a real process-group shutdown, an inherited native listener can
        // take a moment to release its fixed restart port. Reacquire that
        // exact port rather than changing the test runtime's advertised address.
        const deadline = Date.now() + this.#timeouts.shutdownMs;
        while (true) {
          try {
            portLeases.push(await reserveLocalPort(port));
            break;
          } catch (error) {
            const addressInUse =
              error instanceof Error &&
              "code" in error &&
              error.code === "EADDRINUSE";
            if (!addressInUse || Date.now() >= deadline) throw error;
            await sleep(50);
          }
        }
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
          await readFile(this.#launch.credsPath),
        ),
        timeout: this.#timeouts.startupMs,
      });
    } catch (error) {
      await this.#controlPlane?.stop();
      throw error;
    } finally {
      for (const lease of portLeases) await lease.release();
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
    if (this.#websocketProxy === undefined) {
      throw new Error("Runtime was not started with rotatableWebsocketProxy");
    }
    const retired = this.#websocketProxy;
    const replacement = await TcpProxy.start(this.#directWebsocket);
    const previous = this.#launch.advertisedWebsocket;
    this.#launch.advertisedWebsocket = replacement.url;
    try {
      await this.restart();
    } catch (error) {
      await replacement.stop();
      this.#launch.advertisedWebsocket = previous;
      throw error;
    }
    this.#websocketProxy = replacement;
    await retired.stop();
    return [retired.url, replacement.url];
  }

  /** Returns a service-owned SQLite path under this runtime workdir. */
  async tempSqlitePath(name = "test.sqlite"): Promise<string> {
    const dir = join(this.workdir, "sqlite");
    await mkdir(dir, { recursive: true });
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
    for (const session of this.#nativeSessions) {
      try {
        await session.close();
      } catch (error) {
        failures.push(error);
      }
    }
    this.#nativeSessions.clear();
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
    for (const result of await Promise.allSettled([
      this.#websocketProxy?.stop(),
      this.#nativeProxy?.stop(),
    ])) {
      if (result.status === "rejected") failures.push(result.reason);
    }
    try {
      await this.#controlPlane?.stop();
    } catch (error) {
      failures.push(error);
    }
    if (this.#ownsWorkdir && !this.#keepWorkdir) {
      try {
        await rm(this.workdir, { recursive: true, force: true });
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
