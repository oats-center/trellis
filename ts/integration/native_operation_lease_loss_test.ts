import { assert, assertEquals } from "@std/assert";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

for (const pendingControl of [false, true]) {
  Deno.test(
    pendingControl
      ? "native pending control remains polled while heartbeat awaits its gate"
      : "native operation lease loss joins cleanup and releases the original provider gate",
    async () => {
      await withTrellisRuntime(async (runtime) => {
        const identity = await runtime.registerService({
          name: "lease-loss-provider",
          contract: participants.OperationProvider.participant,
        });
        const child = new Deno.Command("setsid", {
          args: rustFixtureArgv("operation_lease_loss"),
          env: {
            TRELLIS_URL: runtime.trellisUrl,
            TRELLIS_IDENTITY_SEED: identity.seed,
          },
          stdin: "piped",
          stdout: "piped",
          stderr: "inherit",
        }).spawn();
        const writer = child.stdin.getWriter();
        const lines: string[] = [];
        const output = (async () => {
          let pending = "";
          for await (
            const text of child.stdout.pipeThrough(new TextDecoderStream())
          ) {
            pending += text;
            const complete = pending.split("\n");
            pending = complete.pop()!;
            lines.push(...complete);
          }
          if (pending) lines.push(pending);
        })();
        let exited = false;
        const status = child.status.then((value) => {
          exited = true;
          return value;
        });
        const event = (value: string, timeoutMs = 15_000) =>
          runtime.waitFor(async () => {
            if (lines.includes(value)) return true;
            if (exited) {
              throw new Error(
                `fixture exited before ${value}: ${
                  JSON.stringify(await status)
                }; ${lines.join(", ")}`,
              );
            }
            return false;
          }, { timeoutMs });
        const errors: unknown[] = [];
        const gate = runtime.nativeTransportGate();
        let hold: ReturnType<typeof gate.armResponseHold> | undefined;
        try {
          const connection = await runtime.waitFor(
            () =>
              gate.connections().find((connection) =>
                connection.subs.some((sub) =>
                  sub.subject.startsWith("operation.v1.") &&
                  sub.subject.endsWith(".Work")
                )
              ) ?? false,
            { timeoutMs: 60_000 },
          );
          const caller = await runtime.connectClient({
            name: "lease-loss-caller",
            contract: participants.Caller.participant,
          });
          const operation = await caller.work({
            value: pendingControl ? "pending-control" : "lease-loss",
          }).start()
            .orThrow();
          await event("entered");
          hold = gate.armResponseHold("$KV.", connection.id);
          let held: Awaited<typeof hold.held> | undefined;
          const heldReply = hold.held.then((value) => {
            held = value;
          });
          await runtime.waitFor(() => held !== undefined, {
            timeoutMs: 15_000,
          });
          await heldReply;
          assert(held);
          assertEquals(held.connectionId, connection.id);
          assert(held.requestSubject.startsWith("$KV."));

          if (pendingControl) {
            // Progress starts at six seconds; its held ACK timeout spans heartbeat
            // ten. The handler must remain polled while renewal waits for its gate.
            await event("control failed");
            await hold.release();
            await writer.write(new TextEncoder().encode("cleanup\n"));
            const completed = await operation.wait({
              observationSignal: AbortSignal.timeout(15_000),
            }).orThrow();
            assertEquals(completed.state, "completed");
            assertEquals(completed.output?.value, "original");
          } else {
            // The idle handler makes this write a renewal. Cut TCP only after its
            // held ACK anchors resume in that branch, then prove cooperative cleanup.
            runtime.interruptNativeTransport();
            await event("ownership lost", 45_000);
            await writer.write(new TextEncoder().encode("cleanup\n"));
            await event("cleanup complete");
            await hold.release();
            runtime.restoreNativeTransport();
            await runtime.waitFor(
              () =>
                caller.connection.status.phase === "connected" &&
                gate.connections().some((candidate) =>
                  !candidate.closed && candidate.id !== connection.id &&
                  candidate.subs.some((sub) =>
                    sub.subject.startsWith("operation.v1.") &&
                    sub.subject.endsWith(".Work")
                  )
                ),
              { timeoutMs: 30_000 },
            );
          }
          const fresh = await caller.work({ value: "fresh" }).start().orThrow();
          const completed = await fresh.wait({
            observationSignal: AbortSignal.timeout(15_000),
          }).orThrow();
          assertEquals(completed.state, "completed");
          assertEquals(completed.output?.value, "original");
        } catch (error) {
          errors.push(error);
        } finally {
          runtime.restoreNativeTransport();
          try {
            if (!exited) await writer.write(new TextEncoder().encode("stop\n"));
            await runtime.waitFor(() => exited, { timeoutMs: 10_000 });
            assert(
              (await status).success,
              `fixture must exit successfully: ${
                JSON.stringify(await status)
              }; ${lines.join(", ")}`,
            );
          } catch (error) {
            errors.push(error);
          } finally {
            if (!exited) child.kill("SIGKILL");
          }
          const cleanup = await Promise.allSettled([
            hold?.release(),
            status,
            writer.close(),
            output,
          ]);
          for (const result of cleanup) {
            if (result.status === "rejected") errors.push(result.reason);
          }
        }
        if (errors.length) {
          throw new AggregateError(
            errors,
            "lease loss lifecycle or teardown failed",
          );
        }
      }, { interruptibleNativeProxy: true });
    },
  );
}
