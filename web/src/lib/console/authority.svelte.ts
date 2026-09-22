/**
 * Reactive console authority owned by the shell and shared with descendants.
 *
 * `authority.ts` owns the state machine; this module owns Svelte reactivity,
 * context publication, and the real `Sessions.Me` call. Descendant pages read
 * the same object so a route can never observe a different authority than the
 * shell that authorized its mount.
 */

import { createContext } from "svelte";
import type { apis } from "trellis-web-generated";

import type { Authority } from "../control-panel.ts";
import type { TrellisConsoleClient } from "../trellis-context.svelte.ts";
import {
  type AuthorityFailure,
  type AuthorityIdentity,
  type AuthorityProfile,
  type AuthoritySnapshot,
  ConsoleAuthorityCore,
} from "./authority.ts";
import { projectConsoleError } from "./display_value.ts";

export type {
  AuthorityFailure,
  AuthorityIdentity,
  AuthoritySnapshot,
  AuthorityState,
} from "./authority.ts";

/** The real `Sessions.Me` call shape; the client is the typed console runtime. */
async function callMe(trellis: TrellisConsoleClient) {
  return await trellis.sessionsMe({}).orThrow();
}

function identityOf(
  me: Awaited<ReturnType<typeof callMe>>,
): AuthorityIdentity {
  return {
    principalId: me.connection.principalId,
    loginSessionId: me.session?.sessionId ?? null,
    connectionId: me.connection.connectionId,
  };
}

function profileOf(
  me: Awaited<ReturnType<typeof callMe>>,
): AuthorityProfile | null {
  return me.user;
}

/**
 * Classifies a failed validation without collapsing states together.
 *
 * A recoverable validation failure is an error with Retry, never a login loop
 * and never forbidden. Only an explicit authorization denial is forbidden.
 */
function classifyFailure(error: unknown): {
  state: "auth-required" | "forbidden" | "error";
  failure: AuthorityFailure;
} {
  const projected = projectConsoleError(error);
  const failure: AuthorityFailure = {
    message: projected.message,
    ...(projected.code === undefined ? {} : { code: projected.code }),
    ...(projected.id === undefined ? {} : { id: projected.id }),
  };
  const code = projected.code;
  if (
    code === "session_not_found" || code === "session_expired" ||
    code === "session_revoked"
  ) {
    return { state: "auth-required", failure };
  }
  if (code === "not_authorized") {
    return { state: "forbidden", failure };
  }
  return { state: "error", failure };
}

/**
 * Reactive authority for one connected console session.
 *
 * Create one per shell, publish it with `setConsoleAuthority`, and read it in
 * descendants with `getConsoleAuthority`. Reloads re-enter `checking` for every
 * validation; a stale completion can never restore a previous snapshot. The
 * snapshot is advisory shell metadata: it does not authorize routes or RPCs.
 */
export class ConsoleAuthority {
  #core: ConsoleAuthorityCore;
  #snapshot = $state.raw<AuthoritySnapshot | null>(null);

  constructor(trellis: TrellisConsoleClient) {
    this.#core = new ConsoleAuthorityCore(
      async () => {
        try {
          const me = await callMe(trellis);
          const authority: Authority = {
            platformPrivileges: me.connection.platformPrivileges,
            grants: me.connection.grants,
          };
          return {
            ok: true,
            authority,
            profile: profileOf(me),
            identity: identityOf(me),
          };
        } catch (error) {
          return {
            ok: false,
            error,
            identity: {
              principalId: null,
              loginSessionId: null,
              connectionId: null,
            },
          };
        }
      },
      classifyFailure,
    );
  }

  /** Current snapshot; reactive when read in effects or markup. */
  get snapshot(): AuthoritySnapshot {
    return this.#snapshot ?? this.#core.snapshot;
  }

  get state(): AuthoritySnapshot["state"] {
    return this.snapshot.state;
  }

  get authority(): Authority | null {
    return this.snapshot.authority;
  }

  get profile(): AuthorityProfile | null {
    return this.snapshot.profile;
  }

  get failure(): AuthorityFailure | null {
    return this.snapshot.failure;
  }

  get identity(): AuthorityIdentity {
    return this.snapshot.identity;
  }

  /** Revalidates authority, re-entering `checking` first. */
  async reload(): Promise<AuthoritySnapshot> {
    const pending = this.#core.reload();
    this.#snapshot = this.#core.snapshot;
    const result = await pending;
    this.#snapshot = this.#core.snapshot;
    return result;
  }

  /** Ends the current generation so reads and unsent intents stop. */
  invalidate(): void {
    this.#core.invalidate();
    this.#snapshot = this.#core.snapshot;
  }

  /** Clears all authority after a confirmed sign-out or revocation. */
  clear(): void {
    this.#core.clear();
    this.#snapshot = this.#core.snapshot;
  }

  /** Ends pending work without dispatching more RPCs or redirects. */
  dispose(): void {
    this.#core.dispose();
  }

  /** True when `operation` may still commit its outcome. */
  isCurrent(operation: number): boolean {
    return this.#core.isCurrent(operation);
  }
}

const [readConsoleAuthority, writeConsoleAuthority] = createContext<
  ConsoleAuthority
>();

/**
 * Publishes the shell-owned authority to descendant pages.
 *
 * Call during shell component initialization, before rendering children.
 */
export function provideConsoleAuthority(authority: ConsoleAuthority): void {
  writeConsoleAuthority(authority);
}

/**
 * Returns the shell-owned authority.
 *
 * Call during component initialization and store the result in a top-level
 * `const`, matching the app-local Trellis context helpers.
 */
export function getConsoleAuthority(): ConsoleAuthority {
  return readConsoleAuthority();
}

/**
 * True when a known authenticated principal or login session was replaced.
 * An unknown captured owner is not a replacement: the first resolved identity
 * belongs to the work this same shell dispatched.
 */
export function authorityIdentityChanged(
  previous: AuthorityIdentity,
  next: AuthorityIdentity,
): boolean {
  return ConsoleAuthorityCore.identityChanged(previous, next);
}
