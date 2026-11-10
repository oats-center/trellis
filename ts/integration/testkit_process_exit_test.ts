/**
 * Live acceptance for the testkit's process ownership.
 *
 * A test that times out never reaches its cleanup, and the test runner then
 * exits its process. The Trellis host that process started must go with it.
 */

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { connect } from "node:net";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";

const SCRIPT = `
import { withTrellisRuntime } from "./ts/integration/_support/runtime.ts";
await withTrellisRuntime(async (runtime) => {
  console.log("url=" + runtime.trellisUrl);
  process.exit(0);
});
`;

async function accepts(url: URL): Promise<boolean> {
  const socket = connect(Number(url.port), url.hostname);
  try {
    await once(socket, "connect");
    return true;
  } catch {
    return false;
  } finally {
    socket.destroy();
  }
}

test("a Trellis host does not outlive the process that started it", async () => {
  const child = spawn(
    process.execPath,
    ["--experimental-transform-types", "--input-type=module", "-e", SCRIPT],
    { stdio: ["ignore", "pipe", "inherit"] },
  );
  let output = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
    output += chunk;
  });
  const [code] = await once(child, "exit");
  assert.equal(code, 0);
  const found = /^url=(.+)$/m.exec(output)?.[1];
  assert(found, output);

  const deadline = Date.now() + 10_000;
  const url = new URL(found);
  while (await accepts(url)) {
    assert(Date.now() < deadline, `${found} still accepts connections`);
    await delay(50);
  }
});
