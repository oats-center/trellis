/**
 * Immutable mutation intent and outcome lifecycle.
 *
 * An intent freezes everything a mutation will send at the moment the operator
 * confirms: the exact operation, target, display label, typed-confirmation
 * value, full generated input including the expected version, one idempotency
 * key, copies of all editable values, and the route/workflow generation it was
 * created under. Confirmation and the RPC receive the intent itself, never a
 * mutable page selection read again after an await.
 *
 * Local permission state is deliberately absent: Trellis authorizes every
 * request, so client-side authority never decides whether a valid mutation is
 * dispatched.
 *
 * `MutationController` is the single production lifecycle used by route
 * components. It is deliberately free of Svelte state so its transitions are
 * directly testable; pages observe it through its `onChange` callback.
 */

import { ulid } from "ulid";

/** How a dispatched mutation ended, including the uncertain case. */
export type MutationOutcome<T> =
  | { kind: "succeeded"; value: T }
  | { kind: "failed"; error: unknown }
  | { kind: "conflict"; error: unknown }
  | { kind: "unknown"; error: unknown };

/** Identifies the route/workflow generation an intent was created under. */
export type IntentScope = {
  readonly routeKey: string;
  /** Page-owned workflow generation, so "start another" invalidates late work. */
  readonly workflowGeneration?: number;
};

/** One confirmed, immutable mutation. */
export type MutationIntent<TInput> = {
  /** Exact operation name, for logging and tests. */
  readonly operation: string;
  /** Fully frozen generated input, including expected version. */
  readonly input: TInput;
  /** Idempotency key allocated at capture, reused for every send attempt. */
  readonly idempotencyKey: string;
  /** Operator-facing target label captured before confirmation. */
  readonly label: string;
  /** Canonical target identity captured before confirmation. */
  readonly targetId: string;
  /** Typed-confirmation value the modal required, captured before confirmation. */
  readonly expectedValue?: string;
  readonly scope: IntentScope;
};

/** States a production mutation passes through. */
export type MutationStatus =
  | "idle"
  | "confirming"
  | "sending"
  | "succeeded"
  | "failed"
  | "conflict"
  | "unknown";

/** Observable lifecycle state of one mutation controller. */
export type MutationState<TInput, TOutput> = {
  readonly status: MutationStatus;
  readonly intent: MutationIntent<TInput> | null;
  readonly outcome: MutationOutcome<TOutput> | null;
  /** True while a confirmation is open or a send is in flight. */
  readonly busy: boolean;
};

/** Error codes that mean the caller's expected version is stale. */
const CONFLICT_CODES = new Set([
  "conflict",
  "revision_conflict",
  "storage_conflict",
  "portal_policy_changed",
]);

function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null) return undefined;
  const record = error as Record<string, unknown>;
  if (typeof record.code === "string") return record.code;
  if (typeof record.reason === "string") return record.reason;
  const remote = record.remoteError;
  if (typeof remote === "object" && remote !== null) {
    const remoteCode = (remote as Record<string, unknown>).code;
    if (typeof remoteCode === "string") return remoteCode;
    const remoteReason = (remote as Record<string, unknown>).reason;
    if (typeof remoteReason === "string") return remoteReason;
  }
  const data = record.data;
  if (typeof data === "object" && data !== null) {
    const dataCode = (data as Record<string, unknown>).code;
    if (typeof dataCode === "string") return dataCode;
  }
  return undefined;
}

/** True when an error reports a stale expected version/revision. */
export function isConflictError(error: unknown): boolean {
  const code = errorCode(error);
  return code !== undefined && CONFLICT_CODES.has(code);
}

/**
 * True when a failure means the request may or may not have reached the
 * server. Such outcomes are never reported as definite failure and are never
 * retried automatically.
 */
