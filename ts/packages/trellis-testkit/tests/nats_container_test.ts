import { assert, assertEquals } from "@std/assert";
import { jetstreamManager } from "@nats-io/jetstream";
import { join } from "@std/path";
import {
  commandExecutableMatches,
  formatPidFile,
  processIsGone,
  recordProcessIdentity,
} from "../src/cleanup.ts";
import {
  cleanupStaleNatsPidFile,
  NatsTestContainer,
  removeOwnedPidFile,
} from "../src/nats_container.ts";

async function portAcceptsConnections(port: number): Promise<boolean> {
  try {
    const conn = await Deno.connect({ hostname: "127.0.0.1", port });
    conn.close();
    return true;
  } catch {
    return false;
  }
}

async function listPidFiles(natsDir: string): Promise<string[]> {
  const names: string[] = [];
  for await (const entry of Deno.readDir(natsDir)) {
    if (entry.name.endsWith(".pid")) names.push(entry.name);
  }
  return names;
}

Deno.test({
  name: "NatsTestContainer.start spawns a clean local nats-server",
  fn: async () => {
    const workdir = await Deno.makeTempDir({ prefix: "trellis-nats-smoke-" });
    const natsDir = join(workdir, "nats");
    let nats: NatsTestContainer | undefined;
    let replacedPidFile: string | undefined;
    const foreignPid = "999999\nforeign\n/not/nats\n";
    try {
      try {
        nats = await NatsTestContainer.start(workdir, { startupMs: 60_000 });
        assertEquals(nats.nc.isClosed(), false);

        const jsm = await jetstreamManager(nats.nc);
        assertEquals(await jsm.streams.list().next(), []);

        const natsPort = Number(nats.natsUrl.split(":").at(-1));
        const websocketPort = Number(nats.websocketUrl.split(":").at(-1));
        assert(await portAcceptsConnections(natsPort));
        assert(await portAcceptsConnections(websocketPort));

        // A foreign owner replaces the pid file while the run is live: stop()
        // must leave the replacement in place, not unlink someone else's file.
        const pidFiles = await listPidFiles(natsDir);
        assert(pidFiles.length === 1);
        replacedPidFile = join(natsDir, pidFiles[0]);
        await Deno.writeTextFile(replacedPidFile, foreignPid);
      } finally {
        await nats?.stop();
      }

      // stop() closes the connection and child, but must preserve the foreign
      // replacement rather than removing or rewriting another owner's file.
      assert(nats !== undefined);
      assertEquals(nats.nc.isClosed(), true);
      assert(replacedPidFile !== undefined);
      assertEquals(await Deno.readTextFile(replacedPidFile), foreignPid);
      const natsPort = Number(nats.natsUrl.split(":").at(-1));
      assertEquals(await portAcceptsConnections(natsPort), false);
    } finally {
      await Deno.remove(workdir, { recursive: true }).catch(() => undefined);
    }
  },
});

Deno.test({
  name: "concurrent NATS starts bind distinct owned port leases",
  fn: async () => {
    const workdirs = await Promise.all(
      Array.from(
        { length: 4 },
        () => Deno.makeTempDir({ prefix: "trellis-nats-concurrent-" }),
      ),
    );
    const containers: NatsTestContainer[] = [];
    try {
      // Retain each successful child even when a sibling fails. Wait for all
      // starts to settle before cleanup so no late child escapes ownership.
      const starts = await Promise.allSettled(
        workdirs.map(async (workdir) => {
          const container = await NatsTestContainer.start(workdir, {
            startupMs: 60_000,
          });
          containers.push(container);
        }),
      );
      for (const result of starts) {
        if (result.status === "rejected") throw result.reason;
      }
      const endpoints = containers.flatMap((container) => [
        container.natsUrl,
        container.websocketUrl,
      ]);
      assertEquals(new Set(endpoints).size, endpoints.length);
    } finally {
      await Promise.allSettled(containers.map((container) => container.stop()));
      await Promise.allSettled(
        workdirs.map((workdir) => Deno.remove(workdir, { recursive: true })),
      );
    }
  },
});

function spawnSleepChild(): Deno.ChildProcess {
  return new Deno.Command("sh", {
    args: ["-c", "sleep 60"],
    stdout: "null",
    stderr: "null",
  }).spawn();
}

async function pidFileExists(path: string): Promise<boolean> {
  return await Deno.stat(path).then(() => true).catch(() => false);
}

