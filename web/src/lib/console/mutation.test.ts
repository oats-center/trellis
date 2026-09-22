import { deepEqual, equal } from "node:assert/strict";

import {
  captureIntent,
  classifyMutationError,
  isConflictError,
  isIntentCurrent,
  isUncertainError,
  MutationController,
} from "./mutation.ts";

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};

Deno.test("U06 a captured intent is unaffected by later mutable state", () => {
  const selected = { id: "dep_a", version: 3n };
  const editable = ["one", "two"];
  const intent = captureIntent({
    operation: "deploymentsApply",
    input: {
      deploymentId: selected.id,
      expectedVersion: selected.version,
      capabilities: editable,
    },
    label: "Deploy A",
    targetId: selected.id,
    scope: { routeKey: "/admin/services" },
    idempotencyKey: "key-1",
  });

  // Mutating every source the intent was built from must not change it.
  selected.id = "dep_b";
  selected.version = 99n;
  editable.push("three");

  equal(intent.input.deploymentId, "dep_a");
  equal(intent.input.expectedVersion, 3n);
  deepEqual(intent.input.capabilities, ["one", "two"]);
  equal(intent.targetId, "dep_a");
  equal(intent.idempotencyKey, "key-1");
  equal(Object.isFrozen(intent.input.capabilities), true);
});

Deno.test("U06 a captured intent copies typed bytes instead of sharing them", () => {
  const bytes = new Uint8Array([1, 2, 3]);
  const intent = captureIntent({
    operation: "portalsPut",
    input: { payload: bytes },
    label: "Portal",
    targetId: "portal-a",
    scope: { routeKey: "/admin/portals" },
    idempotencyKey: "key-bytes",
  });
  bytes[0] = 99;
  equal(intent.input.payload[0], 1);
  equal(intent.input.payload instanceof Uint8Array, true);
});

Deno.test("U06 an intent is invalidated by a route change", () => {
  const intent = captureIntent({
    operation: "portalsRemove",
    input: { id: "a" },
    label: "a",
    targetId: "a",
    scope: { routeKey: "/admin/portals" },
    idempotencyKey: "key-2",
  });
  equal(
    isIntentCurrent(intent, { routeKey: "/admin/portals" }),
    true,
  );
  equal(
    isIntentCurrent(intent, { routeKey: "/admin/services" }),
    false,
  );
});

Deno.test("U06 a workflow generation change invalidates the intent", () => {
  const intent = captureIntent({
    operation: "usersCreate",
    input: { username: "a" },
    label: "a",
    targetId: "a",
    scope: {
      routeKey: "/admin/users/new",
      workflowGeneration: 4,
    },
    idempotencyKey: "key-3",
  });
  equal(
    isIntentCurrent(intent, {
      routeKey: "/admin/users/new",
      workflowGeneration: 4,
    }),
    true,
  );
  equal(
    isIntentCurrent(intent, {
      routeKey: "/admin/users/new",
      workflowGeneration: 5,
    }),
    false,
  );
});

Deno.test("N05 a confirmation invalidated by a target change dispatches nothing", async () => {
  const controller = new MutationController<
    { id: string },
    { ok: true }
  >();
  const intent = captureIntent({
    operation: "sessionsRevoke",
    input: { id: "session-a" },
    label: "session-a",
    targetId: "session-a",
    scope: { routeKey: "/admin/sessions/revoke" },
    idempotencyKey: "key-4",
  });
  equal(controller.begin(intent), true);
  equal(controller.state.status, "confirming");

  let dispatched = 0;
  // The route/target changed while the confirmation was open: the page's own
  // ownership predicate reports the intent is no longer current.
  const outcome = await controller.send({
    isStillValid: () => false,
    dispatch: () => {
      dispatched += 1;
      return Promise.resolve({ ok: true } as const);
    },
  });
  equal(outcome, null);
  equal(dispatched, 0, "a stale intent must not reach the transport");
  equal(controller.state.status, "idle");
  equal(controller.state.intent, null);
});

