import assert from "node:assert/strict";
import { isErr } from "@oatscenter/result";
import { createInterface } from "node:readline";
import { apps } from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import {
  connectNativeClient,
  NativeOAuthClient,
  MemoryNativeOAuthCustody,
} from "../native.ts";
import { LiveStreamError } from "../live/types.ts";
import type { OperationRefData } from "../operations.ts";

const stdin = createInterface({ input: process.stdin });
const messages = stdin[Symbol.asyncIterator]();
const first = await messages.next();
if (first.done) throw new Error("caller credentials required");
const config = JSON.parse(first.value) as {
  issuer: string;
  username: string;
  password: string;
  clientId: string;
  deploymentId: string;
};
const credentialCustody = new MemoryNativeOAuthCustody();
const native = await loginNativeTestClient({
  ...config,
  custody: credentialCustody,
  request: apps.TransferCaller.request,
});
let caller = await connectNativeClient({
  ...native,
  request: apps.TransferCaller.request,
}).orThrow();
try {
  const bytes = Uint8Array.from(
    { length: 5 * 1024 * 1024 + 7 },
    (_, index) => index % 251,
  );
  const upload = await caller
    .upload({ value: "persisted" })
    .transfer(bytes)
    .start()
    .orThrow();
  const terminal = await upload.wait().orThrow();
  assert.equal(terminal.terminal.state, "completed");
  assert.equal(terminal.transferred.size, bytes.length);
  assert.equal(terminal.terminal.transfer?.transferredBytes, bytes.length);

  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let consumed!: () => void;
  const consuming = new Promise<void>((resolve) => {
    consumed = resolve;
  });
  let reference: OperationRefData | undefined;
  const source = (async function* () {
    yield bytes.subarray(0, 1024 * 1024);
    consumed();
    await held;
    yield bytes.subarray(1024 * 1024);
  })();
  const pending = caller
    .upload({ value: "cancelled" })
    .transfer(source)
    .onAccepted(({ snapshot }) => {
      reference = { ...snapshot, deploymentId: config.deploymentId };
    })
    .start()
    .take();
  await consuming;
  assert(reference);
  const cancelled = caller.upload.resume(reference);
  await cancelled.cancel().orThrow();
  release();
  assert(isErr(await pending));
  assert.equal((await cancelled.wait().orThrow()).state, "cancelled");
  assert.equal(
    (await caller.echo({ value: "executions" }).orThrow()).value,
    "1",
  );

  const retired = await caller.watch({}).orThrow();
  const retiredEvents = retired[Symbol.asyncIterator]();
  assert.equal((await retiredEvents.next()).done, false);
  await retired.close();
  const scope = native.session.scope;
  const trust = native.session.trust;
  await caller.connection.close();
  await native.session.close();
  const custody = (
    await NativeOAuthClient.open({ custody: credentialCustody, scope, trust })
  ).take();
  if (isErr(custody)) throw custody.error;
  const resumed = (await custody.resume()).take();
  if (isErr(resumed)) throw resumed.error;
  native.session = resumed;
  caller = await connectNativeClient({
    ...native,
    request: apps.TransferCaller.request,
  }).orThrow();
  assert.equal((await retiredEvents.next()).done, true);
  assert.equal(
    (await caller.upload.resume(reference).get().orThrow()).state,
    "cancelled",
  );

  const live = await caller.watch({}).orThrow();
  const events = live[Symbol.asyncIterator]();
  assert.equal((await events.next()).done, false);
  await caller.refreshAuthority();
  assert.equal((await events.next()).done, false);
  console.log(JSON.stringify({ stage: "revoke" }));
  try {
    for (;;) {
      const next = await events.next();
      if (next.done) break;
    }
  } catch (error) {
    assert(error instanceof LiveStreamError);
  }
  assert.equal((await live.closed).isComplete(), false);
  assert(isErr(await caller.echo({ value: "revoked" }).take()));
  await live.close();
  console.log(JSON.stringify({ stage: "complete" }));
} finally {
  stdin.close();
  await caller.connection.close();
  await native.session.close();
}
