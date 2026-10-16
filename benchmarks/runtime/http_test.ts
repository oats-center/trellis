import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { z } from "zod";
import { spawnTracked } from "../../ts/integration/_support/process.ts";
import { waitFor } from "../../ts/packages/trellis-testkit/src/wait.ts";
import { deadline, payload } from "./model.ts";

test(
  "Node HTTP baseline echoes requests and streams binary transfers without corruption",
  { timeout: 10_000 },
  async (context) => {
    const output = await mkdtemp(join(tmpdir(), "trellis-http-benchmark-"));
    const body = payload(23_001);
    const child = spawnTracked(process.execPath, [
      "--experimental-transform-types",
      fileURLToPath(new URL("./http.ts", import.meta.url)),
    ]);
    const stderr: Buffer[] = [];
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.stdin.end(
      JSON.stringify({
        role: "provider",
        output,
        trellisUrl: "http://127.0.0.1",
        username: "admin",
        password: "unused",
        samples: 1,
        calls: 1,
        sizes: [body.length],
        sessionCounts: [1],
        idleSeconds: 1,
        cpuTicks: 100,
        arrivalRate: 0,
        maxOutstanding: 1,
      }),
    );
    try {
      await writeFile(join(output, `body-${body.length}.bin`), body);
      const { httpUrl } = await waitFor(
        async () => {
          if (child.exitCode !== null)
            throw new Error(Buffer.concat(stderr).toString());
          return z
            .object({ httpUrl: z.url() })
            .parse(
              JSON.parse(await readFile(join(output, "http.json"), "utf8")),
            );
        },
        { timeoutMs: 5_000, intervalMs: 20 },
      );
      const echo = await fetch(`${httpUrl}/echo`, {
        method: "POST",
        body: JSON.stringify({ value: "round-trip" }),
        signal: context.signal,
      });
      assert.deepEqual(await echo.json(), { value: "round-trip" });
      const download = await fetch(`${httpUrl}/download?size=${body.length}`, {
        signal: context.signal,
      });
      assert.equal(download.status, 200);
      assert.deepEqual(new Uint8Array(await download.arrayBuffer()), body);
      const upload = await fetch(`${httpUrl}/upload?size=${body.length}`, {
        method: "PUT",
        body,
        signal: context.signal,
      });
      assert.deepEqual(await upload.json(), {
        size: body.length,
        digest: createHash("sha256").update(body).digest("hex"),
      });
      const live = await fetch(`${httpUrl}/live?value=first-event`, {
        signal: context.signal,
      });
      assert.deepEqual(JSON.parse(await live.text()), { value: "first-event" });
    } finally {
      child.kill("SIGTERM");
      try {
        const status = await deadline(child.exited, 5_000);
        assert.equal(status.code, 0, Buffer.concat(stderr).toString());
      } finally {
        child.kill("SIGKILL");
        await child.exited.catch(() => undefined);
        await rm(output, { recursive: true, force: true });
      }
    }
  },
);
