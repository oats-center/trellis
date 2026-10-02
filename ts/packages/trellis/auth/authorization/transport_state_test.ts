import { assertEquals } from "@std/assert";

import {
  admissionContextDigest,
  admittedPolicyCovers,
  readOwnAdmission,
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

Deno.test("admission names parse only Trellis callout markers", () => {
  assertEquals(
    admissionContextDigest("trellis.auth.v1:digest-abc:NATSSERVER:42"),
    "digest-abc",
  );
  assertEquals(admissionContextDigest("someone-else"), undefined);
  assertEquals(
    admissionContextDigest("trellis.auth.v1:no-separator"),
    undefined,
  );
  assertEquals(admissionContextDigest("trellis.auth.v1:has:server:42"), "has");
  assertEquals(
    admissionContextDigest(
      "trellis.auth.v1:opaque-digest:srv-A:18446744073709551615",
    ),
    "opaque-digest",
  );
  for (
    const malformed of [
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
    ]
  ) {
    assertEquals(admissionContextDigest(malformed), undefined, malformed);
  }
});

Deno.test("transport status compares admitted A against newest allowed D", async () => {
  const state = new TransportAuthorizationState();
  const seen: TransportAuthorizationStatus[] = [];
  state.onStatusChanged((status) => seen.push(status));

  await state.recordAdmission({
    contextDigest: "d1",
    policy: policy({ publishAllow: ["rpc.v1.Orders.*"] }),
    allowed: policy({ publishAllow: ["rpc.v1.>"] }),
    nowUnixSeconds: 1_000,
  });
  assertEquals(state.status(), "upgrade_available");

  // A duplicate renewal repeats the admitted capability, so it is current-only.
  await state.recompute(policy({ publishAllow: ["rpc.v1.Orders.*"] }), 1_000);
  assertEquals(state.status(), "current");

  // A genuinely narrowed current policy means the admitted socket is dead.
  await state.recompute(policy({ publishAllow: ["other"] }), 1_000);
  assertEquals(state.status(), "unavailable");

  // Only semantic changes are published: unavailable, upgrade, current,
  // unavailable.
  assertEquals(seen, [
    "unavailable",
    "upgrade_available",
    "current",
    "unavailable",
  ]);
});

Deno.test("a real disconnect clears admitted transport state", async () => {
  const state = new TransportAuthorizationState();
  await state.recordAdmission({
    contextDigest: "d1",
    policy: policy({ publishAllow: ["a"] }),
    allowed: policy({ publishAllow: ["a"] }),
    nowUnixSeconds: 1_000,
  });
  assertEquals(state.status(), "current");

  state.markDisconnected();
  assertEquals(state.status(), "unavailable");
  assertEquals(state.admittedDigest(), undefined);
});

Deno.test("own admission reads tolerate missing and malformed replies", async () => {
  const valid = await readOwnAdmission({
    request: () =>
      Promise.resolve({
        json: <T>() =>
          ({ data: { user: "trellis.auth.v1:digest-xyz:SERVER:7" } }) as T,
      }),
  }, 100);
  assertEquals(valid?.contextDigest, "digest-xyz");

  const missing = await readOwnAdmission({
    request: () => Promise.resolve({ json: <T>() => ({}) as T }),
  }, 100);
  assertEquals(missing, undefined);

  const failing = await readOwnAdmission({
    request: () => Promise.reject(new Error("no responder")),
  }, 100);
  assertEquals(failing, undefined);
});

Deno.test("admitted policy coverage follows exact NATS wildcard containment", async () => {
  const wide = policy({ publishAllow: ["rpc.v1.>"] });
  assertEquals(
    await admittedPolicyCovers(wide, { publish: ["rpc.v1.Orders.Get"] }, 1_000),
    true,
  );
  const single = policy({ publishAllow: ["a.*"] });
  assertEquals(
    await admittedPolicyCovers(single, { publish: ["a.b"] }, 1_000),
    true,
  );
  // `a.*` matches exactly one token, so it does not cover a nested path.
  assertEquals(
    await admittedPolicyCovers(single, { publish: ["a.b.c"] }, 1_000),
    false,
  );
  // Publish authority does not satisfy a subscribe need.
  assertEquals(
    await admittedPolicyCovers(wide, { subscribe: ["x"] }, 1_000),
    false,
  );
});
