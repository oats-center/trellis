import type { NatsConnection } from "@nats-io/nats-core";
import { AsyncResult } from "@oatscenter/result";
import { TransportRefreshError } from "./errors/TransportRefreshError.ts";
import { logger as noopLogger, type LoggerLike } from "./globals.ts";
import { LiveSessionManager } from "./live/manager.ts";
import { trackConnection } from "./telemetry/lifecycle.ts";

/** Identifies the Trellis runtime that owns a connection. */
export type TrellisConnectionKind = "client" | "device" | "service";

/** Framework-neutral lifecycle phase for an observed Trellis transport. */
export type TrellisConnectionPhase =
  | "connected"
  | "disconnected"
  | "reconnecting"
  | "error"
  | "closed";

/** Diagnostic metadata copied from the underlying transport without exposing raw handles. */
export type TrellisConnectionTransportMetadata = {
  readonly name?: string;
  readonly event?: string;
  readonly server?: string;
  readonly data?: unknown;
  readonly error?: unknown;
};

/** Current framework-neutral connection status. */
export type TrellisConnectionStatus = {
  readonly kind: TrellisConnectionKind;
  readonly phase: TrellisConnectionPhase;
  readonly observedAt: Date;
  /**
   * Whether the newest valid application authorization offers transport
   * capability not yet admitted on the current physical attachment.
   *
   * Only meaningful while `phase` is `connected`; a disconnect clears it and a
   * successful replacement recomputes it from the actual admitted policy.
   */
  readonly transportUpgradeAvailable: boolean;
  readonly transport?: TrellisConnectionTransportMetadata;
};

/** Receives connection status changes. */
export type TrellisConnectionStatusListener = (
  status: TrellisConnectionStatus,
) => void;

/** Narrow transport shape used by Trellis connection lifecycle observation. */
export type TrellisConnectionStatusTransport = {
  status?: () => AsyncIterable<unknown>;
  closed: () => Promise<void | Error>;
  close: () => Promise<void>;
  isClosed?: () => boolean;
  getServer?: () => string | undefined;
};

/** Options for constructing a manually controlled Trellis connection lifecycle. */
export type TrellisConnectionOptions = {
  kind: TrellisConnectionKind;
  initialStatus?: Omit<TrellisConnectionStatus, "transportUpgradeAvailable">;
  availability?: TrellisAvailability;
  close?: () => Promise<void>;
  stopObserving?: () => void | Promise<void>;
  log?: LoggerLike | false;
  /**
   * Explicitly replace the physical attachment when the application asks for a
   * wider admitted policy. Absent on manually controlled handles. @internal
   */
  refreshTransport?: () => AsyncResult<void, TransportRefreshError>;
};

/** Options for observing a transport-backed Trellis connection lifecycle. */
export type ObserveTrellisConnectionOptions = {
  kind: TrellisConnectionKind;
  transport: TrellisConnectionStatusTransport;
  transportName?: string;
  log?: LoggerLike | false;
  lifecycleLog?: TrellisConnectionLifecycleLogOptions;
  availability?: TrellisAvailability;
  /**
   * Receives every raw transport event.
   */
  onTransportEvent?: (event: unknown) => void;
  /** Explicit physical-attachment replacement owned by the connection owner. @internal */
  refreshTransport?: () => AsyncResult<void, TransportRefreshError>;
  /** Process-local handle started before the transport connected. @internal */
  telemetry?: ConnectionTelemetryHandle;
};

/** Options for observing a NATS-backed Trellis connection lifecycle. */
export type ObserveNatsTrellisConnectionOptions = {
  kind: TrellisConnectionKind;
  nc: NatsConnection;
  log?: LoggerLike | false;
  lifecycleLog?: TrellisConnectionLifecycleLogOptions;
  availability?: TrellisAvailability;
  onTransportEvent?: (event: unknown) => void;
  /** Explicit physical-attachment replacement owned by the connection owner. @internal */
  refreshTransport?: () => AsyncResult<void, TransportRefreshError>;
  /** Process-local handle started before the transport connected. @internal */
  telemetry?: ConnectionTelemetryHandle;
};

/** Options for logging transport lifecycle events with Trellis runtime context. */
export type TrellisConnectionLifecycleLogOptions = {
  log: LoggerLike;
  context: Record<string, unknown>;
};

