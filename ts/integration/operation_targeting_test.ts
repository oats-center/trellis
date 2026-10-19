import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { TrellisService } from "@oatscenter/trellis/service";
import { TransportError } from "@oatscenter/trellis/errors";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "targeted Operation admission never falls back and recovers a withheld acceptance only on its persisted owner",
  { timeout: 120_000 },
  async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const identityA = await runtime.registerService({
          name: "target-a",
          deployment: "target-a",
          contract: participants.Provider.participant,
        });
        const identityB = await runtime.registerService({
          name: "target-b",
          deployment: "target-b",
          contract: participants.Provider.participant,
        });
        const a = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.Provider.participant,
          seed: identityA.seed,
          name: "target-a",
        }).orThrow();
        const b = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.Provider.participant,
          seed: identityB.seed,
          name: "target-b",
        }).orThrow();
        const client = await runtime.connectClient({
          name: "target-caller",
          contract: apps.Caller.request,
          timeout: 1000,
        });
        const executions = { a: 0, b: 0 };
        const cleanupReleased = Promise.withResolvers<void>();
        const checkpoint = Promise.withResolvers<void>();
        let cleanups = 0;
        let successor: typeof a | undefined;
        let hold:
          | ReturnType<
              ReturnType<typeof runtime.nativeTransportGate>["armResponseHold"]
            >
          | undefined;
        try {
          await a.handleWork(async ({ op, input, signal }) => {
            if (input.value === "cancelled") {
              if (!signal.aborted)
                await new Promise<void>((resolve) =>
                  signal.addEventListener("abort", () => resolve(), {
                    once: true,
                  }),
                );
              cleanups++;
              await cleanupReleased.promise;
              return;
            }
            executions.a++;
            if (input.value === "takeover") {
              await op.started().orThrow();
              await op
                .progress({
                  value: "checkpoint",
                  nested: { count: 1n, payload: new Uint8Array([1]) },
                })
                .orThrow();
              checkpoint.resolve();
              return op.defer();
            }
            await op.complete({ value: `a:${input.value}` }).orThrow();
          });
          const publications = () =>
            runtime
              .nativeTransportGate()
              .connections()
              .flatMap((connection) => connection.outboundContexts)
              .filter(
                ({ subject }) =>
                  subject ===
                    apis.runtime.API.actions["operation:Work"].subject ||
                  (subject.startsWith("operation.v1.") &&
                    subject.endsWith(".Work")),
              ).length;
          const beforeInvalid = publications();
          // Exercise the ordinary JavaScript boundary: TS rejects this form,
          // but runtime callers must not get a hidden, unrecoverable ID either.
          const builder = client.work({ value: "missing-invocation" });
          const missingId = await Reflect.apply(builder.start, builder, [
            undefined,
            { targetDeploymentId: identityA.deploymentId },
          ]);
          assert(missingId.isErr());
          assert(missingId.error instanceof TransportError);
          assert.equal(missingId.error.code, "trellis.operation.invalid_start");
          for (const options of [
            { invocationId: ulid(), targetDeploymentId: "" },
            { invocationId: ulid(), targetDeploymentId: "not-a-deployment" },
            { invocationId: ulid(), targetDeploymentId: " invalid " },
            {
              invocationId: "not-a-ulid",
              targetDeploymentId: identityA.deploymentId,
            },
          ]) {
            const invalid = await client
              .work({ value: "invalid" })
              .start(undefined, options);
            assert(invalid.isErr());
            assert(invalid.error instanceof TransportError);
            assert.equal(invalid.error.code, "trellis.operation.invalid_start");
          }
          assert.equal(
            publications(),
            beforeInvalid,
            "malformed targeted starts must not publish",
          );
          const canonical = await client
            .work({ value: "canonical" })
            .start()
            .orThrow();
          assert.equal(
            (await canonical.wait().orThrow()).output?.value,
            "a:canonical",
          );

          const pendingB = ulid();
          const unavailable = await client
            .work({ value: "b-start" })
            .start(undefined, {
              invocationId: pendingB,
              targetDeploymentId: identityB.deploymentId,
            });
          assert(
            unavailable.isErr(),
            "an unavailable bound route must not select the available canonical provider",
          );
          assert.equal(executions.a, 1);
          await b.handleWork(async ({ op, input }) => {
            executions.b++;
            await op.complete({ value: `b:${input.value}` }).orThrow();
          });

          const invocationId = ulid();
          const subjectPrefix = "operation.v1.";
          hold = runtime.nativeTransportGate().armResponseHold(subjectPrefix);
          const first = client.work({ value: "accepted" }).start(undefined, {
            invocationId,
            targetDeploymentId: identityA.deploymentId,
          });
          const held = await Promise.race([
            hold.held,
            Promise.resolve(first).then(() => {
              throw new Error(
                "the start completed before its accepted broker reply was held",
              );
            }),
          ]);
          assert(held.requestSubject.startsWith(subjectPrefix));
          const uncertain = await first;
          assert(
            uncertain.isErr(),
            "the real accepted reply must remain unavailable to the caller",
          );
          assert(uncertain.error instanceof TransportError);
          assert.equal(uncertain.error.code, "trellis.request.timeout");
          const recovered = await client
            .work({ value: "accepted" })
            .start(undefined, {
              invocationId,
              targetDeploymentId: identityA.deploymentId,
            })
            .orThrow();
          assert.equal(recovered.deploymentId, identityA.deploymentId);
          assert.equal(
            (await recovered.wait().orThrow()).output?.value,
            "a:accepted",
          );
          assert.deepEqual(executions, { a: 2, b: 0 });
          await hold.release();
          const conflict = await client
            .work({ value: "different" })
            .start(undefined, {
              invocationId,
              targetDeploymentId: identityA.deploymentId,
            });
          assert(conflict.isErr());
          const independent = await client
            .work({ value: "accepted" })
            .start(undefined, {
              invocationId,
              targetDeploymentId: identityB.deploymentId,
            })
            .orThrow();
          assert.equal(independent.id, recovered.id);
          assert.equal(independent.deploymentId, identityB.deploymentId);
          assert.equal(
            (await independent.wait().orThrow()).output?.value,
            "b:accepted",
          );
          assert.deepEqual(executions, { a: 2, b: 1 });
          const bytes = new Uint8Array([0, 1, 2, 127, 255]);
          let uploads = 0;
          await a.handleUpload(async ({ input, transfer, op }) => {
            assert(transfer);
            uploads++;
            assert.deepEqual(await transfer.bytes().orThrow(), bytes);
            await op.complete(input).orThrow();
          });
          await b.handleUpload(async () => {
            throw new Error("a targeted upload must not execute on B");
          });
          const uploaded = await client
            .upload({ value: "targeted-upload" })
            .transfer(bytes)
            .start(undefined, {
              invocationId: ulid(),
              targetDeploymentId: identityA.deploymentId,
            })
            .orThrow();
          assert.equal(uploaded.operation.deploymentId, identityA.deploymentId);
          assert.equal(
            (await uploaded.wait().orThrow()).terminal.output?.value,
            "targeted-upload",
          );
          assert.equal(uploads, 1);
          const cancelledId = ulid();
          const cancelled = await client
            .work({ value: "cancelled" })
            .start(undefined, {
              invocationId: cancelledId,
              targetDeploymentId: identityA.deploymentId,
              cancellationRequested: true,
            })
            .orThrow();
          await runtime.waitFor(() => cleanups === 1);
          const normalReplay = await client
            .work({ value: "cancelled" })
            .start(undefined, {
              invocationId: cancelledId,
              targetDeploymentId: identityA.deploymentId,
            })
            .orThrow();
          assert.equal(normalReplay.id, cancelled.id);
          assert.notEqual(
            (await normalReplay.get().orThrow()).state,
            "cancelled",
          );
          cleanupReleased.resolve();
          assert.equal((await cancelled.wait().orThrow()).state, "cancelled");
          assert.equal(cleanups, 1);
          assert.deepEqual(executions, { a: 2, b: 1 });

          const takeoverId = ulid();
          const pending = await client
            .work({ value: "takeover" })
            .start(undefined, {
              invocationId: takeoverId,
              targetDeploymentId: identityA.deploymentId,
            })
            .orThrow();
          await checkpoint.promise;
          const replica = await runtime.registerService({
            name: "target-a-replica",
            deployment: "target-a",
            contract: participants.Provider.participant,
          });
          assert.equal(replica.deploymentId, identityA.deploymentId);
          successor = await TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
            participant: participants.Provider.participant,
            seed: replica.seed,
            name: "target-a-replica",
          }).orThrow();
          let resumptions = 0;
          await successor.handleWork(
            async ({ op, input, resuming, progress }) => {
              assert.equal(input.value, "takeover");
              assert.equal(resuming, true);
              assert.equal(progress?.value, "checkpoint");
              resumptions++;
              await op.complete({ value: "successor" }).orThrow();
            },
          );
          const retry = await client
            .work({ value: "takeover" })
            .start(undefined, {
              invocationId: takeoverId,
              targetDeploymentId: identityA.deploymentId,
            })
            .orThrow();
          assert.equal(retry.id, pending.id);
          await a.stop();
          await a.wait();
          const restored = client.work.resume(
            JSON.parse(JSON.stringify(pending)),
          );
          const completed = await restored
            .wait({ observationSignal: AbortSignal.timeout(45_000) })
            .orThrow();
          assert.equal(completed.output?.value, "successor");
          assert.equal(restored.deploymentId, identityA.deploymentId);
          assert.equal(resumptions, 1);
          assert.deepEqual(executions, { a: 3, b: 1 });
        } finally {
          cleanupReleased.resolve();
          await hold?.release();
          await client.connection.close();
          await Promise.all([a.stop(), b.stop(), successor?.stop()]);
          await Promise.all([a.wait(), b.wait(), successor?.wait()]);
        }
      },
      { interruptibleNativeProxy: true },
    );
  },
);
