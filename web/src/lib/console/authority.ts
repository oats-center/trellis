/**
 * Pure authority lifecycle for the console shell.
 *
 * This module owns the state machine only: generation counting, operation
 * supersession, and state transitions. It performs no I/O and uses no Svelte
 * state, so the transition logic is directly testable. `authority.svelte.ts`
 * wraps it with reactive state and the Svelte context.
 *
 * Readiness is not a permission cache. It describes whether the shell has a
 * current, usable `Sessions.Me` result for the connected session. Every
 * validation re-enters `checking`, and a stale completion can never restore a
 * previous snapshot. The snapshot is advisory: Trellis authorizes every
 * request, so no route mount or dispatch depends on this state.
 */

import type { apis } from "trellis-web-generated";

import type { Authority } from "../control-panel.ts";

/** Shell states that must not collapse into each other. */
export type AuthorityState =
  | "checking"
  | "ready"
  | "forbidden"
  | "auth-required"
  | "error";

/** Safe failure detail retained for the retry/error surface. */
export type AuthorityFailure = {
  message: string;
  code?: string;
  id?: string;
};

/**
 * Identity of the connection and authenticated principal a snapshot belongs to.
 *
 * Transport connection identity is not user identity: a reconnect changes
 * `connectionId` while the same principal and login session continue. Only a
 * changed principal or login session destroys identity-scoped page state.
 */
export type AuthorityIdentity = {
  /** Authenticated principal, or null before the first usable result. */
  readonly principalId: string | null;
  /** Login session identity, or null for non-login principals. */
  readonly loginSessionId: string | null;
  /** Transport connection identity of the snapshot. */
  readonly connectionId: string | null;
};

/** Profile shape returned by `Sessions.Me`, in its generated form. */
export type AuthorityProfile = apis.auth.SessionsMeOutput["user"] & {};

/** One completed authority validation. */
export type AuthorityLoadResult =
  | {
    readonly ok: true;
    readonly authority: Authority;
    readonly profile: AuthorityProfile | null;
    readonly identity: AuthorityIdentity;
  }
  | {
    readonly ok: false;
    readonly error: unknown;
    readonly identity: AuthorityIdentity;
  };

/** Loads `Sessions.Me` for the current connection. */
export type AuthorityLoader = () => Promise<AuthorityLoadResult>;

/** Classifies a failed validation into a shell state. */
export type AuthorityFailureClassifier = (
  error: unknown,
) => {
  /** Shell state to enter, excluding a usable `ready`. */
  state: Extract<AuthorityState, "auth-required" | "forbidden" | "error">;
  failure: AuthorityFailure;
};

export type AuthoritySnapshot = {
  readonly state: AuthorityState;
  readonly authority: Authority | null;
  readonly profile: AuthorityProfile | null;
  readonly failure: AuthorityFailure | null;
  /**
   * Advances whenever validation restarts, identity changes, or authority is
   * cleared. Pages and intents compare captured generations against this value.
   */
  readonly generation: number;
  readonly identity: AuthorityIdentity;
};

/** Authority that must be empty before any usable result exists. */
const UNKNOWN_IDENTITY: AuthorityIdentity = {
  principalId: null,
  loginSessionId: null,
  connectionId: null,
};

function sameIdentity(
  left: AuthorityIdentity,
  right: AuthorityIdentity,
): boolean {
  return left.principalId === right.principalId &&
    left.loginSessionId === right.loginSessionId;
}

/** True when a snapshot carries authority the shell may act on. */
export function snapshotIsUsable(snapshot: AuthoritySnapshot): boolean {
  return snapshot.state === "ready" && snapshot.authority !== null;
}

/**
 * Owns the authority state machine for one console shell.
 *
 * Call `reload()` after the initial connection, on a usable connection
 * transition, and after a confirmed local action that changes the current
 * session or its authority. `invalidate()` ends the current generation at once
 * so protected reads and unsent intents stop.
 */
