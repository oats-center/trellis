import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { createInterface } from "node:readline";
import { apps } from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { connectNativeClient } from "../native.ts";

const lines = createInterface({ input: process.stdin });
const message = await lines[Symbol.asyncIterator]().next();
lines.close();
if (message.done) throw new Error("registered caller details required");
const input = JSON.parse(message.value) as {
  issuer: string;
  username: string;
  password: string;
  clientId: string;
};
const directory = await mkdtemp("/tmp/opencode/operation-interop-caller-");
const native = await loginNativeTestClient({
  ...input,
  directory,
  request: apps.NativeOptionalExtend.request,
  approvedCapabilities: [
    ...apps.NativeOptionalExtend.request.requiredCapabilities,
    ...apps.NativeOptionalExtend.request.optionalCapabilities,
  ],
});
const caller = await connectNativeClient({
  ...native,
  request: apps.NativeOptionalExtend.request,
}).orThrow();
try {
  const operation = await caller
    .extend({ value: "Node caller input" })
    .start()
    .orThrow();
  const watch = await operation.live({ updates: true }).orThrow();
  try {
    const events = watch[Symbol.asyncIterator]();
    assert.equal((await events.next()).done, false);
    await operation.signal("Continue", { value: "emit" }).orThrow();
    for (;;) {
      const next = await events.next();
      assert.equal(next.done, false);
      if (next.value.type === "update") {
        assert.equal(next.value.operationId, operation.id);
        assert.equal(next.value.update.value, "Rust provider update");
        assert.equal(next.value.snapshot.id, operation.id);
        break;
      }
    }
    await operation.signal("Continue", { value: "finish" }).orThrow();
    assert.equal(
      (await operation.wait().orThrow()).output?.value,
      "Node caller input",
    );
    console.log(JSON.stringify({ stage: "complete" }));
  } finally {
    await watch.close();
  }
} finally {
  await caller.connection.close();
  await native.session.close();
  await rm(directory, { recursive: true, force: true });
}
