/**
 * What a service instance is doing right now, joined from what Auth knows of
 * it (connection, pending update, missing settings) and what it last reported
 * in its heartbeat (phase, build, version, health and checks). The CLI's
 * `svc <ID> instances` computes the same states.
 */

/** Where an instance is in its lifecycle. */
export type InstanceState =
  | "offline"
  | "starting"
  | "waitingForApproval"
  | "waitingForSettings"
  | "serving"
  | "stopping";

/** The fields of a listed instance this projection reads. */
export type ListedInstance = {
  readonly connectedAt?: string;
  readonly participantDigest?: string;
  readonly waiting?: {
    readonly update?: {
      readonly updateId: string;
      readonly reviewUrl?: string;
    };
    readonly missingSettings: readonly string[];
    readonly settingsUrl?: string;
  };
};

/** The fields of an instance's latest health report this projection reads. */
export type ReportedInstance = {
  readonly contractDigest: string;
  readonly effectiveStatus: string;
  readonly observedAt: string;
  readonly latestSample: {
    readonly phase?: string;
    readonly checks: readonly ReportedCheck[];
    readonly participant: { readonly version?: string };
    readonly sample: { readonly time: string };
  };
};

/** One check as a heartbeat reports it. */
export type ReportedCheck = {
  readonly name: string;
  readonly status: string;
  readonly summary?: string;
  readonly error?: string;
};

/** One instance's state, health, build and timing. */
export type InstanceStatus = {
  readonly state: InstanceState;
  /**
   * The effective health status and the checks failing in the current
   * heartbeat; absent while a connected instance has not reported on this
   * connection.
   */
  readonly health?: {
    readonly status: string;
    readonly failingChecks: readonly ReportedCheck[];
  };
  readonly build: {
    readonly digest?: string;
    readonly version?: string;
    /** Whether the build is the deployment's installed one. */
    readonly current: boolean;
  };
  readonly connectedAt?: string;
  readonly lastHeartbeat?: string;
  readonly reviewUrl?: string;
  readonly missingSettings: readonly string[];
  readonly settingsUrl?: string;
};

/**
 * A reported phase. A connected instance still reporting `connecting` has not
 * started serving, so it reads as starting; a phase this Console does not know
 * reads as serving.
 */
function reportedPhase(phase: string | undefined): InstanceState {
  return phase === "connecting"
    ? "starting"
    : phase === "stopping"
      ? "stopping"
      : "serving";
}

/**
 * An instance that is not connected is offline. A connected one is in the
 * phase its latest heartbeat reports, when that heartbeat is current and came
 * on this connection. Before its first heartbeat it waits for its update's
 * approval, then for missing settings, and is otherwise starting. Its health
 * comes from that same current heartbeat; a disconnected instance is offline.
 */
export function instanceStatus(
  instance: ListedInstance,
  report: ReportedInstance | undefined,
  currentDigest: string | undefined,
): InstanceStatus {
  const connectedAt = instance.connectedAt;
  const reporting =
    connectedAt !== undefined &&
    report !== undefined &&
    report.effectiveStatus !== "offline" &&
    Date.parse(report.observedAt) >= Date.parse(connectedAt)
      ? report
      : undefined;
  const missingSettings = instance.waiting?.missingSettings ?? [];
  const state: InstanceState =
    connectedAt === undefined
      ? "offline"
      : reporting
        ? reportedPhase(reporting.latestSample.phase)
        : instance.waiting?.update
          ? "waitingForApproval"
          : missingSettings.length > 0
            ? "waitingForSettings"
            : "starting";
  const digest = reporting?.contractDigest ?? instance.participantDigest;
  const health =
    connectedAt === undefined
      ? { status: "offline", failingChecks: [] }
      : reporting
        ? {
            status: reporting.effectiveStatus,
            failingChecks: reporting.latestSample.checks
              .filter((check) => check.status !== "ok")
              .map(({ name, status, summary, error }) => ({
                name,
                status,
                ...(summary === undefined ? {} : { summary }),
                ...(error === undefined ? {} : { error }),
              })),
          }
        : undefined;
  return {
    state,
    health,
    build: {
      digest,
      version: reporting?.latestSample.participant.version,
      current: digest !== undefined && digest === currentDigest,
    },
    connectedAt,
    lastHeartbeat: report?.latestSample.sample.time,
    reviewUrl: instance.waiting?.update?.reviewUrl,
    missingSettings,
    settingsUrl: instance.waiting?.settingsUrl,
  };
}