export class ConsoleAuthorityCore {
  #loader: AuthorityLoader;
  #classify: AuthorityFailureClassifier;
  #snapshot: AuthoritySnapshot = {
    state: "checking",
    authority: null,
    profile: null,
    failure: null,
    generation: 0,
    identity: UNKNOWN_IDENTITY,
  };
  #operations = 0;
  #disposed = false;

  constructor(
    loader: AuthorityLoader,
    classify: AuthorityFailureClassifier,
  ) {
    this.#loader = loader;
    this.#classify = classify;
  }

  /** Current snapshot. Read `authority.svelte.ts` for a reactive adapter. */
  get snapshot(): AuthoritySnapshot {
    return this.#snapshot;
  }

  get disposed(): boolean {
    return this.#disposed;
  }

  /**
   * True when the outcome of operation `operation` may still be committed.
   * False once a newer validation started, or after disposal.
   */
  isCurrent(operation: number): boolean {
    return !this.#disposed && operation === this.#operations;
  }

  /**
   * Revalidates authority. Every call re-enters `checking` and increments the
   * generation so prior reads and unsent intents stop. A newer call supersedes
   * an in-flight one, so a slow response cannot restore stale authority.
   */
  async reload(): Promise<AuthoritySnapshot> {
    if (this.#disposed) return this.#snapshot;
    const operation = ++this.#operations;
    this.#snapshot = {
      state: "checking",
      authority: null,
      profile: this.#snapshot.profile,
      failure: null,
      generation: this.#snapshot.generation + 1,
      identity: this.#snapshot.identity,
    };
    let result: AuthorityLoadResult;
    try {
      result = await this.#loader();
    } catch (error) {
      result = { ok: false, error, identity: this.#snapshot.identity };
    }
    if (!this.isCurrent(operation)) return this.#snapshot;
    if (result.ok) {
      this.#snapshot = {
        state: "ready",
        authority: result.authority,
        profile: result.profile,
        failure: null,
        generation: this.#snapshot.generation,
        identity: result.identity,
      };
      return this.#snapshot;
    }
    const classified = this.#classify(result.error);
    this.#snapshot = {
      state: classified.state,
      authority: null,
      profile: null,
      failure: classified.failure,
      generation: this.#snapshot.generation,
      identity: result.identity,
    };
    return this.#snapshot;
  }

  /**
   * Ends the current generation immediately without clearing the last known
   * profile. Unsent intents and in-flight reads compare captured generations
   * against the new value. The next `reload()` revalidates.
   */
  invalidate(): void {
    if (this.#disposed) return;
    this.#operations += 1;
    this.#snapshot = {
      state: "checking",
      authority: null,
      profile: this.#snapshot.profile,
      failure: null,
      generation: this.#snapshot.generation + 1,
      identity: this.#snapshot.identity,
    };
  }

  /** Clears all authority after a confirmed sign-out or session revocation. */
  clear(): void {
    if (this.#disposed) return;
    this.#operations += 1;
    this.#snapshot = {
      state: "auth-required",
      authority: null,
      profile: null,
      failure: null,
      generation: this.#snapshot.generation + 1,
      identity: UNKNOWN_IDENTITY,
    };
  }

  /**
   * Ends all pending work without dispatching more RPCs or redirects. A
   * disposed controller commits nothing further.
   */
  dispose(): void {
    this.#disposed = true;
    this.#operations += 1;
  }

  /**
   * True when a fresh result for `identity` must destroy identity-scoped page
   * state because the authenticated principal or login session changed.
   *
   * Only a confirmed change between two known owners is a replacement:
   *
   * - An unknown captured owner adopts the first resolved identity, because the
   *   same shell connection dispatched the request. Otherwise the very first
   *   `Sessions.Me` completion would discard a one-time result the server never
   *   returns again.
   * - An unverifiable result (a transient revalidation failure) is not
   *   confirmed loss either. Definitive session loss leaves the shell in
   *   `auth-required`, which uses the ordinary login/navigation recovery and
   *   destroys the page with its secret output.
   */
  static identityChanged(
    previous: AuthorityIdentity,
    next: AuthorityIdentity,
  ): boolean {
    if (previous.principalId === null || next.principalId === null) {
      return false;
    }
    return !sameIdentity(previous, next);
  }
}
