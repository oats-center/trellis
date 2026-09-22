/**
 * Ownership rules for the Events page's detail reads.
 *
 * The page can have one selected event or one selected consumer at a time, and
 * a detail read may complete after the operator has moved on. This controller
 * owns the decision of whether a completed read may still publish: it tracks
 * the semantic list key it belongs to, the selected kind and identity, and a
 * generation that advances whenever the query changes or a new detail starts.
 */

/** Which detail area currently owns the page's detail surface. */
export type DetailKind = "event" | "consumer";

/** One detail read's ownership token, captured before its request starts. */
export type DetailToken = {
  readonly kind: DetailKind;
  readonly id: string;
  readonly listKey: string;
  readonly generation: number;
};

/**
 * Production ownership controller shared by the Events page's event and
 * consumer detail reads.
 */
export class DetailOwnership {
  #listKey = "";
  #generation = 0;
  #kind: DetailKind | null = null;
  #id: string | null = null;

  /** The semantic key this controller currently owns details for. */
  get listKey(): string {
    return this.#listKey;
  }

  /** The selected detail kind, or null when nothing is selected. */
  get kind(): DetailKind | null {
    return this.#kind;
  }

  /** The selected identity, or null when nothing is selected. */
  get selectedId(): string | null {
    return this.#id;
  }

