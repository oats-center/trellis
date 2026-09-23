import { LIVE_VERSION } from "./client_open.ts";

/** Identity for one Operation-watch live source. */
export type OperationLiveSource = {
  operationId: string;
  includeUpdates: boolean;
};

/** Observation envelope embedded in an Operation watch control request. */
export type OperationWatchObservationOpen = {
  format: string;
  type: "open";
  openId: string;
  receiveMaxPayloadBytes: number;
};

/** Opening control body published to `{operationSubject}.control`. */
export type OperationWatchOpen = {
  action: "watch";
  operationId: string;
  includeUpdates: boolean;
  observation: OperationWatchObservationOpen;
};

export type OperationWatchSourceSession = {
  emit: (value: unknown) => Promise<void>;
  signal: AbortSignal;
};

/** Parse a live Operation-watch open. A missing observation is a protocol error. */
export function parseOperationWatchOpen(
  value: unknown,
): OperationWatchOpen | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return;
  const record = value as Record<string, unknown>;
  if (record.action !== "watch") return;
  if (
    typeof record.operationId !== "string" || record.operationId.length === 0
  ) {
    return;
  }
  const observation = record.observation;
  if (
    !observation || typeof observation !== "object" ||
    Array.isArray(observation)
  ) {
    return;
  }
  const open = observation as Record<string, unknown>;
  if (open.format !== LIVE_VERSION || open.type !== "open") return;
  if (typeof open.openId !== "string") return;
  if (typeof open.receiveMaxPayloadBytes !== "number") return;
  return {
    action: "watch",
    operationId: record.operationId,
    includeUpdates: record.includeUpdates === true,
    observation: {
      format: LIVE_VERSION,
      type: "open",
      openId: open.openId,
      receiveMaxPayloadBytes: open.receiveMaxPayloadBytes,
    },
  };
}

/**
 * Run a durable Operation watch only after Pulse. Abort is not a business
 * complete, so the provider must not emit a successful END.
 */
export async function runDelayedOperationSource(
  session: OperationWatchSourceSession,
  start: (session: OperationWatchSourceSession) => Promise<void>,
): Promise<void> {
  if (session.signal.aborted) {
    throw new Error("operation watch aborted");
  }
  await start(session);
  if (session.signal.aborted) {
    throw new Error("operation watch aborted");
  }
}
