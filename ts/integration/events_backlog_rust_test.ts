import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

test("generated Rust durable Events retain publication-time authority after normal renewal", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "public-backlog",
        contract: participants.EventServiceBoth.participant,
      });
      const child = spawnTracked("setsid", rustFixtureArgv("events"), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_IDENTITY_SEED: identity.seed,
          PUBLIC_BACKLOG: "true",
        },
      });
      child.stderr.pipe(process.stderr);
      const reader = createInterface({ input: child.stdout });
      const lines: string[] = [];
      reader.on("line", (line) => lines.push(line));
      try {
        const publication = await runtime.waitFor(() =>
          lines.find((line) => line.startsWith("PUBLIC_BACKLOG_READY ")),
        );
        const { expiresAt }: { expiresAt: number } = JSON.parse(
          publication.slice("PUBLIC_BACKLOG_READY ".length),
        );
        await runtime.waitFor(() => Date.now() >= expiresAt * 1_000, {
          timeoutMs: 90_000,
        });
        child.stdin.end("PULL\n");
        assert.equal((await child.exited).code, 0, lines.join("\n"));
        assert(lines.includes("PUBLIC_BACKLOG_DELIVERED"));
      } finally {
        if (child.exitCode === null && child.signalCode === null)
          child.kill("SIGTERM");
        await child.exited;
        reader.close();
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 55,
        refreshLeadSeconds: 12,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 20,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
