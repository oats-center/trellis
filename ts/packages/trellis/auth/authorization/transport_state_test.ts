import assert from "node:assert/strict";
import { test } from "node:test";

import { admittedPolicyCovers } from "./transport_state.ts";
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
