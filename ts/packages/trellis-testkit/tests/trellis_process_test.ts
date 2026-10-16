import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { startTrellisProcess } from "../src/trellis_process.ts";

test(
  "a real child exiting before readiness reports its status and captured output",
  { timeout: 15_000 },
  async () => {
    const directory = await mkdtemp(join(tmpdir(), "trellis-process-exit-"));
    try {
      await assert.rejects(
        () =>
          startTrellisProcess({
            trellisUrl: "http://127.0.0.1:9",
            configPath: join(directory, "config.toml"),
            command: {
              cmd: process.execPath,
              args: [
                "-e",
                "console.log('launcher stdout'); console.error('launcher stderr'); process.exitCode = 23;",
              ],
            },
            startupTimeoutMs: 10_000,
            shutdownTimeoutMs: 1_000,
          }),
        (error: unknown) => {
          assert.ok(error instanceof Error);
          assert.match(
            error.message,
            /exited before readiness \(exit code 23\)/,
          );
          assert.match(error.message, /launcher stdout/);
          assert.match(error.message, /launcher stderr/);
          return true;
        },
      );
    } finally {
      await rm(directory, { recursive: true });
    }
  },
);
