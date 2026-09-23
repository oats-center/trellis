import { dirname, fromFileUrl, join } from "@std/path";
import type { LocalNatsBootstrapManifest } from "./nats_bootstrap.ts";

/** A directory or reverse-proxy source used by a real test control plane. */
export type TrellisControlPlaneWebSource =
  | { directory: string }
  | { proxy: string };

/** @internal File-backed Trellis control-plane config used by test runtimes. */
export type TrellisControlPlaneConfig = {
  logLevel: string;
  port: number;
  instanceName: string;
  web: {
    origins: string[];
    publicOrigin: string;
    allowInsecureOrigins: string[];
    source?: TrellisControlPlaneWebSource;
    portalSource?: TrellisControlPlaneWebSource;
    consoleSource?: TrellisControlPlaneWebSource;
  };
  httpRateLimit: {
    windowMs: number;
    max: number;
  };
  storage: {
    dbPath: string;
  };
  auth: {
    localIdentity: {
      enabled: boolean;
      passwordPolicy: {
        minLength: number;
      };
      passwordHashing: {
        profile: "default" | "insecure-test-fast";
      };
    };
  };
  ttlMs: {
    sessions: number;
    oauth: number;
    deviceFlow: number;
    pendingAuth: number;
    connections: number;
    natsJwt: number;
  };
  nats: {
    servers: string;
    jetstream: {
      replicas: number;
    };
    system: { credsPath: string };
    trellis: { credsPath: string };
    auth: { credsPath: string };
    authCallout: {
      issuer: { nkey: string; signing: string };
      target: { nkey: string; signing: string };
      sxSeed: string;
    };
  };
  sessionKeySeed: string;
  client: {
    natsServers: string[];
    nativeNatsServers: string[];
  };
  oauth: {
    redirectBase: string;
    alwaysShowProviderChooser: boolean;
    providers: Record<string, TrellisControlPlaneOAuthProvider>;
  };
};

/** Platform TTL settings (milliseconds) for an isolated test control plane. */
export type TrellisControlPlaneTtlMs = TrellisControlPlaneConfig["ttlMs"];

/** Serializable OAuth/OIDC provider config for test control planes. */
export type TrellisControlPlaneOAuthProvider =
  | {
    type: "github";
    clientId: string;
    clientSecret?: string;
    displayName?: string;
  }
  | {
    type: "oidc";
    issuer: string;
    clientId: string;
    clientSecret?: string;
    displayName?: string;
    scopes?: string[];
    /** JSON Pointers selecting verified ID-token role claims. */
    roleClaims?: string[];
    organization?: string;
    logout?: {
      enabled?: boolean;
      endpoint?: string;
      mode?: "oidc" | "auth0";
      allowFederated?: boolean;
    };
  };

function base64url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(
    /=+$/,
    "",
  );
}

/** Generates a random base64url seed for Trellis session-key material. */
export function generateSessionSeed(): string {
  const seed = new Uint8Array(32);
  crypto.getRandomValues(seed);
  return base64url(seed);
}

const heldProcessLocks = new Set<string>();

/** Owned localhost port reservation for a child process. */
export type ReservedPort = {
  /** Reserved TCP port. */
  readonly port: number;
  /** Releases the socket immediately before spawning the child. */
  releaseForSpawn(): void;
  /** Releases the socket and cooperative process lock. */
  release(): void;
};

