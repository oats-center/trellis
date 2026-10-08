import { assert, assertEquals } from "@std/assert";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type { AuthSessionsListResponse } from "../../ts/packages/trellis-testkit/trellis/types/index.js";
import { withTrellisRuntime } from "../../ts/integration/_support/runtime.ts";

const consumerDirectory = Deno.env.get("TRELLIS_WORKER_CONSUMER_DIR");
if (!consumerDirectory) {
  throw new Error(
    "TRELLIS_WORKER_CONSUMER_DIR must name the producer-built npm consumer with generated/ fixture contracts",
  );
}
const nodeBinary = Deno.env.get("TRELLIS_WORKER_NODE_BIN") ?? "node";
Deno.copyFileSync(
  new URL("./worker-lifecycle-provider.mjs", import.meta.url),
  `${consumerDirectory}/worker-lifecycle-provider.mjs`,
);

for (
  const scenario of [
    "loss",
    "stale-request",
    "revocation",
    "growth-1",
    "growth-2",
    "growth-3",
    "growth-failure",
    "burst",
    "balanced",
  ]
) {
  Deno.test(`real worker lifecycle: ${scenario}`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: `worker-gap-${scenario}`,
        contract: participants.Provider.participant,
      });
      const inspector = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: identity.seed,
      }).orThrow();
      const child = new Deno.Command(nodeBinary, {
        args: [
          new URL("./worker-lifecycle-debugger.mjs", import.meta.url).pathname,
        ],
        env: {
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_IDENTITY_SEED: identity.seed,
          TRELLIS_WORKER_CONSUMER_DIR: consumerDirectory,
          TRELLIS_MAX_VERIFICATION_WORKERS: /^growth-[123]$/.test(scenario)
            ? scenario.slice(-1)
            : "3",
        },
        stdin: "piped",
        stdout: "piped",
        stderr: "inherit",
      }).spawn();
      const events: { event: string; value?: string; title?: string }[] = [];
      const reader = (async () => {
        let buffer = "";
        for await (
          const chunk of child.stdout.pipeThrough(new TextDecoderStream())
        ) {
          buffer += chunk;
          let end;
          while ((end = buffer.indexOf("\n")) >= 0) {
            const line = buffer.slice(0, end);
            buffer = buffer.slice(end + 1);
            console.log(line);
            events.push(JSON.parse(line));
          }
        }
      })();
      const writer = child.stdin.getWriter();
      const send = async (command: string, contextDigest?: string) =>
        await writer.write(
          new TextEncoder().encode(
            `${JSON.stringify({ command, contextDigest })}\n`,
          ),
        );
      try {
        await runtime.waitFor(
          () => events.some((event) => event.event === "ready"),
          { timeoutMs: 60_000 },
        );
        const caller = await runtime.connectClient({
          name: `gap-${scenario}`,
          contract: participants.Caller.participant,
        });
        if (scenario === "burst") {
          await send("resume");
          const values = await Promise.all(
            Array.from(
              { length: 8 },
              (_, index) => caller.echo({ value: `burst-${index}` }).orThrow(),
            ),
          );
          values.forEach((value, index) =>
            assertEquals(value.value, `burst-${index}`)
          );
          await send("count");
          await runtime.waitFor(() =>
            events.some((event) => event.event === "count")
          );
          assertEquals(
            Number(events.find((event) => event.event === "count")!.value),
            1,
          );
          await caller.echo({ value: "after-burst" }).orThrow();
          return;
        }
        const held = caller.echo({ value: scenario }, { timeout: 60_000 });
        await runtime.waitFor(
          () => events.some((event) => event.event === "paused"),
          { timeoutMs: 10_000 },
        );
        await send("inspect");
        await runtime.waitFor(
          () => events.some((event) => event.event === "proof"),
          { timeoutMs: 10_000 },
        );
        const proof = JSON.parse(
          events.find((event) => event.event === "proof")!.value!,
        );
        assert(
          proof.parsed.ok,
          "held request must have passed complete proof verification",
        );
        if (scenario === "balanced") {
          let completed = 0;
          const fresh = Array.from(
            { length: 8 },
            (_, index) =>
              caller.echo({ value: `balanced-${index}` }, { timeout: 60_000 })
                .then((result) => {
                  if (result.isOk()) completed++;
                  return result;
                }),
          );
          await runtime.waitFor(() => completed === 8, { timeoutMs: 10_000 });
          for (const [index, result] of (await Promise.all(fresh)).entries()) {
            assertEquals(result.orThrow().value, `balanced-${index}`);
          }
          assertEquals(
            await inspector.kv.records.get(`entered-${scenario}`).orThrow(),
            undefined,
          );
          await send("count");
          await runtime.waitFor(() =>
            events.some((event) => event.event === "count")
          );
          assertEquals(
            Number(events.find((event) => event.event === "count")!.value),
            2,
          );
          await send("resume");
          assertEquals((await held).orThrow().value, scenario);
          return;
        }
        if (scenario.startsWith("growth-")) {
          const maximum = scenario === "growth-failure"
            ? 2
            : Number(scenario.slice(-1));
          await send(
            scenario === "growth-failure" ? "hold-startup" : "hold-growth",
          );
          await runtime.waitFor(() =>
            events.some((event) => event.event === "holding-growth")
          );
          const queuedAt = performance.now();
          const queued = Array.from(
            { length: 12 },
            (_, index) =>
              caller.echo({ value: `queued-${index}` }, { timeout: 60_000 })
                .then((result) => result),
          );
          if (scenario === "growth-failure") {
            await runtime.waitFor(
              () => events.some((event) => event.event === "startup-paused"),
              { timeoutMs: 10_000 },
            );
            await send("exit-starting");
            await runtime.waitFor(
              () => events.some((event) => event.event === "exited"),
              { timeoutMs: 10_000 },
            );
          } else {await runtime.waitFor(
              () =>
                events.filter((event) => event.event === "paused").length >=
                  maximum,
              { timeoutMs: 10_000 },
            );}
          // Deliberately sustain backlog past another growth window at the cap.
          await runtime.waitFor(() => performance.now() - queuedAt >= 500);
          await send("resume");
          for (const [index, value] of (await Promise.all(queued)).entries()) {
            assertEquals(value.orThrow().value, `queued-${index}`);
          }
          assertEquals((await held).orThrow().value, scenario);
          await send("count");
          await runtime.waitFor(() =>
            events.some((event) => event.event === "count")
          );
          assertEquals(
            Number(events.find((event) => event.event === "count")!.value),
            maximum,
          );
          // Drained workers remain usable rather than being retired.
          assertEquals(
            (await caller.echo({ value: "after-drain" }).orThrow()).value,
            "after-drain",
          );
          return;
        } else if (scenario === "stale-request") {
          await runtime.waitFor(
            () =>
              Date.now() / 1000 >
                proof.iat + proof.policy.allowedClockSkewSeconds + 2,
            { timeoutMs: 45_000 },
          );
          await send("resume");
        } else if (scenario === "revocation") {
          await send("watch-revocation", proof.parsed.contextDigest);
          await runtime.waitFor(
            () => events.some((event) => event.event === "watching"),
            { timeoutMs: 10_000 },
          );
          const sessions = await runtime.callAdminRpc(
            "authSessionsList",
            {},
          ) as AuthSessionsListResponse;
          const session = sessions.items.find((session) =>
            session.participantId === participants.Caller.participant.id
          );
          assert(session);
          await runtime.callAdminRpc("authSessionsRevoke", {
            sessionId: session.sessionId,
            expectedVersion: session.version,
            idempotencyKey: crypto.randomUUID(),
            reason: "worker verification interleaving",
          });
          await runtime.waitFor(
            () => events.some((event) => event.event === "revoked"),
            { timeoutMs: 10_000 },
          );
          const revocation = JSON.parse(
            events.find((event) => event.event === "revoked")!.value!,
          );
          assertEquals(revocation.contextDigest, proof.parsed.contextDigest);
          assert(revocation.revokedAt > 0);
          await send("resume");
        } else {
          // New accepted requests wait behind the paused real verifier, then
          // execute on added workers. The held attempt is never rescheduled.
          let succeeded = 0;
          const fresh = Array.from(
            { length: 8 },
            (_, index) =>
              caller.echo({ value: `growth-${index}` }, { timeout: 60_000 })
                .then((result) => {
                  if (result.isOk()) succeeded++;
                  return result;
                }),
          );
          await runtime.waitFor(() => succeeded > 0, { timeoutMs: 10_000 });
          await send("exit");
          await runtime.waitFor(
            () => events.some((event) => event.event === "exited"),
            { timeoutMs: 10_000 },
          );
          await send("resume");
          await Promise.all(fresh);
        }
        assert((await held).isErr(), "held request must not succeed");
        assertEquals(
          await inspector.kv.records.get(`entered-${scenario}`).orThrow(),
          undefined,
        );
        if (scenario === "loss") {
          for (const value of ["healthy-first", "healthy-next"]) {
            assertEquals((await caller.echo({ value }).orThrow()).value, value);
          }
        }
      } finally {
        try {
          await send("stop");
        } catch { /* Child may already have exited. */ }
        await writer.close().catch(() => {});
        await child.status;
        await reader;
        await inspector.stop();
      }
    });
  });
}
