/**
 * Live acceptance for the testkit's process ownership.
 *
 * A test that times out never reaches its cleanup, and the test runner then
 * exits its process. The Trellis host that process started must go with it.
 */

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { lstat, readFile } from "node:fs/promises";
import { connect } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { startTrellisRuntime } from "./_support/runtime.ts";

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
  const child = spawn(process.execPath, ["--input-type=module", "-e", SCRIPT], {
    stdio: ["ignore", "pipe", "inherit"],
  });
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

/** The `/proc/<pid>/stat` fields after `comm`, or undefined once the pid is gone. */
async function statFields(pid: number): Promise<string[] | undefined> {
  const stat = await readFile(`/proc/${pid}/stat`, "utf8").catch(
    () => undefined,
  );
  return stat?.slice(stat.lastIndexOf(")") + 2).split(" ");
}

test("a killed host's broker has exited before its sandbox is removed", async () => {
  const runtime = await startTrellisRuntime();
  const pidFile = join(
    runtime.workdir,
    "config/trellis/var/run/nats-server.pid",
  );
  const broker = Number((await readFile(pidFile, "utf8")).trim());
  const host = Number((await statFields(broker))?.[1]);
  // The broker runs in a process group of its own, so killing the host
  // doesn't kill it; the kernel sends it SIGTERM when the host dies.
  process.kill(host, "SIGKILL");
  await runtime.stop();
  await assert.rejects(() => lstat(runtime.workdir), { code: "ENOENT" });
  const state = (await statFields(broker))?.[0];
  assert(
    state === undefined || state === "Z" || state === "X",
    `broker ${broker} still runs (state ${state}) after its sandbox was removed`,
  );
});
