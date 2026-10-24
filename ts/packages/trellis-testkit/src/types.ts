import type { AppRequestDescriptor } from "@oatscenter/trellis";
import type { ConnectedNativeClient } from "@oatscenter/trellis/native";

/** Authorization timing owned by production bootstrap. */
export type TrellisControlPlaneAuthorization = {
  contextLifetimeSeconds: number;
  /** Signed OAuth admission lifetime for real NATS CONNECT. */
  oauthAdmissionLifetimeSeconds: number;
  /** Allowed skew for real signed authorization contexts. */
  allowedClockSkewSeconds: number;
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
  /** Additional exact browser origins allowed by the test runtime. */
  webOrigins?: readonly string[];
  /** Route the advertised browser WebSocket endpoint through a replaceable TCP proxy. */
  rotatableWebsocketProxy?: boolean;
  /**
   * Advertise an interruptible TCP proxy as the native NATS transport so tests
   * can drop and restore the real physical path for native clients.
   */
  interruptibleNativeProxy?: boolean;
  /** Authorization-context lifetime overrides for the isolated test control plane. */
  authorization?: Partial<TrellisControlPlaneAuthorization>;
  timeouts?: {
    startupMs?: number;
    waitForMs?: number;
    shutdownMs?: number;
  };
};

/** Ordinary user created by this test runtime; credentials remain private to it. */
export type TrellisTestUser = Readonly<{
  principalId: string;
  username: string;
}>;

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