/** Immutable installed availability for optional participant surfaces. */
export type TrellisAvailability = Readonly<{
  capabilities: Readonly<Record<string, boolean>>;
  resources: Readonly<Record<string, boolean>>;
}>;

const EMPTY_AVAILABILITY: TrellisAvailability = Object.freeze({
  capabilities: Object.freeze({}),
  resources: Object.freeze({}),
});
const installAvailability = Symbol("installAvailability");
const attachTelemetry = Symbol("attachTelemetry");
const transitionTelemetry = Symbol("transitionTelemetry");
const terminalTelemetry = Symbol("terminalTelemetry");
const disposeTelemetry = Symbol("disposeTelemetry");
const installTransportBase = Symbol("installTransportBase");
const installTransportUpgrade = Symbol("installTransportUpgrade");

type ConnectionTelemetryHandle = ReturnType<typeof trackConnection>;

function telemetryKind(kind: TrellisConnectionKind):
  | "user"
  | "service"
  | "device" {
  return kind === "client" ? "user" : kind;
}

/**
 * Starts the one process-local numeric handle for a connection attempt.
 *
 * The handle exists from before the transport connects so the process-local
 * registry observes the `connecting` state. The attempt owner adopts it into
 * its `TrellisConnection`, which owns disposal.
 *
 * @internal
 */
export function startConnectionTelemetry(
  kind: TrellisConnectionKind,
): ConnectionTelemetryHandle {
  return trackConnection(telemetryKind(kind));
}

function immutableAvailability(
  availability: TrellisAvailability,
): TrellisAvailability {
  return Object.freeze({
    capabilities: Object.freeze({ ...availability.capabilities }),
    resources: Object.freeze({ ...availability.resources }),
  });
}

/**
 * Framework-neutral Trellis connection lifecycle handle.
 *
 * The class stores the latest transport-neutral status, delivers it immediately
 * to subscribers, and owns lifecycle cleanup without exposing raw transport
 * handles to framework adapters.
 */
export class TrellisConnection {
  #status: TrellisConnectionStatus;
  #listeners = new Set<TrellisConnectionStatusListener>();
  #availability: TrellisAvailability;
  #availabilityListeners = new Set<
    (availability: TrellisAvailability) => void
  >();
  #closeTransport: () => Promise<void>;
  #stopObserving: () => void | Promise<void>;
  #observationStopped: Promise<void> = Promise.resolve();
  #observationFailure: unknown;
  #log: LoggerLike;
  #stopped = false;
  #telemetry?: ReturnType<typeof trackConnection>;
  #telemetryUsable = false;
  #transportBase: TrellisConnectionTransportMetadata = {};
  #transportUpgradeAvailable = false;
  #refreshTransportWork?: () => AsyncResult<void, TransportRefreshError>;
  #refreshInFlight?: AsyncResult<void, TransportRefreshError>;
  readonly live = new LiveSessionManager();

  /** Creates a Trellis connection lifecycle handle. */
  constructor(options: TrellisConnectionOptions) {
    this.#status = options.initialStatus
      ? { ...options.initialStatus, transportUpgradeAvailable: false }
      : createStatus(options.kind, "connected");
    this.#closeTransport = options.close ?? (async () => {});
    this.#stopObserving = options.stopObserving ?? (async () => {});
    this.#log = options.log === false ? noopLogger : options.log ?? noopLogger;
    this.#refreshTransportWork = options.refreshTransport;
    this.#availability = options.availability
      ? immutableAvailability(options.availability)
      : EMPTY_AVAILABILITY;
  }

  /** Returns the latest observed connection status. */
  get status(): TrellisConnectionStatus {
    return this.#status;
  }

  /** Returns the current immutable installed availability snapshot. */
  availability(): TrellisAvailability {
    return this.#availability;
  }

  /** Yields the current availability and each subsequent installed replacement. */
  watchAvailability(): AsyncIterable<TrellisAvailability> {
    const availabilityListeners = this.#availabilityListeners;
    const initialAvailability = this.#availability;
    return {
      async *[Symbol.asyncIterator]() {
        const pending = [initialAvailability];
        let wake: (() => void) | undefined;
        const listener = (availability: TrellisAvailability) => {
          pending.push(availability);
          wake?.();
          wake = undefined;
        };
        availabilityListeners.add(listener);
        try {
          while (true) {
            if (pending.length === 0) {
              await new Promise<void>((resolve) => {
                wake = resolve;
              });
            }
            const next = pending.shift();
            if (next) yield next;
          }
        } finally {
          availabilityListeners.delete(listener);
        }
      },
    };
  }