  /**
   * Adopts a new semantic list key. Any selection that belonged to the old key
   * is dropped, and every in-flight detail read stops owning the surface.
   */
  setListKey(listKey: string): void {
    if (listKey === this.#listKey) return;
    this.#listKey = listKey;
    this.invalidate();
  }

  /**
   * Starts a detail read for `id`, replacing any current selection
   * synchronously so the previous detail cannot stay visible while the new one
   * loads. The returned token must be validated before publishing a result.
   */
  begin(kind: DetailKind, id: string): DetailToken {
    this.#generation += 1;
    this.#kind = kind;
    this.#id = id;
    return {
      kind,
      id,
      listKey: this.#listKey,
      generation: this.#generation,
    };
  }

  /**
   * True when `token` still owns the detail surface: the semantic query has
   * not changed, no newer read started, and the same kind and identity remain
   * selected.
   */
  owns(token: DetailToken): boolean {
    return token.generation === this.#generation &&
      token.listKey === this.#listKey &&
      token.kind === this.#kind &&
      token.id === this.#id;
  }

  /**
   * A query change ended every current detail read. The generation advances so
   * a late completion can never publish, and the selection clears so the
   * detail area cannot keep showing data from the previous query.
   */
  invalidate(): void {
    this.#generation += 1;
    this.#kind = null;
    this.#id = null;
  }
}

/** Consumer lifecycle status the console knows how to interpret. */
export type KnownConsumerStatus =
  | "current"
  | "processing"
  | "behind"
  | "saturated"
  | "inactive"
  | "failing"
  | "missing"
  | "orphaned"
  | "unmanaged";

/**
 * A known status, or `unknown:<raw>` when the server reported a status this
 * build does not understand. Unknown values stay visible and sort neutrally
 * rather than collapsing to a known state or a false healthy value.
 */
export type ConsumerStatus = KnownConsumerStatus | `unknown:${string}`;

const CONSUMER_SEVERITY: Record<KnownConsumerStatus, number> = {
  missing: 0,
  saturated: 1,
  inactive: 2,
  failing: 3,
  behind: 4,
  processing: 5,
  current: 6,
  orphaned: 7,
  unmanaged: 8,
};

/** Neutral ordering for a status the console does not recognize. */
export const UNKNOWN_CONSUMER_SEVERITY = 9;

/** The ordering weight for a consumer status, known or unknown. */
export function consumerSeverityOf(status: ConsumerStatus): number {
  if (status.startsWith("unknown:")) return UNKNOWN_CONSUMER_SEVERITY;
  return CONSUMER_SEVERITY[status as KnownConsumerStatus];
}

/**
 * Normalizes a raw server status. Unrecognized values are retained as
 * `unknown:<raw>` so optional unknown data is never converted to a false
 * known value.
 */
export function consumerStatus(value: unknown): ConsumerStatus {
  const text = String(value);
  return Object.hasOwn(CONSUMER_SEVERITY, text)
    ? text as ConsumerStatus
    : `unknown:${text}`;
}

/** Completion of an optional Events read, independent of its value. */
export type OptionalReadState = "pending" | "ready" | "unavailable";

/** Operator-visible count that never turns missing data into a numeric zero. */
export function optionalNumericLabel(
  state: OptionalReadState,
  count: bigint | number | undefined,
): string {
  if (state === "pending") return "…";
  if (state !== "ready") return "Unavailable";
  const value = count === undefined
    ? 0
    : typeof count === "bigint"
    ? Number(count)
    : count;
  return value.toLocaleString();
}

/** Rate copy for a metrics tile; missing data is not "0.0 per minute". */
export function optionalRateDetail(
  state: OptionalReadState,
  total: bigint | number | undefined,
  windowMinutes: number,
): string {
  if (state === "pending") return "Loading this window";
  if (state !== "ready") return "Metrics unavailable";
  const value = total === undefined
    ? 0
    : typeof total === "bigint"
    ? Number(total)
    : total;
  const rate = windowMinutes > 0 ? value / windowMinutes : 0;
  return `${
    rate.toLocaleString(undefined, { maximumFractionDigits: 1 })
  } per minute`;
}

/** Inputs for the Events health ledger; optional reads carry explicit state. */
export type EventsHealthLedgerInput = {
  readonly metricsState: OptionalReadState;
  readonly consumersState: OptionalReadState;
  readonly total?: bigint;
  readonly integrityExceptions?: bigint;
  readonly unresolved?: bigint;
  readonly payloadTotalLabel: string;
  readonly payloadAverageLabel: string;
  readonly windowMinutes: number;
  readonly attentionConsumers: number;
  readonly shownConsumers: number;
  readonly oldestLagValue: string;
  readonly oldestLagDetail: string;
  readonly oldestLagDisabled: boolean;
  readonly focus: string;
  readonly attentionConsumersOnly: boolean;
  readonly oldestLagActive: boolean;
};

export type EventsHealthLedgerItem = {
  readonly id: string;
  readonly label: string;
  readonly value: string | number;
  readonly detail: string;
  readonly tone: "success" | "warning" | "error" | "info";
  readonly active: boolean;
  readonly disabled?: boolean;
};

/**
 * Projects the Events health ledger. A pending or unavailable optional read
 * stays non-numeric; only a successful ready read may render zero.
 */
export function projectEventsHealthLedger(
  input: EventsHealthLedgerInput,
): EventsHealthLedgerItem[] {
  const metrics = input.metricsState;
  const consumers = input.consumersState;
  const exceptionShare = metrics !== "ready"
    ? (metrics === "pending" ? "Loading this window" : "Metrics unavailable")
    : `${
      Number(input.total ?? 0n) > 0
        ? (Number(input.integrityExceptions ?? 0n) /
          Number(input.total ?? 0n) * 100).toFixed(2)
        : "0.00"
    }% of events`;
  const payloadDetail = metrics !== "ready"
    ? (metrics === "pending" ? "Loading this window" : "Metrics unavailable")
    : `${input.payloadAverageLabel} average`;
  return [
    {
      id: "all",
      label: "Event flow",
      value: optionalNumericLabel(metrics, input.total),
      detail: optionalRateDetail(metrics, input.total, input.windowMinutes),
      tone: "info",
      active: input.focus === "all",
      disabled: metrics !== "ready",
    },
    {
      id: "exceptions",
      label: "Integrity exceptions",
      value: optionalNumericLabel(metrics, input.integrityExceptions),
      detail: exceptionShare,
      tone: "error",
      active: input.focus === "exceptions",
      disabled: metrics !== "ready",
    },
    {
      id: "unresolved",
      label: "Unresolved",
      value: optionalNumericLabel(metrics, input.unresolved),
      detail: metrics === "ready"
        ? "owner not resolved"
        : (metrics === "pending"
          ? "Loading this window"
          : "Metrics unavailable"),
      tone: "warning",
      active: input.focus === "unresolved",
      disabled: metrics !== "ready",
    },
    {
      id: "consumers",
      label: "Consumers",
      value: consumers === "ready"
        ? input.attentionConsumers
        : optionalNumericLabel(consumers, undefined),
      detail: consumers === "ready"
        ? `need attention · ${input.shownConsumers} shown`
        : (consumers === "pending"
          ? "Loading consumers"
          : "Consumer health unavailable"),
      tone: "error",
      active: input.attentionConsumersOnly,
      disabled: consumers !== "ready",
    },
    {
      id: "oldest-lag",
      label: "Oldest lag",
      value: consumers === "ready" ? input.oldestLagValue : "Unknown",
      detail: consumers === "ready"
        ? input.oldestLagDetail
        : (consumers === "pending"
          ? "Loading consumers"
          : "Consumer health unavailable"),
      tone: "warning",
      active: input.oldestLagActive,
      disabled: consumers !== "ready" || input.oldestLagDisabled,
    },
    {
      id: "largest",
      label: "Payload",
      value: metrics === "ready"
        ? input.payloadTotalLabel
        : optionalNumericLabel(metrics, undefined),
      detail: payloadDetail,
      tone: "success",
      active: input.focus === "largest",
      disabled: metrics !== "ready",
    },
  ];
}

/**
 * Runs a page follow-up only while the Events view is still mounted.
 * A dispose between a successful mutation and this call drops the refresh.
 */
export async function followUpIfMounted(
  mounted: () => boolean,
  followUp: () => Promise<void>,
): Promise<boolean> {
  if (!mounted()) return false;
  await followUp();
  return true;
}

/**
 * Production dead-letter mutation path used by the Events page.
 * A successful server result after dispose must not start follow-up reads.
 */
export async function runDeadLetterMutation(options: {
  mounted: () => boolean;
  mutate: () => Promise<void>;
  followUp: () => Promise<void>;
  onError: (cause: unknown) => void;
}): Promise<void> {
  try {
    await options.mutate();
    await followUpIfMounted(options.mounted, options.followUp);
  } catch (cause) {
    if (!options.mounted()) return;
    options.onError(cause);
  }
}
