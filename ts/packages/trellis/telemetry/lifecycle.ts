import {
  type MeterProvider,
  metrics,
  type ObservableResult,
} from "@opentelemetry/api";
import {
  recordTrellisDuration,
  type TrellisDurationMetricAttributes,
  type TrellisDurationMetricName,
} from "./metrics.ts";
import { getTrellisMeter, recordCatalogCounter } from "./metrics.ts";

type ConnectionKind = "user" | "service" | "device";
type ConnectionState = "connecting" | "usable" | "suspended" | "terminal";
const connectionStates = new Map<
  symbol,
  { kind: ConnectionKind; state: ConnectionState }
>();
const seenConnectionStates = new Set<string>();
let connectionGaugeProvider: MeterProvider | undefined;

/**
 * Ensures one observable gauge callback per family per provider generation.
 *
 * A process normally keeps one global meter provider for its lifetime; a
 * focused test that installs its own reader must still collect the process
 * local registry, so registration follows the current provider reference
 * instead of latching to whichever provider existed first.
 */
function ensureObservableGauge(
  registered: MeterProvider | undefined,
  name: string,
  unit: string,
  callback: (observer: ObservableResult) => void,
): MeterProvider | undefined {
  const provider = metrics.getMeterProvider();
  if (registered === provider) return registered;
  getTrellisMeter().createObservableGauge(name, { unit }).addCallback(callback);
  return provider;
}

/** Owns one logical connection's numeric telemetry until disposal. */
export function trackConnection(kind: ConnectionKind): {
  transition(state: ConnectionState, reason: string): void;
  dispose(): void;
} {
  connectionGaugeProvider = ensureObservableGauge(
    connectionGaugeProvider,
    "trellis.connection.count",
    "{connection}",
    (observer) => {
      const counts = new Map<string, number>();
      for (const { kind, state } of connectionStates.values()) {
        const key = `${kind}:${state}`;
        counts.set(key, (counts.get(key) ?? 0) + 1);
      }
      for (const key of seenConnectionStates) {
        const [kind, state] = key.split(":");
        observer.observe(counts.get(key) ?? 0, {
          "trellis.participant.kind": kind,
          "trellis.state": state,
        });
      }
    },
  );
  const id = Symbol();
  connectionStates.set(id, { kind, state: "connecting" });
  seenConnectionStates.add(`${kind}:connecting`);
  return {
    transition(state, reason) {
      const current = connectionStates.get(id);
      if (!current || current.state === state) return;
      current.state = state;
      seenConnectionStates.add(`${kind}:${state}`);
      recordCatalogCounter("trellis.connection.transitions", 1, {
        "trellis.participant.kind": kind,
        "trellis.reason": reason,
      });
    },
    dispose() {
      connectionStates.delete(id);
    },
  };
}

const coverageSources = new Set<
  () => { own: boolean; peerCovered: number; peerUnavailable: number }
>();
let coverageGaugeProvider: MeterProvider | undefined;

/** Registers a read-only, synchronously sampled provider coverage source. */
export function trackCoverage(
  source: () => { own: boolean; peerCovered: number; peerUnavailable: number },
): () => void {
  coverageGaugeProvider = ensureObservableGauge(
    coverageGaugeProvider,
    "trellis.auth.coverage.count",
    "{context}",
    (observer) => {
      let own = 0;
      let unavailable = 0;
      let peerCovered = 0;
      let peerUnavailable = 0;
      for (const source of coverageSources) {
        const snapshot = source();
        own += Number(snapshot.own);
        unavailable += Number(!snapshot.own);
        peerCovered += snapshot.peerCovered;
        peerUnavailable += snapshot.peerUnavailable;
      }
      observer.observe(own, {
        "trellis.kind": "own",
        "trellis.state": "covered",
      });
      observer.observe(unavailable, {
        "trellis.kind": "own",
        "trellis.state": "unavailable",
      });
      observer.observe(peerCovered, {
        "trellis.kind": "peer",
        "trellis.state": "covered",
      });
      observer.observe(peerUnavailable, {
        "trellis.kind": "peer",
        "trellis.state": "unavailable",
      });
    },
  );
  coverageSources.add(source);
  return () => coverageSources.delete(source);
}

/** Exactly-once duration observation for one unit of work. */
export interface TelemetryObservation {
  /** Records the observation with one bounded outcome value. */
  finish(outcome: string, extra?: TrellisDurationMetricAttributes): void;
  /** Records the configured cancelled/interrupted outcome once. */
  cancel(): void;
}

/**
 * Starts one duration observation.
 *
 * Callers use `try/finally` so `?`/throw paths cannot silently skip the
 * observation, matching the Rust observation guard boundary.
 */
export function startObservation(
  metric: TrellisDurationMetricName,
  attributes: TrellisDurationMetricAttributes,
  dropOutcome = "cancelled",
): TelemetryObservation {
  const startedAt = performance.now();
  let finished = false;
  const record = (
    outcome: string,
    extra?: TrellisDurationMetricAttributes,
  ): void => {
    if (finished) return;
    finished = true;
    recordTrellisDuration(
      metric,
      performance.now() - startedAt,
      { ...attributes, ...extra, outcome },
    );
  };
  return {
    finish: (outcome, extra) => record(outcome, extra),
    cancel: () => record(dropOutcome),
  };
}

/**
 * Runs one async unit under a duration observation with the same boundary.
 *
 * The original value or exception is always preserved.
 */
export async function withObservation<T>(
  metric: TrellisDurationMetricName,
  attributes: TrellisDurationMetricAttributes,
  run: () => Promise<T>,
  options?: {
    /** Outcome derivation for a successful result; defaults to `ok`. */
    outcomeOf?: (value: T) => string;
    /** Outcome recorded when the unit throws; defaults to `error`. */
    errorOutcome?: string;
    /** Outcome recorded when an abort signal cancels the unit. */
    signal?: AbortSignal;
  },
): Promise<T> {
  const observation = startObservation(metric, attributes);
  try {
    const value = await run();
    if (options?.signal?.aborted) {
      observation.cancel();
    } else {
      observation.finish(options?.outcomeOf?.(value) ?? "ok");
    }
    return value;
  } catch (error) {
    observation.finish(options?.errorOutcome ?? "error");
    throw error;
  }
}
