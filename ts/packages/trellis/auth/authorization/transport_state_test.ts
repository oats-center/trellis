import assert from "node:assert/strict";
import { test } from "node:test";

import {
  admissionContextDigest,
  admittedPolicyCovers,
  TransportAuthorizationState,
  type TransportAuthorizationStatus,
} from "./transport_state.ts";
import {
  TRANSPORT_AUTHORIZATION_FORMAT_V1,
  type TransportAuthorizationV1,
} from "../protocol_wasm.ts";

const policy = (
  over: Partial<TransportAuthorizationV1> = {},
): TransportAuthorizationV1 => ({
  format: TRANSPORT_AUTHORIZATION_FORMAT_V1,
  account: "AACCOUNT",
  publishAllow: [],
  subscribeAllow: [],
  response: null,
  hardExpiresAt: null,
  ...over,
});

test("admission names parse only Trellis callout markers", () => {
  assert.deepEqual(
    admissionContextDigest("trellis.auth.v1:digest-abc:NATSSERVER:42"),
    "digest-abc",
  );
  assert.deepEqual(admissionContextDigest("someone-else"), undefined);
  assert.deepEqual(
    admissionContextDigest("trellis.auth.v1:no-separator"),
    undefined,
  );
  assert.deepEqual(
    admissionContextDigest("trellis.auth.v1:has:server:42"),
    "has",
  );
  assert.deepEqual(
    admissionContextDigest(
      "trellis.auth.v1:opaque-digest:srv-A:18446744073709551615",
    ),
    "opaque-digest",
  );
  for (const malformed of [
    "trellis.auth.v1:digest:",
    "trellis.auth.v1:digest:SERVER",
    "trellis.auth.v1:digest::7",
    "trellis.auth.v1:digest:SERVER:",
    "trellis.auth.v1:digest:SERVER:abc",
    "trellis.auth.v1:digest:SERVER:0",
    "trellis.auth.v1:digest:SERVER:07",
    "trellis.auth.v1:digest:SERVER:+7",
    "trellis.auth.v1:digest:SERVER:-7",
    "trellis.auth.v1:digest:SERVER:7.0",
    "trellis.auth.v1:digest:SERVER:18446744073709551616",
    "trellis.auth.v1:digest:SERVER:7:",
    "trellis.auth.v1:digest:SERVER:7:extra",
    "trellis.auth.v1:di gest:SERVER:7",
    "trellis.auth.v1:digest:SER VER:7",
    "trellis.auth.v1:digest:SER\u{85}VER:7",
    "trellis.auth.v1:digest:SER\u{feff}VER:7",
    "trellis.auth.v1:digest:SERVER:7 ",
    "trellis.auth.v1:digest:SERVER:7\n",
  ]) {
    assert.deepEqual(admissionContextDigest(malformed), undefined, malformed);
  }
});

test("transport status compares admitted A against newest allowed D", async () => {
  const state = new TransportAuthorizationState();
  const seen: TransportAuthorizationStatus[] = [];
  state.onStatusChanged((status) => seen.push(status));

  await state.recordAdmission({
    contextDigest: "d1",
    policy: policy({ publishAllow: ["rpc.v1.Orders.*"] }),
    allowed: policy({ publishAllow: ["rpc.v1.>"] }),
    nowUnixSeconds: 1_000,
  });
  assert.deepEqual(state.status(), "upgrade_available");

  // A duplicate renewal repeats the admitted capability, so it is current-only.
  await state.recompute(policy({ publishAllow: ["rpc.v1.Orders.*"] }), 1_000);
  assert.deepEqual(state.status(), "current");

  // A genuinely narrowed current policy means the admitted socket is dead.
  await state.recompute(policy({ publishAllow: ["other"] }), 1_000);
  assert.deepEqual(state.status(), "unavailable");

  // Only semantic changes are published: unavailable, upgrade, current,
  // unavailable.
  assert.deepEqual(seen, [
    "unavailable",
    "upgrade_available",
    "current",
    "unavailable",
  ]);
});

test("the disconnect transition clears cached admission", async () => {
  const state = new TransportAuthorizationState();
  await state.recordAdmission({
    contextDigest: "d1",
    policy: policy({ publishAllow: ["a"] }),
    allowed: policy({ publishAllow: ["a"] }),
    nowUnixSeconds: 1_000,
  });
  assert.deepEqual(state.status(), "current");

  state.markDisconnected();
  assert.deepEqual(state.status(), "unavailable");
  assert.deepEqual(state.admittedDigest(), undefined);
});

test("admitted policy coverage follows exact NATS wildcard containment", async () => {
  const wide = policy({ publishAllow: ["rpc.v1.>"] });
  assert.deepEqual(
    await admittedPolicyCovers(wide, { publish: ["rpc.v1.Orders.Get"] }, 1_000),
    true,
  );
  const single = policy({ publishAllow: ["a.*"] });
  assert.deepEqual(
    await admittedPolicyCovers(single, { publish: ["a.b"] }, 1_000),
    true,
  );
  // `a.*` matches exactly one token, so it does not cover a nested path.
  assert.deepEqual(
    await admittedPolicyCovers(single, { publish: ["a.b.c"] }, 1_000),
    false,
  );
  // Publish authority does not satisfy a subscribe need.
  assert.deepEqual(
    await admittedPolicyCovers(wide, { subscribe: ["x"] }, 1_000),
    false,
  );
});