Deno.test("N05 a second operation cannot start while one is active", async () => {
  const controller = new MutationController<{ id: string }, void>();
  const intent = captureIntent({
    operation: "sessionsRevoke",
    input: { id: "session-a" },
    label: "a",
    targetId: "a",
    scope: { routeKey: "/sessions" },
    idempotencyKey: "key-5",
  });
  equal(controller.begin(intent), true);
  equal(
    controller.begin(intent),
    false,
    "a second confirmation must not replace the first",
  );
  controller.cancel();
  equal(controller.begin(intent), true);
  await controller.send({
    isStillValid: () => true,
    dispatch: () => Promise.resolve(),
  });
  equal(controller.busy, false);
  equal(controller.begin(intent), true);
});

Deno.test("U30 resetting a settled one-time intent drops its receipt", async () => {
  const controller = new MutationController<
    { userId: string },
    { completionUrl: string }
  >();
  controller.begin(captureIntent({
    operation: "usersPasswordResetCreate",
    input: { userId: "user-a" },
    label: "user-a",
    targetId: "user-a",
    scope: { routeKey: "user-new" },
  }));
  await controller.send({
    isStillValid: () => true,
    dispatch: () => Promise.resolve({ completionUrl: "one-time-url" }),
  });
  equal(controller.state.outcome?.kind, "succeeded");
  controller.reset();
  equal(controller.state.outcome, null);
  equal(controller.state.intent, null);
});

Deno.test("N05 send dispatches exactly the captured input and key", async () => {
  const controller = new MutationController<
    { id: string; expectedVersion: bigint },
    string
  >();
  const mutable = { id: "version-1", version: 1n };
  const intent = captureIntent({
    operation: "deploymentsEnable",
    input: { id: mutable.id, expectedVersion: mutable.version },
    label: mutable.id,
    targetId: mutable.id,
    scope: { routeKey: "/services" },
    idempotencyKey: "key-6",
  });
  controller.begin(intent);
  mutable.version = 42n;
  mutable.id = "other";

  let seen: { id: string; expectedVersion: bigint } | null = null;
  let seenKey: string | null = null;
  const outcome = await controller.send({
    isStillValid: () => true,
    dispatch: (captured) => {
      seen = captured.input;
      seenKey = captured.idempotencyKey;
      return Promise.resolve("ok");
    },
  });
  deepEqual(seen, { id: "version-1", expectedVersion: 1n });
  equal(seenKey, "key-6");
  deepEqual(outcome, { kind: "succeeded", value: "ok" });
  equal(controller.state.status, "succeeded");
});

Deno.test("N08 an uncertain failure is never reported as definite failure", async () => {
  const controller = new MutationController<{ id: string }, void>();
  const intent = captureIntent({
    operation: "usersCreate",
    input: { id: "a" },
    label: "a",
    targetId: "a",
    scope: { routeKey: "/users/new" },
    idempotencyKey: "key-7",
  });
  controller.begin(intent);
  const outcome = await controller.send({
    isStillValid: () => true,
    dispatch: () =>
      Promise.reject(
        Object.assign(new Error("socket closed"), { name: "TransportError" }),
      ),
  });
  equal(outcome?.kind, "unknown");
  equal(controller.state.status, "unknown");
  equal(
    controller.state.intent?.idempotencyKey,
    "key-7",
    "the original intent and key are retained for explicit reconciliation",
  );
});

Deno.test("N08 a lost response keeps the key and does not auto-retry", async () => {
  let dispatches = 0;
  const controller = new MutationController<{ id: string }, void>();
  const intent = captureIntent({
    operation: "usersCreate",
    input: { id: "a" },
    label: "a",
    targetId: "a",
    scope: { routeKey: "/users/new" },
    idempotencyKey: "key-8",
  });
  controller.begin(intent);
  await controller.send({
    isStillValid: () => true,
    dispatch: () => {
      dispatches += 1;
      return Promise.reject(
        Object.assign(new Error("timeout"), {
          name: "TransportError",
          code: "timeout",
        }),
      );
    },
  });
  equal(dispatches, 1, "an unknown outcome is not automatically resent");
  equal(controller.state.intent?.idempotencyKey, "key-8");
});

Deno.test("U06 conflict and uncertainty are classified from codes, not text", () => {
  equal(isConflictError({ data: { code: "revision_conflict" } }), true);
  equal(isConflictError({ reason: "storage_conflict" }), true);
  equal(isConflictError({ remoteError: { code: "conflict" } }), true);
  equal(isConflictError(new Error("revision_conflict")), false);

  equal(isUncertainError({ name: "TransportError" }), true);
  equal(isUncertainError({ code: "unavailable" }), true);
  equal(isUncertainError({ code: "not_authorized" }), false);

  deepEqual(classifyMutationError({ data: { code: "conflict" } }), {
    kind: "conflict",
    error: { data: { code: "conflict" } },
  });
});