/** Reserves a localhost TCP port until a child process is ready to bind it. */
export function reserveLocalPort(port = 0): ReservedPort {
  const firstPrivatePort = 49_152;
  const privatePortCount = 16_384;
  const lockRoot = Deno.env.get("TRELLIS_TEST_PORT_LOCK_DIR") ?? "/tmp";
  Deno.mkdirSync(lockRoot, { recursive: true });
  const start = Deno.pid % privatePortCount;
  for (
    let attempt = 0;
    attempt < (port === 0 ? privatePortCount : 1);
    attempt++
  ) {
    const reservedPort = port ||
      firstPrivatePort + (start + attempt) % privatePortCount;
    const lockPath = `${lockRoot}/trellis-test-port-${reservedPort}.lock`;
    const lockFile = tryAcquireProcessLock(lockPath);
    if (lockFile === undefined) {
      if (port !== 0) {
        throw new Error(
          `localhost TCP port ${port} is reserved by another test`,
        );
      }
      continue;
    }
    try {
      const listener = Deno.listen({
        hostname: "127.0.0.1",
        port: reservedPort,
      });
      let socketHeld = true;
      let lockHeld = true;
      return {
        port: reservedPort,
        releaseForSpawn() {
          if (!socketHeld) return;
          socketHeld = false;
          listener.close();
        },
        release() {
          if (socketHeld) {
            socketHeld = false;
            listener.close();
          }
          if (lockHeld) {
            lockHeld = false;
            releaseProcessLock(lockPath, lockFile);
          }
        },
      };
    } catch (error) {
      releaseProcessLock(lockPath, lockFile);
      if (
        port === 0 &&
        (error instanceof Deno.errors.AddrInUse ||
          error instanceof Deno.errors.PermissionDenied)
      ) {
        continue;
      }
      throw error;
    }
  }
  throw new Error("no private localhost TCP port is available");
}

function tryAcquireProcessLock(lockPath: string): Deno.FsFile | undefined {
  if (heldProcessLocks.has(lockPath)) return undefined;
  let file: Deno.FsFile;
  try {
    file = Deno.openSync(lockPath, {
      create: true,
      read: true,
      write: true,
    });
  } catch (error) {
    if (error instanceof Deno.errors.IsADirectory) return undefined;
    throw error;
  }
  if (!file.tryLockSync()) {
    file.close();
    return undefined;
  }
  heldProcessLocks.add(lockPath);
  try {
    file.truncateSync(0);
    file.seekSync(0, Deno.SeekMode.Start);
    file.writeSync(new TextEncoder().encode(`${Deno.pid}\n`));
  } catch (error) {
    releaseProcessLock(lockPath, file);
    throw error;
  }
  return file;
}

function releaseProcessLock(lockPath: string, file: Deno.FsFile): void {
  heldProcessLocks.delete(lockPath);
  try {
    Deno.removeSync(lockPath);
  } catch {
  }
  try {
    file.unlockSync();
  } finally {
    file.close();
  }
}