Deno.test("stale nats pid file with a recycled pid is never signaled", async () => {
  const dir = await Deno.makeTempDir({ prefix: "trellis-nats-recycle-" });
  const proc = spawnSleepChild();
  try {
    const pidFile = join(dir, "nats-999999-1.pid");
    // The pid is live but the recorded start identity does not match it.
    await Deno.writeTextFile(
      pidFile,
      formatPidFile({ pid: proc.pid, start: "0", executable: "/bin/sh" }),
    );
    await cleanupStaleNatsPidFile(pidFile);
    // Give any incorrect signal a moment to land before asserting survival.
    await new Promise((resolve) => setTimeout(resolve, 500));
    assertEquals(await processIsGone(proc.pid), false);
    assertEquals(await pidFileExists(pidFile), false);
  } finally {
    try {
      Deno.kill(proc.pid, "SIGKILL");
    } catch {
      // already gone
    }
    await proc.status.catch(() => undefined);
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("stale nats pid file with matching identity is signaled", async () => {
  const dir = await Deno.makeTempDir({ prefix: "trellis-nats-match-" });
  const proc = spawnSleepChild();
  try {
    const pidFile = join(dir, "nats-999999-1.pid");
    await Deno.writeTextFile(
      pidFile,
      formatPidFile(await recordProcessIdentity(proc.pid, "/bin/sh")),
    );
    await cleanupStaleNatsPidFile(pidFile);
    assertEquals(await processIsGone(proc.pid), true);
    assertEquals(await pidFileExists(pidFile), false);
  } finally {
    try {
      Deno.kill(proc.pid, "SIGKILL");
    } catch {
      // already gone
    }
    await proc.status.catch(() => undefined);
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("stale cleanup ignores symlinked pid files", async () => {
  const dir = await Deno.makeTempDir({ prefix: "trellis-nats-symlink-" });
  const proc = spawnSleepChild();
  try {
    const target = join(dir, "real.pid");
    const link = join(dir, "nats-999999-1.pid");
    await Deno.writeTextFile(
      target,
      formatPidFile(await recordProcessIdentity(proc.pid, "/bin/sh")),
    );
    await Deno.symlink(target, link);
    await cleanupStaleNatsPidFile(link);
    // Give any (incorrect) signal a moment to land before asserting survival.
    await new Promise((resolve) => setTimeout(resolve, 500));
    assertEquals(await processIsGone(proc.pid), false);
    assertEquals(await Deno.lstat(link).then((stat) => stat.isSymlink), true);
  } finally {
    try {
      Deno.kill(proc.pid, "SIGKILL");
    } catch {
      // already gone
    }
    await proc.status.catch(() => undefined);
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("stale cleanup leaves pid files replaced during the grace period", async () => {
  const dir = await Deno.makeTempDir({ prefix: "trellis-nats-grace-" });
  const proc = spawnSleepChild();
  try {
    const pidFile = join(dir, "nats-999999-1.pid");
    await Deno.writeTextFile(
      pidFile,
      formatPidFile(await recordProcessIdentity(proc.pid, "/bin/sh")),
    );
    const cleanup = cleanupStaleNatsPidFile(pidFile);
    // Replace the content while the SIGTERM grace wait is in flight.
    await new Promise((resolve) => setTimeout(resolve, 500));
    const replaced = "77777\nreplacement\n/other/nats-server\n";
    await Deno.writeTextFile(pidFile, replaced);
    await cleanup;
    assertEquals(await Deno.readTextFile(pidFile), replaced);
  } finally {
    try {
      Deno.kill(proc.pid, "SIGKILL");
    } catch {
      // already gone
    }
    await proc.status.catch(() => undefined);
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("stale cleanup never reads or removes non-regular pid paths", async () => {
  const dir = await Deno.makeTempDir({ prefix: "trellis-nats-fifo-" });
  try {
    // A FIFO at the pid path must never be opened (a read with no writer would
    // block the harness); the isFile guard must reject it before any read.
    const fifoPath = join(dir, "nats-999999-1.pid");
    const mkfifo = await new Deno.Command("mkfifo", {
      args: [fifoPath],
      stdout: "null",
      stderr: "null",
    }).output();
    assert(mkfifo.success, "mkfifo must succeed to exercise FIFO cleanup");
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      const outcome = await Promise.race([
        cleanupStaleNatsPidFile(fifoPath).then(() => "done"),
        new Promise<string>((resolve) => {
          timeout = setTimeout(() => resolve("blocked"), 3_000);
        }),
      ]);
      assertEquals(outcome, "done", "cleanup must not block on a FIFO");
    } finally {
      if (timeout !== undefined) clearTimeout(timeout);
    }
    assertEquals(await Deno.stat(fifoPath).then((s) => s.isFifo), true);
    await removeOwnedPidFile(fifoPath, "anything\n");
    assertEquals(await Deno.stat(fifoPath).then((s) => s.isFifo), true);
    // A directory at the pid path is also non-regular: no read, no removal.
    const dirPath = join(dir, "nats-999998-1.pid");
    await Deno.mkdir(dirPath);
    await cleanupStaleNatsPidFile(dirPath);
    assertEquals(await Deno.stat(dirPath).then((s) => s.isDirectory), true);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test(
  "non-linux stale cleanup requires the exact executable token",
  () => {
    const executable = "/cache/trellis/nats-server-v2.14.4-linux-amd64";
    assertEquals(
      commandExecutableMatches(`${executable} -c /tmp/nats.conf`, executable),
      true,
    );
    assertEquals(
      commandExecutableMatches(
        "nats-server-v2.14.4-linux-amd64 --jetstream",
        executable,
      ),
      true,
    );
    // A substring match on "nats-server" would have passed this.
    assertEquals(
      commandExecutableMatches(
        "/usr/bin/nats-server-backup --jetstream",
        executable,
      ),
      false,
    );
    assertEquals(
      commandExecutableMatches(
        "/opt/other/nats-server-v2.14.4-linux-amd64",
        executable,
      ),
      false,
    );
    assertEquals(commandExecutableMatches("sh -c sleep 60", executable), false);
    assertEquals(commandExecutableMatches("", executable), false);
  },
);

Deno.test("owned pid file removal leaves replaced files alone", async () => {
  const dir = await Deno.makeTempDir({ prefix: "trellis-nats-owner-" });
  try {
    const pidFile = join(dir, "nats-1-1.pid");
    const content = "12345\n67890\n/opt/nats-server\n";
    await Deno.writeTextFile(pidFile, content);
    await removeOwnedPidFile(pidFile, content);
    assertEquals(await pidFileExists(pidFile), false);

    await Deno.writeTextFile(pidFile, content);
    // Another owner replaced the content between spawn and stop.
    await Deno.writeTextFile(pidFile, "99999\n00000\n/other/nats-server\n");
    await removeOwnedPidFile(pidFile, content);
    assertEquals(
      await Deno.readTextFile(pidFile),
      "99999\n00000\n/other/nats-server\n",
    );
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});
