import { RemoteError, TransportError } from "@oatscenter/trellis/errors";
import { assert, assertEquals } from "@std/assert";
import { isErr, Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("service stops its Operation providers when the broker cannot acknowledge cleanup", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "offline-operation-provider",
      contract: participants.Provider.participant,
    });
    const provider = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    await provider.handleWork(async () => {});
    const brokerPid = Number(
      (await Deno.readTextFile(
        `${runtime.workdir}/runtime/trellis/nats-server.pid`,
      )).trim(),
    );
    assert(Number.isSafeInteger(brokerPid) && brokerPid > 0);
    Deno.kill(brokerPid, "SIGSTOP");
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        provider.stop(),
        new Promise<never>((_, reject) => {
          timeout = setTimeout(
            () =>
              reject(new Error("Service stop required unavailable transport")),
            10_000,
          );
        }),
      ]);
    } finally {
      clearTimeout(timeout);
      Deno.kill(brokerPid, "SIGCONT");
      await provider.stop();
    }
  });
});

for (const language of ["typescript", "rust"] as const) {
  Deno.test(`${language} service bounds requests and bytes without blocking cancellation`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: `admission-${language}`,
        contract: participants.Provider.participant,
      });
      // An ordinary second session reads the shared durable resource, without
      // sending inspection RPCs through the saturated application intake.
      const inspector = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: identity.seed,
      }).orThrow();
      const records = inspector.kv.records;
      let provider: typeof inspector | undefined;
      let child: Deno.ChildProcess | undefined;
      let childOutput: Promise<unknown> | undefined;
      const calls: Promise<unknown>[] = [];
      const client = await runtime.connectClient({
        name: `admission-caller-${language}`,
        contract: participants.Caller.participant,
      });
      try {
        await records.put("release-ready", { value: "ready" }).orThrow();
        if (language === "rust") {
          child = new Deno.Command(rustFixtureArgv("admission")[0], {
            env: {
              TRELLIS_URL: runtime.trellisUrl,
              TRELLIS_IDENTITY_SEED: identity.seed,
            },
            stdout: "piped",
            stderr: "piped",
          }).spawn();
          childOutput = child.output();
        } else {
          provider = await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.Provider.participant,
            seed: identity.seed,
            runtime: {
              requestLimits: { requests: 2, bytes: 16 * 1024 },
            },
          }).orThrow();
          await provider.handleEcho(async ({ input, client }) => {
            await client.kv.records.put(`entered-${input.value}`, input)
              .orThrow();
            await runtime.waitFor(async () =>
              await client.kv.records.get(`release-${input.value}`).orThrow() ??
                false
            );
            return Result.ok(input);
          });
          await provider.handleWork(async ({ input, op, signal }) => {
            await op.progress({
              value: input.value,
              nested: { count: 1n, payload: new Uint8Array([1]) },
            }).orThrow();
            if (!signal.aborted) {
              await new Promise<void>((resolve) =>
                signal.addEventListener("abort", () => resolve(), {
                  once: true,
                })
              );
            }
          });
        }
        await runtime.waitFor(async () =>
          (await client.echo({ value: "ready" })).isOk()
        );
        const operation = await client.work({ value: "running" }).start()
          .orThrow();
        const first = client.echo({ value: "first" }).orThrow();
        const second = client.echo({ value: "second" }).orThrow();
        calls.push(first, second);
        await runtime.waitFor(async () => {
          const values = await Promise.all([
            records.get("entered-first").orThrow(),
            records.get("entered-second").orThrow(),
          ]);
          return values.every(Boolean);
        });

        const refused = await client.echo({ value: "refused" });
        assert(isErr(refused));
        assert(
          refused.error instanceof RemoteError,
          Deno.inspect(refused.error),
        );
        assert("code" in refused.error.remoteError);
        assertEquals(refused.error.remoteError.code, "trellis.service.busy");
        const excessStart = await client.work({ value: "excess-start" })
          .start();
        assert(isErr(excessStart));
        assert(
          excessStart.error instanceof TransportError,
          Deno.inspect(excessStart.error),
        );
        assertEquals(excessStart.error.code, "trellis.service.busy");

        // Both ordinary slots remain occupied. Cancellation must still finish,
        // not merely be acknowledged while waiting for those slots to free.
        await operation.cancel().orThrow();
        assertEquals(
          (await operation.wait({
            observationSignal: AbortSignal.timeout(15_000),
          }).orThrow()).state,
          "cancelled",
        );
        await records.put("release-first", { value: "release" }).orThrow();
        await records.put("release-second", { value: "release" }).orThrow();
        assertEquals((await first).value, "first");
        assertEquals((await second).value, "second");
        assert(
          !(await records.get("entered-refused").orThrow()),
          "a busy refusal must not execute or automatically retry the handler",
        );

        const oversized = await client.echo({ value: "x".repeat(32 * 1024) });
        assert(isErr(oversized));
        assert(
          oversized.error instanceof RemoteError,
          Deno.inspect(oversized.error),
        );
        assert("code" in oversized.error.remoteError);
        assertEquals(oversized.error.remoteError.code, "trellis.service.busy");
        // Verify byte refusal returned any partially acquired count reservation.
        assertEquals(
          (await client.echo({ value: "ready" }).orThrow()).value,
          "ready",
        );
        if (provider) {
          const firstDrain = client.echo({ value: "drain-first" }).orThrow();
          const secondDrain = client.echo({ value: "drain-second" }).orThrow();
          calls.push(firstDrain, secondDrain);
          await runtime.waitFor(async () => {
            const entered = await Promise.all([
              records.get("entered-drain-first").orThrow(),
              records.get("entered-drain-second").orThrow(),
            ]);
            return entered.every(Boolean);
          });
          const stopped = provider.stop();
          await records.put("release-drain-first", { value: "release" })
            .orThrow();
          await records.put("release-drain-second", { value: "release" })
            .orThrow();
          assertEquals((await firstDrain).value, "drain-first");
          assertEquals((await secondDrain).value, "drain-second");
          await stopped;
        }
      } finally {
        await records.put("release-first", { value: "release" }).orThrow();
        await records.put("release-second", { value: "release" }).orThrow();
        await records.put("release-drain-first", { value: "release" })
          .orThrow();
        await records.put("release-drain-second", { value: "release" })
          .orThrow();
        await Promise.allSettled(calls);
        await client.connection.close();
        await provider?.stop();
        if (child) {
          try {
            child.kill("SIGTERM");
          } catch (error) {
            if (!(error instanceof Deno.errors.NotFound)) throw error;
          }
          await childOutput;
        }
        await inspector.stop();
      }
    });
  });
}