/** Builds the real Trellis control-plane config for an isolated test runtime. */
export function buildControlPlaneConfig(args: {
  workdir: string;
  natsWorkdir?: string;
  natsUrl: string;
  websocketUrl: string;
  manifest: LocalNatsBootstrapManifest;
  port: number;
  oauthProviders?: Record<string, TrellisControlPlaneOAuthProvider>;
  webOrigins?: readonly string[];
  webSource?: TrellisControlPlaneWebSource;
  portalSource?: TrellisControlPlaneWebSource;
  consoleSource?: TrellisControlPlaneWebSource;
  ttlMs?: Partial<TrellisControlPlaneTtlMs>;
}): TrellisControlPlaneConfig {
  const natsDir = join(args.natsWorkdir ?? args.workdir, "nats");
  const publicOrigin = `http://localhost:${args.port}`;
  return {
    logLevel: "info",
    port: args.port,
    instanceName: "Trellis Test",
    web: {
      origins: [publicOrigin, ...(args.webOrigins ?? [])],
      publicOrigin,
      allowInsecureOrigins: [
        publicOrigin,
        args.websocketUrl,
        ...(args.webOrigins ?? []),
      ],
      source: args.webSource,
      portalSource: args.portalSource,
      consoleSource: args.consoleSource,
    },
    httpRateLimit: { windowMs: 60_000, max: 0 },
    storage: { dbPath: join(args.workdir, "trellis", "trellis.sqlite") },
    auth: {
      localIdentity: {
        enabled: true,
        passwordPolicy: { minLength: 8 },
        passwordHashing: { profile: "insecure-test-fast" },
      },
    },
    ttlMs: {
      sessions: 24 * 60 * 60_000,
      oauth: 5 * 60_000,
      deviceFlow: 30 * 60_000,
      pendingAuth: 5 * 60_000,
      connections: 2 * 60 * 60_000,
      natsJwt: 60 * 60_000,
      ...args.ttlMs,
    },
    nats: {
      servers: args.natsUrl,
      jetstream: { replicas: 1 },
      system: {
        credsPath: join(natsDir, args.manifest.paths.creds.systemService),
      },
      trellis: {
        credsPath: join(natsDir, args.manifest.paths.creds.trellisService),
      },
      auth: { credsPath: join(natsDir, args.manifest.paths.creds.authService) },
      authCallout: {
        issuer: {
          nkey: args.manifest.accounts.auth.publicKey,
          signing: Deno.readTextFileSync(
            join(natsDir, args.manifest.paths.secrets.authIssuerSigning),
          ).trim(),
        },
        target: {
          nkey: args.manifest.accounts.trellis.publicKey,
          signing: Deno.readTextFileSync(
            join(natsDir, args.manifest.paths.secrets.authTargetSigning),
          ).trim(),
        },
        sxSeed: Deno.readTextFileSync(
          join(natsDir, args.manifest.paths.secrets.authCalloutXKey),
        ).trim(),
      },
    },
    sessionKeySeed: generateSessionSeed(),
    client: {
      natsServers: [args.websocketUrl],
      nativeNatsServers: [args.natsUrl],
    },
    oauth: {
      redirectBase: `${publicOrigin}/auth/callback`,
      alwaysShowProviderChooser: false,
      providers: args.oauthProviders ?? {},
    },
  };
}

