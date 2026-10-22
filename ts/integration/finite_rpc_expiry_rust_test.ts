/** Accepted finite results outlive their opening lease, not their request deadline. */
import assert from "node:assert/strict";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apis,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

test("generated Rust finite success and declared error survive opening expiry without replay", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.FiniteProvider.participant;
      const identity = await runtime.registerService({
        name: "finite-provider",
        deployment: "finite-provider",
        contract,
      });
      const provider = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      let closing = false;
      const stopped = provider.wait().catch((error: unknown) => {
        if (!closing) throw error;
      });
      void stopped.catch(() => undefined);
      const held = new Map<string, { release(): void; digest: string }>();
      const completed = new Set<string>();
      const handler: Parameters<typeof provider.handleHoldSuccess>[0] = async ({
        input,
        context,
      }) => {
        assert(context.caller.type === "verified");
        assert(!held.has(input.value), "accepted work must not be replayed");
        const gate = Promise.withResolvers<void>();
        held.set(input.value, {
          release: gate.resolve,
          digest: context.caller.authorityDigest,
        });
        await gate.promise;
        completed.add(input.value);
        return input.value === "declared"
          ? Result.err(
              new apis.finite.Rejected({
                id: ulid(),
                type: apis.finite.Rejected.type,
                message: "declined",
                value: input.value,
              }),
            )
          : Result.ok(input);
      };
      // Distinct ordinary intake subjects allow the four barriers to be held
      // together without changing the provider's serial per-subject execution.
      await provider.handleHoldSuccess(handler);
      await provider.handleHoldDeclared(handler);
      await provider.handleHoldCancelled(handler);
      await provider.handleHoldDeadline(handler);
      const caller = await runtime.registerService({
        name: "finite-caller",
        deployment: "finite-caller",
        contract: participants.FiniteCaller.participant,
      });
      const child = spawnTracked("setsid", rustFixtureArgv("finite_rpc"), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
          TRELLIS_IDENTITY_SEED: caller.seed,
        },
      });
      child.stderr.pipe(process.stderr);
      const reader = createInterface({ input: child.stdout });
      const lines: string[] = [];
      reader.on("line", (line) => lines.push(line));
      const closed = new AbortController();
      reader.once("close", () => closed.abort(new Error(lines.join("\n"))));
      const waitFor = async (prefix: string, count = 1) => {
        const signal = AbortSignal.any([
          closed.signal,
          AbortSignal.timeout(90_000),
        ]);
        for (;;) {
          assert(
            !lines.some((line) => line.startsWith("FINITE_UNEXPECTED")),
            lines.join("\n"),
          );
          const found = lines.filter((line) => line.startsWith(prefix))[
            count - 1
          ];
          if (found !== undefined) return found;
          await once(reader, "line", { signal });
        }
      };
      try {
        await waitFor("FINITE_READY");
        child.stdin.write("CONTEXT\n");
        const original: { digest: string; expiresAt: number } = JSON.parse(
          (await waitFor("FINITE_CONTEXT ")).slice("FINITE_CONTEXT ".length),
        );
        // Open before normal renewal; the handler's authenticated digest below
        // establishes which signed authority actually admitted every request.
        await runtime.waitFor(
          () => Date.now() >= original.expiresAt * 1_000 - 45_000,
          { timeoutMs: 80_000 },
        );
        child.stdin.write("START\n");
        await runtime
          .waitFor(() => held.size === 4, { timeoutMs: 10_000 })
          .catch((cause: unknown) => {
            throw new Error(
              `Finite openings: ${JSON.stringify([...held.keys()])}; native output:\n${lines.join("\n")}`,
              { cause },
            );
          });
        assert(
          [...held.values()].every((call) => call.digest === original.digest),
        );
        child.stdin.write("CANCEL\n");
        await waitFor("FINITE_CANCELLED");
        await runtime.waitFor(() => Date.now() >= original.expiresAt * 1_000, {
          timeoutMs: 50_000,
        });
        child.stdin.write("CONTEXT\n");
        const renewed: { digest: string; expiresAt: number } = JSON.parse(
          (await waitFor("FINITE_CONTEXT ", 2)).slice("FINITE_CONTEXT ".length),
        );
        assert.notEqual(renewed.digest, original.digest);
        assert(renewed.expiresAt * 1_000 > Date.now());
        for (const [name, call] of held) if (name !== "timeout") call.release();
        await waitFor("FINITE_SUCCESS");
        await waitFor("FINITE_DECLARED");
        await waitFor("FINITE_TIMEOUT");
        held.get("timeout")?.release();
        await runtime.waitFor(() => completed.size === 4);
        child.stdin.end("EXIT\n");
        assert.equal((await child.exited).code, 0);
        assert.equal(
          lines.filter((line) => line === "FINITE_CANCELLED").length,
          1,
        );
        assert.equal(held.size, 4);
      } finally {
        for (const call of held.values()) call.release();
        if (child.exitCode === null && child.signalCode === null)
          child.kill("SIGTERM");
        await child.exited;
        reader.close();
        closing = true;
        await provider.connection.close();
        await stopped;
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
