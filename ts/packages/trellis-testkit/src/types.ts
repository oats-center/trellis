import type { AppRequestDescriptor } from "@oatscenter/trellis";
import type { ConnectedNativeClient } from "@oatscenter/trellis/native";

/** Directory or HTTP(S) proxy used by a production web surface. */
export type TrellisControlPlaneWebSource =
  | { directory: string }
  | {
      proxy: string;
    };

/** Platform flow TTL overrides in milliseconds. */
export type TrellisControlPlaneTtlMs = {
  sessions: number;
  oauth: number;
  deviceFlow: number;
  pendingAuth: number;
};

/** Authorization timing owned by production bootstrap. */
export type TrellisControlPlaneAuthorization = {
  contextLifetimeSeconds: number;
  refreshLeadSeconds: number;
  refreshJitterSeconds: number;
  minimumContextLifetimeSeconds: number;
};

/** Provider input accepted by production bootstrap; secrets travel in private JSON files. */
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
      roleClaims?: string[];
    };

/** Generated participant descriptor accepted by Trellis test admin automation. */
export type TrellisTestParticipantLike = Readonly<{
  kind: "service" | "device";
  identity: string;
  path: string;
  packageEvidence: unknown;
  implements: readonly Readonly<{ identity: string }>[];
  uses: readonly Readonly<{ api: Readonly<{ identity: string }> }>[];
}>;

/** Polling options for `waitFor` and runtime readiness helpers. */
export type WaitForOptions = {
  timeoutMs?: number;
  intervalMs?: number;
};

/** Exact matching production binaries, acquired from release by default. */
export type TrellisNativeSource =
  | { kind: "release" }
  | {
      kind: "path";
      cli: string;
      server: string;
    };

/** Options for the Trellis control-plane started by the test runtime. */
export type TrellisTestRuntimeTrellisOptions = {
  source?: TrellisNativeSource;
  mode?: "all" | "platform" | "jobs" | "health" | "events";
  environment?: {
    inherit?: boolean;
    set?: Record<string, string>;
    unset?: readonly string[];
  };
};

/** Options for starting an isolated Trellis test runtime. */
export type TrellisTestRuntimeStartOptions = {
  keepWorkdir?: boolean;
  deployment?: string;
  /** Existing or desired local test-admin password. */
  adminPassword?: string;
  trellis?: TrellisTestRuntimeTrellisOptions;
  /** Seed via production bootstrap-admin by default; browser-flow exercises first-admin UI. */
  firstAdmin?: "seeded" | "browser-flow";
  /** OAuth/OIDC providers injected into the isolated test control-plane config. */
  oauthProviders?: Record<string, TrellisControlPlaneOAuthProvider>;
  /** Additional exact browser origins allowed by the test runtime. */
  webOrigins?: readonly string[];
  /** Shared built-in web source for the real control plane. */
  webSource?: TrellisControlPlaneWebSource;
  /** Login Portal source overriding the shared web source. */
  portalSource?: TrellisControlPlaneWebSource;
  /** Console source overriding the shared web source. */
  consoleSource?: TrellisControlPlaneWebSource;
  /** Route the advertised browser WebSocket endpoint through a replaceable TCP proxy. */
  rotatableWebsocketProxy?: boolean;
  /**
   * Advertise an interruptible TCP proxy as the native NATS transport so tests
   * can drop and restore the real physical path for native clients.
   */
  interruptibleNativeProxy?: boolean;
  /**
   * Non-loopback host used as the browser public origin and advertised WebSocket
   * host, so the document is an ordinary insecure browser context.
   */
  browserHost?: string;
  /** Platform TTL overrides (milliseconds) for the isolated test control plane. */
  ttlMs?: Partial<TrellisControlPlaneTtlMs>;
  /** Authorization-context lifetime overrides for the isolated test control plane. */
  authorization?: Partial<TrellisControlPlaneAuthorization>;
  timeouts?: {
    startupMs?: number;
    waitForMs?: number;
    shutdownMs?: number;
  };
};

/** Session-key material returned for a registered service. */
export type TrellisTestServiceKey = {
  seed: string;
  deploymentId: string;
  instanceId: string;
  participantId: string;
};

/** Result returned when a participant is installed for a test deployment. */
export type TrellisTestParticipantApproval = {
  participantId: string;
  revision?: bigint;
  deploymentId?: string;
};

/** Contract value accepted by the Trellis test runtime. */
export type TrellisTestParticipant = TrellisTestParticipantLike;

/** Contract value accepted by app/client helpers. */
export type TrellisTestClientParticipant = AppRequestDescriptor &
  Readonly<{ kind: "native" }>;

/** Connected app/client type returned by `TrellisTestRuntime.connectClient`. */
export type TrellisTestConnectedClient<TContract extends AppRequestDescriptor> =
  ConnectedNativeClient<TContract>;
