import { assert } from "@std/assert";
import { TrellisTestRuntime } from "../index.ts";

// Run from the frozen staged artifact. No path/source override: this exercises
// public release pins without importing producer-checkout source.
await using runtime = await TrellisTestRuntime.start();
const username = `release-smoke-${crypto.randomUUID()}`;
const created = await runtime.callAdminRpc("authUsersCreate", {
  email: null,
  image: null,
  name: username,
  username,
  idempotencyKey: crypto.randomUUID(),
});
const reset = await runtime.callAdminRpc("authUsersPasswordResetCreate", {
  returnTarget: null,
  userId: created.user.userId,
  idempotencyKey: crypto.randomUUID(),
});
assert(
  reset.flow.targetPrincipalId === created.user.principalId,
  "the generated password reset did not resolve the newly persisted user",
);
console.log(
  "Exact staged release testkit: acquired pinned native host, seeded admin, real generated RPC persisted a user",
);
