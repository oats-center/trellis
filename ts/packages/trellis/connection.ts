import type { NatsConnection } from "@nats-io/nats-core";
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
  initialStatus?: TrellisConnectionStatus;
  availability?: TrellisAvailability;
  close?: () => Promise<void>;
  stopObserving?: () => void | Promise<void>;
  log?: LoggerLike | false;
};

/** Options for observing a transport-backed Trellis connection lifecycle. */
export type ObserveTrellisConnectionOptions = {
  kind: TrellisConnectionKind;
  transport: TrellisConnectionStatusTransport;
  transportName?: string;
  log?: LoggerLike | false;
  lifecycleLog?: TrellisConnectionLifecycleLogOptions;
  availability?: TrellisAvailability;
  onTransportEvent?: (event: unknown) => void;
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
  readonly live = new LiveSessionManager();

  /** Creates a Trellis connection lifecycle handle. */
  constructor(options: TrellisConnectionOptions) {
    this.#status = options.initialStatus ??
      createStatus(options.kind, "connected");
    this.#closeTransport = options.close ?? (async () => {});
    this.#stopObserving = options.stopObserving ?? (async () => {});
    this.#log = options.log === false ? noopLogger : options.log ?? noopLogger;
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
      // A diagnostic transport error is not a terminal authority decision:
      // keep the current usable/suspended observation and live ownership.
    } else {
      this.live.suspend();
      if (this.#telemetryUsable) {
        this.#telemetryUsable = false;
        this.#telemetry?.transition("suspended", "disconnect");
      }
    }

    this.#status = status;
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
  reason: "connected" | "coverage_lost" | "resumed" | "refreshed" | "revoked",
): void {
  connection[transitionTelemetry](usable ? "usable" : "suspended", reason);
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
          if (status) {
            logTransportLifecycleEvent(options, event);
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
      return createStatus(kind, "error", transport);
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