Deno.test("N09 a metadata edit after capture cannot change the request", () => {
  const draft = {
    displayName: "Original",
    entryUrl: "https://original.example",
    providers: ["oidc"],
  };
  const intent = captureIntent({
    operation: "portalsPut",
    input: {
      portalId: "portal-a",
      displayName: draft.displayName,
      entryUrl: draft.entryUrl,
      providers: [...draft.providers],
      expectedVersion: 7n,
    },
    label: "portal-a",
    targetId: "portal-a",
    scope: { routeKey: "/admin/portals" },
    idempotencyKey: "key-9",
  });

  draft.displayName = "Changed after confirmation";
  draft.entryUrl = "https://changed.example";
  draft.providers.push("saml");

  deepEqual(intent.input, {
    portalId: "portal-a",
    displayName: "Original",
    entryUrl: "https://original.example",
    providers: ["oidc"],
    expectedVersion: 7n,
  });
});

Deno.test("V16 repeated references reuse their clone instead of the source", () => {
  const shared = { values: ["initial"] };
  const intent = captureIntent({
    operation: "portalsPut",
    input: { first: shared, second: shared },
    label: "portal-a",
    targetId: "portal-a",
    scope: { routeKey: "/admin/portals" },
    idempotencyKey: "key-shared",
  });

  shared.values.push("changed after confirmation");

  deepEqual(intent.input.first.values, ["initial"]);
  deepEqual(
    intent.input.second.values,
    ["initial"],
    "a repeated reference must reuse the clone, not the mutable source",
  );
  equal(
    intent.input.first,
    intent.input.second,
    "the shared alias stays shared in the captured copy",
  );
  equal(Object.isFrozen(intent.input.first.values), true);
});

Deno.test("V16 repeated nested arrays are copied, not retained", () => {
  const inner = ["a", "b"];
  const intent = captureIntent({
    operation: "portalsPut",
    input: { left: inner, right: inner },
    label: "portal-a",
    targetId: "portal-a",
    scope: { routeKey: "/admin/portals" },
    idempotencyKey: "key-arrays",
  });
  inner.push("c");
  deepEqual(intent.input.left, ["a", "b"]);
  deepEqual(intent.input.right, ["a", "b"]);
});

Deno.test("V16 a cyclic request is rejected before any send", () => {
  const cyclic: Record<string, unknown> = { name: "loop" };
  cyclic.self = cyclic;
  let thrown: unknown = null;
  try {
    captureIntent({
      operation: "portalsPut",
      input: cyclic,
      label: "portal-a",
      targetId: "portal-a",
      scope: { routeKey: "/admin/portals" },
      idempotencyKey: "key-cycle",
    });
  } catch (error) {
    thrown = error;
  }
  equal(thrown instanceof TypeError, true);
  equal(
    (thrown as TypeError | null)?.message.includes("cyclic"),
    true,
  );
});

Deno.test("V16 bigint and byte values survive capture unchanged", () => {
  const bytes = new Uint8Array([7, 8, 9]);
  const intent = captureIntent({
    operation: "portalsPut",
    input: { version: 12345678901234567890n, payload: bytes },
    label: "portal-a",
    targetId: "portal-a",
    scope: { routeKey: "/admin/portals" },
    idempotencyKey: "key-bigint-bytes",
  });
  bytes[0] = 0;
  equal(intent.input.version, 12345678901234567890n);
  deepEqual([...intent.input.payload], [7, 8, 9]);
});

Deno.test("V16 a non-plain request object is rejected before confirmation", () => {
  let thrown: unknown = null;
  try {
    captureIntent({
      operation: "portalsPut",
      input: { when: new Date() },
      label: "portal-a",
      targetId: "portal-a",
      scope: { routeKey: "/admin/portals" },
      idempotencyKey: "key-nonplain",
    });
  } catch (error) {
    thrown = error;
  }
  equal(thrown instanceof TypeError, true);
  equal(
    (thrown as TypeError | null)?.message.includes("non-plain"),
    true,
  );
});
