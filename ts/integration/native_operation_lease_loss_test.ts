import assert from "node:assert/strict";
import { Readable, Writable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import {
  apps,
  apis,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

for (const mode of [
  "lease-loss",
  "pending-control",
  "unreadable-state",
  "unreadable-signals",
] as const) {
  const unreadable =
    mode === "unreadable-state" || mode === "unreadable-signals";
  const pendingControl = mode === "pending-control";
  test(
    {
      "lease-loss":
        "native operation lease loss joins cleanup and releases the original provider gate",
      "pending-control":
        "native pending control remains polled while heartbeat awaits its gate",
      "unreadable-state":
        "an execution whose state read failed cannot cancel, acknowledge or complete after storage recovers",
      "unreadable-signals":
        "an execution whose signal read failed cannot write after storage recovers",
    }[mode],
    async () => {
      await withTrellisRuntime(
        async (runtime) => {
          const identity = await runtime.registerService({
            name: "lease-loss-provider",
            contract: participants.OperationProvider.participant,
          });
          const child = spawnTracked(
            "setsid",
            rustFixtureArgv("operation_lease_loss"),
            {
              env: {
                ...process.env,
                TRELLIS_URL: runtime.oauthIssuer,
                TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
                TRELLIS_IDENTITY_SEED: identity.seed,
              },
            },
          );
          child.stderr.pipe(process.stderr);
          const writer = Writable.toWeb(child.stdin).getWriter();
          const lines: string[] = [];
          const output = (async () => {
            let pending = "";
            for await (const text of Readable.toWeb(child.stdout).pipeThrough(
              new TextDecoderStream(),
            )) {
              pending += text;
              const complete = pending.split("\n");
              pending = complete.pop() ?? "";
              lines.push(...complete);
            }
            if (pending) lines.push(pending);
          })();
          let exited = false;
          const status = child.exited.then((value) => {
            exited = true;
            return value;
          });
          const event = (value: string, timeoutMs = 15_000) =>
            runtime.waitFor(
              async () => {
                if (lines.includes(value)) return true;
                if (exited) {
                  throw new Error(
                    `fixture exited before ${value}: ${JSON.stringify(
                      await status,
                    )}; ${lines.join(", ")}`,
                  );
                }
                return false;
              },
              { timeoutMs },
            );
          const errors: unknown[] = [];
          const gate = runtime.nativeTransportGate();
          let hold: ReturnType<typeof gate.armResponseHold> | undefined;
          try {
            const connection = await runtime.waitFor(
              () =>
                gate
                  .connections()
                  .find((connection) =>
                    connection.subs.some(
                      (sub) =>
                        sub.subject ===
                        apis.runtime.API.actions["operation:Work"].subject,
                    ),
                  ) ?? false,
              { timeoutMs: 60_000 },
            );
            const caller = await runtime.connectClient({
              name: "lease-loss-caller",
              contract: apps.Caller.request,
            });
            const operation = await caller
              .work({ value: mode })
              .start()
              .orThrow();
            await event("entered");
            if (unreadable) {
              if (mode === "unreadable-state") {
                await operation
                  .signal("Continue", { value: "continue" })
                  .orThrow();
                await event("signal received");
              }
              // Withhold this provider's next state read, then let the
              // handler make it.
              hold = gate.armResponseHold("$JS.API.DIRECT.GET.", connection.id);
              await writer.write(new TextEncoder().encode("cleanup\n"));
              await hold.held;
              await event(
                mode === "unreadable-state" ? "read failed" : "signals failed",
                30_000,
              );
              await hold.release();
              hold = undefined;
              // Storage answers again within the lease; every write by the
              // execution that saw its state become unreadable is refused.
              await writer.write(new TextEncoder().encode("cleanup\n"));
              await runtime.waitFor(
                () => lines.some((line) => line.startsWith("complete ")),
                { timeoutMs: 15_000 },
              );
              assert.deepEqual(
                lines.filter((line) =>
                  /^(cancel|acknowledge|complete) /.test(line),
                ),
                mode === "unreadable-state"
                  ? [
                      "cancel refused",
                      "acknowledge refused",
                      "complete refused",
                    ]
                  : ["complete refused"],
              );
              const after = await operation.get().orThrow();
              assert.notEqual(after.state, "completed");
              assert.notEqual(after.state, "cancelled");
            }
            if (!unreadable) {
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
              assert.deepEqual(held.connectionId, connection.id);
              assert(held.requestSubject.startsWith("$KV."));

              if (pendingControl) {
                // Progress starts at six seconds; its held ACK timeout spans heartbeat
                // ten. The handler must remain polled while renewal waits for its gate.
                await event("control failed");
                await hold.release();
                await writer.write(new TextEncoder().encode("cleanup\n"));
                const completed = await operation
                  .wait({
                    observationSignal: AbortSignal.timeout(15_000),
                  })
                  .orThrow();
                assert.deepEqual(completed.state, "completed");
                assert.deepEqual(completed.output?.value, "original");
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
                    gate
                      .connections()
                      .some(
                        (candidate) =>
                          !candidate.closed &&
                          candidate.id !== connection.id &&
                          candidate.subs.some(
                            (sub) =>
                              sub.subject ===
                              apis.runtime.API.actions["operation:Work"]
                                .subject,
                          ),
                      ),
                  { timeoutMs: 30_000 },
                );
              }
            }
            const fresh = await caller
              .work({ value: "fresh" })
              .start()
              .orThrow();
            const completed = await fresh
              .wait({
                observationSignal: AbortSignal.timeout(15_000),
              })
              .orThrow();
            assert.deepEqual(completed.state, "completed");
            assert.deepEqual(completed.output?.value, "original");
          } catch (error) {
            errors.push(error);
          } finally {
            runtime.restoreNativeTransport();
            try {
              if (!exited)
                await writer.write(new TextEncoder().encode("stop\n"));
              await runtime.waitFor(() => exited, { timeoutMs: 10_000 });
              assert(
                (await status).code === 0,
                `fixture must exit successfully: ${JSON.stringify(
                  await status,
                )}; ${lines.join(", ")}`,
              );
            } catch (error) {
              errors.push(error);
            } finally {
              if (!exited) child.kill("SIGKILL");
            }
            const cleanup = await Promise.allSettled([
              hold?.release(),
              status,
              output,
            ]);
            for (const result of cleanup) {
              if (result.status === "rejected") errors.push(result.reason);
            }
            writer.releaseLock();
          }
          if (errors.length) {
            // Name every failure: the test reporter prints an AggregateError's
            // members only as "[Error]".
            throw new AggregateError(
              errors,
              `lease loss lifecycle or teardown failed:\n${errors
                .map((error) =>
                  error instanceof Error
                    ? (error.stack ?? error.message)
                    : String(error),
                )
                .join("\n")}`,
            );
          }
        },
        { interruptibleNativeProxy: true },
      );
    },
  );
}
