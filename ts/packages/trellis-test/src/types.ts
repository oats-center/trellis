import type {
  CallerParticipant,
  CallerRuntime,
  ClientAuthContinuation,
  ClientAuthOptions,
  ClientAuthRequiredContext,
} from "@qlever-llc/trellis";

import type {
  TrellisControlPlaneOAuthProvider,
  TrellisControlPlaneTtlMs,
  TrellisControlPlaneWebSource,
} from "./control_plane_config.ts";

/** Generated participant descriptor accepted by Trellis test admin automation. */
export type TrellisTestParticipantLike = Readonly<{
  identity: string;
  path: string;
  packageEvidence: unknown;
}>;

/** Polling options for `waitFor` and runtime readiness helpers. */
export type WaitForOptions = {
  timeoutMs?: number;
  intervalMs?: number;
};

/** Local command override for the spawned Trellis control-plane process. */
export type TrellisTestRuntimeTrellisCommand = {
  cmd: string;
  args: readonly string[];
  env?: Record<string, string>;
  cwd?: string;
};

/** Options for the Trellis control-plane started by the test runtime. */
export type TrellisTestRuntimeTrellisOptions = {
  command: TrellisTestRuntimeTrellisCommand;
};

/** Options for starting an isolated Trellis test runtime. */
export type TrellisTestRuntimeStartOptions = {
  keepWorkdir?: boolean;
  deployment?: string;
  /** Existing or desired local test-admin password. */
  adminPassword?: string;
  trellis: TrellisTestRuntimeTrellisOptions;
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
  /** Platform TTL overrides (milliseconds) for the isolated test control plane. */
  ttlMs?: Partial<TrellisControlPlaneTtlMs>;
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

/** Session-key material returned for a registered app/client participant. */
export type TrellisTestClientKey = {
  seed: string;
  participantId: string;
};

/** Authentication options for connecting a test app/client participant. */
export type TrellisTestClientAuth = {
  auth: ClientAuthOptions;
  onAuthRequired(
    ctx: ClientAuthRequiredContext,
  ): Promise<ClientAuthContinuation>;
};

/** Result returned when a participant is installed for a test deployment. */
export type TrellisTestParticipantApproval = {
  participantId: string;
  installedRevision: bigint;
  deploymentId?: string;
  binding?: Record<string, unknown> | null;
};

/** Result of a deployment apply that may require an explicit consent decision. */
export type TrellisTestParticipantApplyResult =
  | { status: "approved"; approval: TrellisTestParticipantApproval }
  | { status: "approval_required"; pendingId: string };

/** Contract value accepted by the Trellis test runtime. */
export type TrellisTestParticipant = TrellisTestParticipantLike;

/** Contract value accepted by app/client helpers. */
export type TrellisTestClientParticipant = CallerParticipant;

/** Connected app/client type returned by `TrellisTestRuntime.connectClient`. */
export type TrellisTestConnectedClient<TContract extends CallerParticipant> =
  CallerRuntime<TContract>;