export function isUncertainError(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  const record = error as Record<string, unknown>;
  if (record.name === "TransportError") return true;
  const code = errorCode(error);
  if (code !== undefined) {
    return code === "timeout" || code === "unavailable" ||
      code === "connection_closed";
  }
  if (record.name === "TypeError") return true;
  return false;
}

/** Classifies a thrown/returned mutation error for console reporting. */
export function classifyMutationError(error: unknown): MutationOutcome<never> {
  if (isConflictError(error)) return { kind: "conflict", error };
  if (isUncertainError(error)) return { kind: "unknown", error };
  return { kind: "failed", error };
}

/**
 * Deep-freezes plain request data.
 *
 * A shallow freeze does not make nested arrays immutable, and JSON-cloning
 * would corrupt `bigint` and typed bytes, so this walks ordinary arrays and
 * records, copies binary buffers, and leaves already-immutable primitives as
 * they are. A Svelte state proxy is cloned by value rather than frozen in
 * place.
 *
 * A repeated non-cyclic reference reuses its clone, so the captured request
 * never retains the mutable source. A cyclic reference or an unsupported
 * non-plain object is rejected here, before any confirmation can send it.
 */
function freezeRequestValue(
  value: unknown,
  clones: Map<object, unknown>,
  active: Set<object>,
): unknown {
  if (value === null) return null;
  const type = typeof value;
  if (type !== "object") return value;
  const object = value as object;
  if (active.has(object)) {
    throw new TypeError("captured request contains a cyclic reference");
  }
  const existing = clones.get(object);
  if (existing !== undefined) return existing;
  if (value instanceof Uint8Array) {
    const copy = new Uint8Array(value);
    clones.set(object, copy);
    return copy;
  }
  if (Array.isArray(value)) {
    active.add(object);
    const copy: unknown[] = value.map((item) =>
      freezeRequestValue(item, clones, active)
    );
    active.delete(object);
    const frozen = Object.freeze(copy);
    clones.set(object, frozen);
    return frozen;
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new TypeError("captured request contains a non-plain object");
  }
  active.add(object);
  const copy: Record<string, unknown> = {};
  for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
    copy[key] = freezeRequestValue(entry, clones, active);
  }
  active.delete(object);
  const frozen = Object.freeze(copy);
  clones.set(object, frozen);
  return frozen;
}

/** Captures immutable request data, copying binary values. */
export function freezeRequest<TInput>(input: TInput): TInput {
  return freezeRequestValue(input, new Map(), new Set()) as TInput;
}

/** Arguments for capturing one confirmed mutation. */
export type CaptureIntentArgs<TInput> = {
  readonly operation: string;
  readonly input: TInput;
  readonly label: string;
  readonly targetId: string;
  readonly expectedValue?: string;
  readonly scope: IntentScope;
  /** Test seam for deterministic keys; production allocates a ULID. */
  readonly idempotencyKey?: string;
};

/**
 * Captures one immutable intent.
 *
 * The returned value is the only thing a later send may use: mutating page
 * state, arrays, or the expected version afterwards cannot change what is
 * dispatched.
 */
export function captureIntent<TInput>(
  args: CaptureIntentArgs<TInput>,
): MutationIntent<TInput> {
  return Object.freeze({
    operation: args.operation,
    input: freezeRequest(args.input),
    idempotencyKey: args.idempotencyKey ?? ulid(),
    label: args.label,
    targetId: args.targetId,
    ...(args.expectedValue === undefined
      ? {}
      : { expectedValue: args.expectedValue }),
    scope: Object.freeze({ ...args.scope }),
  });
}

/**
 * Returns true when an unsent intent is still valid for the live scope.
 *
 * A route or workflow change invalidates an intent before it is sent. This is
 * rechecked after confirmation and before the RPC, because a confirmation can
 * outlive the page state that produced it. Authorization is not checked here:
 * a valid operation is dispatched and Trellis answers with its own denial.
 */