  [installAvailability](availability: TrellisAvailability): void {
    if (availability === this.#availability) return;
    this.#availability = immutableAvailability(availability);
    for (const listener of this.#availabilityListeners) {
      listener(this.#availability);
    }
  }

  [installTransportBase](base: TrellisConnectionTransportMetadata): void {
    this.#transportBase = base;
  }

  /**
   * Records the retained transport-upgrade observation.
   *
   * The flag is only meaningful while connected: a disconnect clears it, and a
   * successful replacement recomputes it from the actual admitted policy.
   * @internal
   */
  [installTransportUpgrade](available: boolean): void {
    const next = available && this.#status.phase === "connected";
    if (next === this.#transportUpgradeAvailable) return;
    this.#transportUpgradeAvailable = next;
    this.#status = { ...this.#status, transportUpgradeAvailable: next };
    for (const listener of this.#listeners) listener(this.#status);
  }

  /**
   * Explicitly replace the physical NATS attachment to adopt wider authority.
   *
   * Available whether or not an upgrade is currently outstanding. Concurrent
   * calls coalesce onto a single in-flight operation and result; the operation
   * runs under one deadline owned by the connection owner. This is a real
   * reconnect: it publishes ordinary lifecycle transitions rather than hiding
   * the replacement as maintenance.
   */
  refreshTransport(): AsyncResult<void, TransportRefreshError> {
    if (this.#refreshInFlight) return this.#refreshInFlight;
    if (this.#stopped || this.#status.phase === "closed") {
      return AsyncResult.err(TransportRefreshError.connectionClosed());
    }
    const work = this.#refreshTransportWork;
    if (!work) {
      return AsyncResult.err(TransportRefreshError.connectionClosed());
    }
    let inFlight: AsyncResult<void, TransportRefreshError>;
    try {
      inFlight = work();
    } catch (error) {
      return AsyncResult.err(
        error instanceof TransportRefreshError
          ? error
          : TransportRefreshError.fromTransport(error),
      );
    }
    this.#refreshInFlight = inFlight;
    const clear = () => {
      if (this.#refreshInFlight === inFlight) this.#refreshInFlight = undefined;
    };
    inFlight.then(clear, clear);
    return inFlight;
  }

  [attachTelemetry](telemetry?: ConnectionTelemetryHandle): void {
    this.#telemetry = telemetry ??
      trackConnection(telemetryKind(this.#status.kind));
  }

  [transitionTelemetry](state: "usable" | "suspended", reason: string): void {
    this.#telemetryUsable = state === "usable";
    this.#telemetry?.transition(state, reason);
  }

  [terminalTelemetry](): void {
    this.live.stop();
    this.#telemetryUsable = false;
    this.#telemetry?.transition("terminal", "terminal");
  }

  [disposeTelemetry](): void {
    this.#telemetry?.dispose();
    this.#telemetry = undefined;
  }

  /**
   * Subscribes to status changes and immediately delivers the current status.
   *
   * The returned function removes the listener. Calling it more than once is
   * safe.
   */
  subscribe(listener: TrellisConnectionStatusListener): () => void {
    this.#listeners.add(listener);
    listener(this.#status);

    return () => {
      this.#listeners.delete(listener);
    };
  }

  /** Stops status observation without closing the underlying transport. */
  stopObserving(): void {
    if (this.#stopped) {
      return;
    }

    this.#stopped = true;
    this.#observationStopped = Promise.resolve().then(() =>
      this.#stopObserving()
    ).catch((error) => {
      this.#observationFailure = error;
    });
  }

  /** Closes the underlying transport and publishes a terminal closed status. */
  async close(): Promise<void> {
    this.live.stop();
    this.stopObserving();
    try {
      await this.#closeTransport();
      await this.#observationStopped;
      if (this.#observationFailure !== undefined) {
        throw this.#observationFailure;
      }
      this.setStatus(createStatus(this.#status.kind, "closed"));
    } catch (error) {
      this[terminalTelemetry]();
      this.setStatus(createStatus(this.#status.kind, "error", { error }));
      throw error;
    } finally {
      this[disposeTelemetry]();
    }
  }

  /** Publishes a new status to all active listeners. */
  setStatus(status: TrellisConnectionStatus): void {
    if (
      this.#stopped && status.phase !== "closed" && status.phase !== "error"
    ) {
      return;
    }

    if (status.phase === "closed") {
      this[terminalTelemetry]();
    } else if (status.phase === "connected") {
      this.live.resume();
    } else if (status.phase === "error") {
      // A terminal close failure or transport-status watcher failure: record the
      // error phase without publishing a clean close or a suspend transition.
      // Nonterminal raw transport errors never reach here.
    } else {
      this.live.suspend();
      if (this.#telemetryUsable) {
        this.#telemetryUsable = false;
        this.#telemetry?.transition("suspended", "disconnect");
      }
    }

    // Transport-upgrade availability is a retained observation, not a phase.
    // It survives ordinary status changes while connected and is cleared the
    // moment the logical connection leaves the connected phase.
    const transportUpgradeAvailable = status.phase === "connected"
      ? this.#transportUpgradeAvailable
      : false;
    this.#transportUpgradeAvailable = transportUpgradeAvailable;
    this.#status =
      transportUpgradeAvailable === status.transportUpgradeAvailable
        ? status
        : { ...status, transportUpgradeAvailable };
    this.#log.debug(
      {
        kind: status.kind,
        phase: status.phase,
        transport: status.transport,
      },
      "Trellis connection status changed",
    );

    for (const listener of this.#listeners) {
      listener(status);
    }
  }
}

/** @internal Installs one immutable availability snapshot after bootstrap verification. */
export function installConnectionAvailability(
  connection: TrellisConnection,
  availability: TrellisAvailability,
): void {
  connection[installAvailability](availability);
}

/** @internal Records final own-authorization publication or withdrawal. */
export function transitionConnectionAvailability(
  connection: TrellisConnection,
  usable: boolean,
  reason: "connected" | "coverage_lost" | "resumed" | "revoked",
): void {
  connection[transitionTelemetry](usable ? "usable" : "suspended", reason);
}

/**
 * Records whether the current attachment has a deferred transport upgrade.
 *
 * Called by the connection owner after an actual admission read. @internal
 */
export function installConnectionTransportUpgrade(
  connection: TrellisConnection,
  available: boolean,
): void {
  connection[installTransportUpgrade](available);
}

/**
 * Force one ordinary physical NATS reconnect and resolve on the reconnect
 * event.
 *
 * The status iterator is taken before the reconnect request so the event cannot
 * be missed, and the whole wait shares one absolute deadline. This is a real
 * replacement, so callers must let it flow through ordinary lifecycle
 * transitions rather than hiding it as maintenance. @internal
 */
export async function replaceTransportAttachment(
  nc: NatsConnection,
  timeoutMs: number,
): Promise<void> {
  const iterator = nc.status()[Symbol.asyncIterator]();
  try {
    nc.reconnect();
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw TransportRefreshError.timedOut();
      const next = await Promise.race([
        iterator.next(),
        new Promise<never>((_, reject) => {
          setTimeout(
            () => reject(TransportRefreshError.timedOut()),
            remaining,
          );
        }),
      ]);
      if (next.done) break;
      if ((next.value as { type?: unknown } | null)?.type === "reconnect") {
        return;
      }
    }
    throw TransportRefreshError.timedOut();
  } finally {
    await iterator.return?.();
  }
}

/** Observes a narrow transport status stream as a Trellis connection lifecycle. */
export function observeTrellisConnection(
  options: ObserveTrellisConnectionOptions,
): TrellisConnection {
  let stopped = false;
  const statusStream = options.transport.status;
  const statusIterator = typeof statusStream === "function"
    ? statusStream.call(options.transport)[Symbol.asyncIterator]()
    : undefined;
  let statusTask: Promise<void> | undefined;
  let closedFailure: unknown;
  let closedTask = Promise.resolve();
  const connection = new TrellisConnection({
    kind: options.kind,
    availability: options.availability,
    refreshTransport: options.refreshTransport,
    close: async () => {
      const alreadyClosed = options.transport.isClosed?.() ?? false;
      await options.transport.close();
      await statusIterator?.return?.();
      await statusTask;
      await closedTask;
      if (!alreadyClosed && closedFailure !== undefined) throw closedFailure;
    },
    stopObserving: () => {
      stopped = true;
    },
    log: options.log,
  });
  connection[attachTelemetry](options.telemetry);

  const baseTransport = createTransportMetadata(
    options.transport,
    options.transportName,
  );
  connection[installTransportBase](baseTransport);

  if (statusIterator) {
    statusTask = (async () => {
      try {
        while (true) {
          const next = await statusIterator.next();
          if (next.done) return;
          const event = next.value;
          options.onTransportEvent?.(event);
          if (stopped) {
            return;
          }

          const status = statusFromTransportEvent(
            options.kind,
            event,
            baseTransport,
          );
          // A nonterminal transport diagnostic is reported to lifecycle logging
          // and raw transport bookkeeping, but it never moves the logical phase:
          // only a real close transitions the connection to error/closed.
          logTransportLifecycleEvent(options, event);
          if (status) {
            connection.setStatus(status);
          }
        }
      } catch (error) {
        if (!stopped) {
          logTransportStatusWatcherFailure(options, error);
          connection.setStatus(createStatus(options.kind, "error", {
            ...baseTransport,
            error,
          }));
        }
      }
    })();
  }

  closedTask = options.transport.closed().then(
    (closedError) => {
      if (closedError instanceof Error) closedFailure = closedError;
      if (stopped) return;
      if (closedError instanceof Error) {
        logTransportClosed(options, closedError);
        connection[terminalTelemetry]();
        connection.setStatus(createStatus(options.kind, "error", {
          ...baseTransport,
          error: closedError,
        }));
        return;
      }
      logTransportClosed(options);
      connection.setStatus(createStatus(options.kind, "closed", baseTransport));
    },
    (error) => {
      closedFailure = error;
      if (stopped) return;
      logTransportClosed(options, error);
      connection[terminalTelemetry]();
      connection.setStatus(createStatus(options.kind, "error", {
        ...baseTransport,
        error,
      }));
    },
  );

  return connection;
}

/** Observes a NATS connection without exposing the raw NATS handle publicly. */
export function observeNatsTrellisConnection(
  options: ObserveNatsTrellisConnectionOptions,
): TrellisConnection {
  const connection = observeTrellisConnection({
    kind: options.kind,
    transport: options.nc,
    transportName: "nats",
    log: options.log,
    lifecycleLog: options.lifecycleLog,
    availability: options.availability,
    onTransportEvent: options.onTransportEvent,
    refreshTransport: options.refreshTransport,
    telemetry: options.telemetry,
  });
  void options.nc.closed().then(
    () => connection[disposeTelemetry](),
    () => connection[disposeTelemetry](),
  );
  return connection;
}

function lifecycleLabel(kind: TrellisConnectionKind): string {
  switch (kind) {
    case "client":
      return "Client";
    case "device":
      return "Device";
    case "service":
      return "Service";
  }
}

function normalizeTransportError(error: Error): Record<string, unknown> {
  const record = error as Error & {
    operation?: unknown;
    subject?: unknown;
    queue?: unknown;
  };

  return {
    name: error.name,
    message: error.message,
    ...(typeof record.operation === "string"
      ? { operation: record.operation }
      : {}),
    ...(typeof record.subject === "string" ? { subject: record.subject } : {}),
    ...(typeof record.queue === "string" ? { queue: record.queue } : {}),
  };
}

function normalizeTransportStatus(status: unknown): Record<string, unknown> {
  if (!status || typeof status !== "object") {
    return { status };
  }

  const record = status as Record<string, unknown>;
  return {
    ...(typeof record.type === "string" ? { type: record.type } : {}),
    ...(record.error instanceof Error
      ? { error: normalizeTransportError(record.error) }
      : {}),
    ...(typeof record.data === "string" ? { data: record.data } : {}),
    ...(record.data && typeof record.data === "object"
      ? { data: record.data }
      : {}),
  };
}

function getNatsLifecycleLog(kind: TrellisConnectionKind, event: unknown): {
  level: "info" | "warn" | "error";
  message: string;
} | null {
  if (!event || typeof event !== "object") {
    return null;
  }

  const label = lifecycleLabel(kind);
  switch ((event as { type?: unknown }).type) {
    case "disconnect":
      return { level: "warn", message: `${label} disconnected from NATS` };
    case "reconnecting":
      return { level: "warn", message: `${label} attempting NATS reconnect` };
    case "forceReconnect":
      return { level: "warn", message: `${label} forcing NATS reconnect` };
    case "reconnect":
      return { level: "info", message: `${label} reconnected to NATS` };
    case "staleConnection":
      return {
        level: "warn",
        message: `${label} NATS connection became stale`,
      };
    case "error":
      return { level: "error", message: `${label} NATS error` };
    default:
      return null;
  }
}

function logTransportLifecycleEvent(
  options: ObserveTrellisConnectionOptions,
  event: unknown,
): void {
  if (!options.lifecycleLog) {
    return;
  }

  const lifecycleLog = getNatsLifecycleLog(options.kind, event);
  if (!lifecycleLog) {
    return;
  }

  options.lifecycleLog.log[lifecycleLog.level](
    {
      ...options.lifecycleLog.context,
      connection: normalizeTransportStatus(event),
    },
    lifecycleLog.message,
  );
}

function logTransportStatusWatcherFailure(
  options: ObserveTrellisConnectionOptions,
  error: unknown,
): void {
  if (!options.lifecycleLog) {
    return;
  }

  options.lifecycleLog.log.warn(
    { ...options.lifecycleLog.context, error },
    `${lifecycleLabel(options.kind)} NATS status watcher failed`,
  );
}

function logTransportClosed(
  options: ObserveTrellisConnectionOptions,
  error?: Error,
): void {
  if (!options.lifecycleLog) {
    return;
  }

  const label = lifecycleLabel(options.kind);
  if (error) {
    options.lifecycleLog.log.error(
      { ...options.lifecycleLog.context, error },
      `${label} NATS connection closed with error`,
    );
    return;
  }

  options.lifecycleLog.log.warn(
    options.lifecycleLog.context,
    `${label} NATS connection closed`,
  );
}

function createStatus(
  kind: TrellisConnectionKind,
  phase: TrellisConnectionPhase,
  transport?: TrellisConnectionTransportMetadata,
): TrellisConnectionStatus {
  return {
    kind,
    phase,
    observedAt: new Date(),
    transportUpgradeAvailable: false,
    ...(transport ? { transport } : {}),
  };
}

function createTransportMetadata(
  transport: TrellisConnectionStatusTransport,
  name?: string,
): TrellisConnectionTransportMetadata {
  return {
    ...(name ? { name } : {}),
    ...(typeof transport.getServer === "function"
      ? { server: transport.getServer() }
      : {}),
  };
}

function statusFromTransportEvent(
  kind: TrellisConnectionKind,
  event: unknown,
  baseTransport: TrellisConnectionTransportMetadata,
): TrellisConnectionStatus | null {
  const transportEvent = normalizeTransportEvent(event);
  if (!transportEvent.type) {
    return null;
  }

  const transport = {
    ...baseTransport,
    event: transportEvent.type,
    ...(transportEvent.data === undefined ? {} : { data: transportEvent.data }),
    ...(transportEvent.error === undefined
      ? {}
      : { error: transportEvent.error }),
  };

  switch (transportEvent.type) {
    case "disconnect":
    case "disconnected":
      return createStatus(kind, "disconnected", transport);
    case "reconnecting":
    case "forceReconnect":
    case "staleConnection":
      return createStatus(kind, "reconnecting", transport);
    case "reconnect":
      return createStatus(kind, "connected", transport);
    case "error":
      // A raw NATS error event is a diagnostic, not a terminal authority
      // decision or a logical disconnect. It reaches lifecycle logging and the
      // raw transport bookkeeping, while the current logical phase is retained;
      // a terminal failure still arrives through closed().
      return null;
    case "closed":
      return createStatus(kind, "closed", transport);
    default:
      return null;
  }
}

function normalizeTransportEvent(event: unknown): {
  type?: string;
  data?: unknown;
  error?: unknown;
} {
  if (!event || typeof event !== "object") {
    return {};
  }

  const record = event as Record<string, unknown>;
  return {
    ...(typeof record.type === "string" ? { type: record.type } : {}),
    ...("data" in record ? { data: record.data } : {}),
    ...("error" in record ? { error: record.error } : {}),
  };
}
