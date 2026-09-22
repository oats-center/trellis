import { equal } from "node:assert/strict";

import {
  type AuthorityFailureClassifier,
  type AuthorityLoadResult,
  ConsoleAuthorityCore,
} from "./authority.ts";
import type { Authority } from "../control-panel.ts";
import type { AuthorityProfile } from "./authority.ts";

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};

const READY_AUTHORITY: Authority = {
  platformPrivileges: ["trellis.auth::admin"],
  grants: { format: "v1", permissions: [] } as Authority["grants"],
};

function ready(principalId: string): AuthorityLoadResult {
  return {
    ok: true,
    authority: READY_AUTHORITY,
    profile: {
      userId: principalId,
      name: principalId,
    } as unknown as AuthorityProfile,
    identity: {
      principalId,
      loginSessionId: `session-${principalId}`,
      connectionId: "connection-1",
    },
  };
}

function failed(
  code: string,
  principalId: string | null = null,
): AuthorityLoadResult {
  return {
    ok: false,
    error: { data: { code, message: code } },
    identity: {
      principalId,
      loginSessionId: principalId === null ? null : `session-${principalId}`,
      connectionId: principalId === null ? null : "connection-1",
    },
  };
}

const classify: AuthorityFailureClassifier = (error) => {
  const code = (error as { data?: { code?: string } })?.data?.code;
  if (code === "session_not_found") {
    return { state: "auth-required", failure: { message: code } };
  }
  if (code === "not_authorized") {
    return { state: "forbidden", failure: { message: code } };
  }
  return { state: "error", failure: { message: code ?? "error" } };
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((onResolve, onReject) => {
    resolve = onResolve;
    reject = onReject;
  });
  return { promise, resolve, reject };
}

Deno.test("U22 revalidation stops readiness immediately", async () => {
  const first = deferred<AuthorityLoadResult>();
  const second = deferred<AuthorityLoadResult>();
  let call = 0;
  const core = new ConsoleAuthorityCore(
    () => (++call === 1 ? first.promise : second.promise),
    classify,
  );

  const firstReload = core.reload();
  first.resolve(ready("usr_a"));
  await firstReload;
  equal(core.snapshot.state, "ready");

  const secondReload = core.reload();
  equal(
    core.snapshot.state,
    "checking",
    "readiness must end as soon as a new validation starts",
  );
  equal(core.snapshot.authority, null);
  second.resolve(ready("usr_a"));
  await secondReload;
  equal(core.snapshot.state, "ready");
});

Deno.test("U23 a stale completion cannot restore authority", async () => {
  const first = deferred<AuthorityLoadResult>();
  const second = deferred<AuthorityLoadResult>();
  let call = 0;
  const core = new ConsoleAuthorityCore(
    () => (++call === 1 ? first.promise : second.promise),
    classify,
  );

  const stale = core.reload();
  const current = core.reload();
  second.resolve(ready("usr_a"));
  await current;
  equal(core.snapshot.state, "ready");

  first.resolve(ready("usr_a"));
  await stale;
  equal(
    core.snapshot.state,
    "ready",
    "the superseded validation must not change the outcome",
  );
  const operationsAfterStale = core.snapshot.generation;
  first.resolve(failed("session_not_found"));
  await stale;
  equal(core.snapshot.generation, operationsAfterStale);
});

Deno.test("U24 invalidation advances the generation and clears readiness", async () => {
  const core = new ConsoleAuthorityCore(
    () => Promise.resolve(ready("usr_a")),
    classify,
  );
  await core.reload();
  const before = core.snapshot.generation;
  core.invalidate();
  equal(core.snapshot.state, "checking");
  equal(core.snapshot.authority, null);
  equal(core.snapshot.generation, before + 1);
  equal(
    core.snapshot.profile?.userId,
    "usr_a",
    "last-known profile may remain for non-actionable display",
  );
});

Deno.test("U25 a transport failure is an error state with Retry, not forbidden", async () => {
  const core = new ConsoleAuthorityCore(
    () => Promise.resolve(failed("internal_error", "usr_a")),
    classify,
  );
  await core.reload();
  equal(core.snapshot.state, "error");
  equal(core.snapshot.failure?.message, "internal_error");
});

Deno.test("U26 an authorization denial is forbidden, and a missing session is auth-required", async () => {
  const forbidden = new ConsoleAuthorityCore(
    () => Promise.resolve(failed("not_authorized", "usr_a")),
    classify,
  );
  await forbidden.reload();
  equal(forbidden.snapshot.state, "forbidden");

  const missing = new ConsoleAuthorityCore(
    () => Promise.resolve(failed("session_not_found")),
    classify,
  );
  await missing.reload();
  equal(missing.snapshot.state, "auth-required");
});

Deno.test("U27 dispose commits nothing further", async () => {
  const pending = deferred<AuthorityLoadResult>();
  const core = new ConsoleAuthorityCore(() => pending.promise, classify);
  const reload = core.reload();
  core.dispose();
  pending.resolve(ready("usr_a"));
  await reload;
  equal(core.snapshot.state, "checking");
  equal(core.snapshot.authority, null);
  equal(core.disposed, true);
});

Deno.test("U28 a thrown loader is classified, not propagated", async () => {
  const core = new ConsoleAuthorityCore(
    () => Promise.reject(new Error("socket closed")),
    classify,
  );
  await core.reload();
  equal(core.snapshot.state, "error");
});

Deno.test("U29 identity change is principal and login-session scoped, not connection scoped", () => {
  const base = {
    principalId: "usr_a",
    loginSessionId: "session-a",
    connectionId: "connection-1",
  };
  equal(
    ConsoleAuthorityCore.identityChanged(base, {
      ...base,
      connectionId: "connection-2",
    }),
    false,
    "a reconnect preserves identity",
  );
  equal(
    ConsoleAuthorityCore.identityChanged(base, {
      ...base,
      loginSessionId: "session-b",
    }),
    true,
  );
  equal(
    ConsoleAuthorityCore.identityChanged(base, {
      principalId: "usr_b",
      loginSessionId: "session-b",
      connectionId: "connection-1",
    }),
    true,
  );
  equal(
    ConsoleAuthorityCore.identityChanged({
      principalId: null,
      loginSessionId: null,
      connectionId: null,
    }, base),
    false,
    "an operation dispatched before the first Me result adopts that result",
  );
  equal(
    ConsoleAuthorityCore.identityChanged(base, {
      principalId: null,
      loginSessionId: null,
      connectionId: null,
    }),
    false,
    "an unverifiable result is not confirmed replacement; auth-required clears through navigation",
  );
});