export function isIntentCurrent(
  intent: MutationIntent<unknown>,
  scope: IntentScope,
): boolean {
  if (intent.scope.routeKey !== scope.routeKey) return false;
  if (
    intent.scope.workflowGeneration !== undefined &&
    intent.scope.workflowGeneration !== scope.workflowGeneration
  ) {
    return false;
  }
  return true;
}

/** How a send must be evaluated for one confirmed intent. */
export type SendIntentArgs<TInput, TOutput> = {
  /** Recheck of route/workflow validity immediately before dispatch. */
  readonly isStillValid: (intent: MutationIntent<TInput>) => boolean;
  /** Sends exactly the captured input and idempotency key. */
  readonly dispatch: (intent: MutationIntent<TInput>) => Promise<TOutput>;
  /** Optional observation of a cancelled-before-dispatch result. */
  readonly onCancelled?: (intent: MutationIntent<TInput>) => void;
};

/**
 * Owns one mutation from confirmation through outcome.
 *
 * Only this controller decides whether an outcome may be committed: a route or
 * workflow change before dispatch cancels without sending, and a settled
 * outcome is reported to the caller which decides whether the current page
 * still owns it.
 */
export class MutationController<TInput, TOutput> {
  #state: MutationState<TInput, TOutput> = {
    status: "idle",
    intent: null,
    outcome: null,
    busy: false,
  };
  #onChange?: (state: MutationState<TInput, TOutput>) => void;

  constructor(
    onChange?: (state: MutationState<TInput, TOutput>) => void,
  ) {
    this.#onChange = onChange;
  }

  /** Current lifecycle state. */
  get state(): MutationState<TInput, TOutput> {
    return this.#state;
  }

  /** True while a confirmation is open or a send is in flight. */
  get busy(): boolean {
    return this.#state.busy;
  }

  /** The captured intent, or null when none is pending. */
  get intent(): MutationIntent<TInput> | null {
    return this.#state.intent;
  }

  #commit(state: MutationState<TInput, TOutput>): void {
    this.#state = state;
    this.#onChange?.(state);
  }

  /**
   * Starts confirmation for one intent. Returns false when a confirmation or
   * send is already active, so a second operation cannot begin.
   */
  begin(intent: MutationIntent<TInput>): boolean {
    if (this.#state.busy) return false;
    this.#commit({
      status: "confirming",
      intent,
      outcome: null,
      busy: true,
    });
    return true;
  }

  /** Cancels the open confirmation. A send in flight is not cancellable. */
  cancel(): void {
    if (this.#state.status !== "confirming") return;
    this.#commit({
      status: "idle",
      intent: null,
      outcome: null,
      busy: false,
    });
  }

  /** Clears a settled outcome, returning the controller to idle. */
  reset(): void {
    if (this.#state.busy) return;
    this.#commit({
      status: "idle",
      intent: null,
      outcome: null,
      busy: false,
    });
  }

  /**
   * Sends the confirmed intent.
   *
   * Validity is rechecked immediately before dispatch. A stale intent cancels
   * without sending so a changed target can never be mutated.
   */
  async send(
    args: SendIntentArgs<TInput, TOutput>,
  ): Promise<MutationOutcome<TOutput> | null> {
    const intent = this.#state.intent;
    if (intent === null || this.#state.status !== "confirming") return null;
    if (!args.isStillValid(intent)) {
      this.#commit({
        status: "idle",
        intent: null,
        outcome: null,
        busy: false,
      });
      args.onCancelled?.(intent);
      return null;
    }
    this.#commit({
      status: "sending",
      intent,
      outcome: null,
      busy: true,
    });
    let outcome: MutationOutcome<TOutput>;
    try {
      const value = await args.dispatch(intent);
      outcome = { kind: "succeeded", value };
    } catch (error) {
      outcome = classifyMutationError(error);
    }
    this.#commit({
      status: outcome.kind,
      intent,
      outcome,
      busy: false,
    });
    return outcome;
  }
}