/** Writes a Trellis control-plane config file and returns its path. */
export async function writeTrellisConfig(args: {
  workdir: string;
  config: TrellisControlPlaneConfig;
  configPath?: string;
}): Promise<string> {
  const configPath = args.configPath ??
    join(args.workdir, "trellis", "config.toml");
  await Deno.mkdir(dirname(configPath), { recursive: true });
  const configDir = dirname(configPath);
  const trustFixture = fromFileUrl(
    new URL("../fixtures/authorization-trust", import.meta.url),
  );
  await Promise.all([
    Deno.writeTextFile(
      join(configDir, "event-session.seed"),
      `${args.config.sessionKeySeed}\n`,
    ),
    Deno.writeTextFile(
      join(configDir, "auth-issuer-signing.seed"),
      `${args.config.nats.authCallout.issuer.signing}\n`,
    ),
    Deno.writeTextFile(
      join(configDir, "auth-target-signing.seed"),
      `${args.config.nats.authCallout.target.signing}\n`,
    ),
    Deno.writeTextFile(
      join(configDir, "auth-sx.seed"),
      `${args.config.nats.authCallout.sxSeed}\n`,
    ),
    Deno.copyFile(
      join(trustFixture, "authorization-root.json"),
      join(configDir, "authorization-root.json"),
    ),
    Deno.copyFile(
      join(trustFixture, "authorization-issuer-manifest.json"),
      join(configDir, "authorization-issuer-manifest.json"),
    ),
    Deno.copyFile(
      join(trustFixture, "authorization-issuer.seed"),
      join(configDir, "authorization-issuer.seed"),
    ).then(() =>
      Deno.chmod(join(configDir, "authorization-issuer.seed"), 0o600)
    ),
  ]);

  const quote = (value: string) => JSON.stringify(value);
  const strings = (values: string[]) => `[${values.map(quote).join(", ")}]`;
  const webSource = (name: string, source?: TrellisControlPlaneWebSource) =>
    source
      ? `${name} = { ${"directory" in source ? "directory" : "proxy"} = ${
        quote("directory" in source ? source.directory : source.proxy)
      } }\n`
      : "";
  const storage = (section: string) => `
[${section}.storage]
kind = "sqlite"
path = ${quote(`${args.config.storage.dbPath}.${section}`)}
journal_mode = "wal"
busy_timeout_ms = 30000
single_writer = true
`;
  let providers = "";
  for (const [id, provider] of Object.entries(args.config.oauth.providers)) {
    providers += `\n[oauth.providers.${quote(id)}]\n`;
    providers += `type = ${quote(provider.type)}\n`;
    if (provider.type === "oidc") {
      providers += `issuer = ${quote(provider.issuer)}\n`;
    }
    providers += `client_id = ${quote(provider.clientId)}\n`;
    if (provider.clientSecret) {
      providers += `client_secret = ${quote(provider.clientSecret)}\n`;
    }
    if (provider.displayName) {
      providers += `display_name = ${quote(provider.displayName)}\n`;
    }
    if (provider.type === "oidc" && provider.scopes) {
      providers += `scopes = ${strings(provider.scopes)}\n`;
    }
    if (provider.type === "oidc" && provider.roleClaims) {
      providers += `role_claims = ${strings(provider.roleClaims)}\n`;
    }
  }

  await Deno.writeTextFile(
    configPath,
    `
instance_name = ${quote(args.config.instanceName)}
event_session_seed_file = "./event-session.seed"
event_context_digest_file = "./event-context.digest"

[http]
port = ${args.config.port}
public_origin = ${quote(args.config.web.publicOrigin)}
origins = ${strings(args.config.web.origins)}
allow_insecure_origins = ${strings(args.config.web.allowInsecureOrigins)}
${webSource("web_source", args.config.web.source)}${
      webSource("portal_source", args.config.web.portalSource)
    }${webSource("console_source", args.config.web.consoleSource)}
rate_limit_max = ${args.config.httpRateLimit.max}
rate_limit_window_ms = ${args.config.httpRateLimit.windowMs}

[nats]
servers = ${quote(args.config.nats.servers)}

[nats.runtime]
auth_creds_path = ${quote(args.config.nats.auth.credsPath)}
trellis_creds_path = ${quote(args.config.nats.trellis.credsPath)}
system_creds_path = ${quote(args.config.nats.system.credsPath)}

[nats.auth_callout]
issuer_signing_seed_file = "./auth-issuer-signing.seed"
target_signing_seed_file = "./auth-target-signing.seed"
xkey_seed_file = "./auth-sx.seed"

[auth.authorization]
issuer_signing_seed_file = "./authorization-issuer.seed"
context_lifetime_seconds = 300
refresh_lead_seconds = 60
refresh_jitter_seconds = 15
minimum_context_lifetime_seconds = 76
maximum_bootstrap_jwt_lifetime_seconds = 3600
allowed_clock_skew_seconds = 30
maximum_context_bytes = 16384
maximum_permissions = 4096
context_bucket = "trellis_authorization_contexts"
registry_replicas = 1

[client]
ws_nats_servers = ${strings(args.config.client.natsServers)}
nats_servers = ${strings(args.config.client.nativeNatsServers)}

[leases]
bucket = "trellis_runtime_leases"
replicas = 1
ttl_ms = 30000
renew_ms = 5000

[auth.local_identity]
enabled = ${args.config.auth.localIdentity.enabled}
password_min_length = ${args.config.auth.localIdentity.passwordPolicy.minLength}

[oauth]
redirect_base = ${quote(args.config.oauth.redirectBase)}
always_show_provider_chooser = ${args.config.oauth.alwaysShowProviderChooser}
${providers}
${storage("platform")}
[platform.ttl_ms]
sessions = ${args.config.ttlMs.sessions}
oauth = ${args.config.ttlMs.oauth}
device_flow = ${args.config.ttlMs.deviceFlow}
pending_auth = ${args.config.ttlMs.pendingAuth}
connections = ${args.config.ttlMs.connections}
nats_jwt = ${args.config.ttlMs.natsJwt}
${storage("jobs")}
[health]
transport_retention_hours = 1
transport_max_bytes = 16777216
${storage("health")}
${storage("events")}
`,
  );
  return configPath;
}
