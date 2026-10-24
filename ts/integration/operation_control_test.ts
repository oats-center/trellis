import { TrellisService } from "@oatscenter/trellis/service";
import {
  OperationNotFoundError,
  TransportError,
  UnexpectedError,
} from "@oatscenter/trellis/errors";
import { isOk, Result } from "@oatscenter/result";
import { inspect } from "node:util";
import { ulid } from "ulid";
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("Operation reads retain committed progress after an oversized persistence failure", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "committed-progress-read",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      name: "committed-progress-read",
      seed: identity.seed,
    }).orThrow();
    const exited = service.wait().catch((cause: unknown) => cause);
    const finish = Promise.withResolvers<void>();
    let writeFailed: boolean | undefined;
    let handlerFailure: unknown;
    await service.handleWork(async ({ op }) => {
      try {
        await op.started().orThrow();
        await op
          .progress({
            value: "committed checkpoint",
            nested: { count: 1n, payload: new Uint8Array([1]) },
          })
          .orThrow();
        // The production progress-size guard rejects this before persistence.
        // A public read must retain the last successfully stored progress.
        await assert.rejects(async () => {
          await op
            .progress({
              value: "x".repeat(128 * 1024),
              nested: { count: 2n, payload: new Uint8Array([2]) },
            })
            .orThrow();
        });
        writeFailed = true;
      } catch (cause) {
        handlerFailure = cause;
        writeFailed = false;
      }
      await finish.promise;
      return op.defer();
    });
    const caller = await runtime.connectClient({
      name: "committed-progress-reader",
      contract: apps.Caller.request,
    });
    try {
      const operation = await caller
        .work({ value: "read-after-failure" })
        .start()
        .orThrow();
      await runtime.waitFor(() => writeFailed !== undefined);
      assert.equal(writeFailed, true, inspect(handlerFailure, { depth: 8 }));
      assert.equal(
        (await operation.get().orThrow()).progress?.value,
        "committed checkpoint",
      );
      const observation = await operation
        .live({
          signal: AbortSignal.timeout(5_000),
        })
        .orThrow();
      let observed = false;
      for await (const event of observation) {
        if (event.type !== "progress") continue;
        assert.equal(event.progress.value, "committed checkpoint");
        observed = true;
        break;
      }
      assert(observed, "a new observation must deliver persisted progress");
    } finally {
      finish.resolve();
      await caller.connection.close();
      await service.stop();
      await exited;
    }
  });
});

test("serialized references keep controls on their deployment when a canonical start reaches another deployment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const firstIdentity = await runtime.registerService({
      name: "reference-owner-a",
      deployment: "reference-owner-a",
      contract: participants.Provider.participant,
    });
    const secondIdentity = await runtime.registerService({
      name: "reference-owner-b",
      deployment: "reference-owner-b",
      contract: participants.Provider.participant,
    });
    const connect = (seed: typeof firstIdentity.seed, name: string) =>
      TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        seed,
        name,
      }).orThrow();
    const first = await connect(firstIdentity.seed, "reference-owner-a");
    const client = await runtime.connectClient({
      name: "reference-caller",
      contract: apps.Caller.request,
    });
    let second: typeof first | undefined;
    let restarted: typeof first | undefined;
    try {
      await first.handleWork(
        async ({ op }) =>
          await op.complete({ value: "first-deployment" }).orThrow(),
      );
      const invocationId = ulid();
      const original = await client
        .work({ value: "same-invocation" })
        .start(undefined, { invocationId })
        .orThrow();
      assert.deepEqual(
        (await original.wait().orThrow()).output?.value,
        "first-deployment",
      );
      const serialized = JSON.stringify(original);
      await first.stop();
      await first.wait();
      second = await connect(secondIdentity.seed, "reference-owner-b");
      await second.handleWork(
        async ({ op }) =>
          await op.complete({ value: "second-deployment" }).orThrow(),
      );
      // Reusing an invocation ID here is an explicit new request, not SDK replay.
      const alternative = await client
        .work({ value: "same-invocation" })
        .start(undefined, { invocationId })
        .orThrow();
      assert.deepEqual(
        (await alternative.wait().orThrow()).output?.value,
        "second-deployment",
      );
      assert.deepEqual(alternative.id, original.id);
      assert(alternative.deploymentId !== original.deploymentId);
      restarted = await connect(firstIdentity.seed, "reference-owner-a");
      await restarted.handleWork(
        async ({ op }) =>
          await op.complete({ value: "unexpected-reexecution" }).orThrow(),
      );
      const restored = client.work.resume(JSON.parse(serialized));
      assert.deepEqual(
        (await restored.get().orThrow()).output?.value,
        "first-deployment",
      );
      assert.deepEqual(
        (await restored.wait().orThrow()).output?.value,
        "first-deployment",
      );
    } finally {
      await client.connection.close();
      await Promise.all([first.stop(), second?.stop(), restarted?.stop()]);
      await Promise.all([first.wait(), second?.wait(), restarted?.wait()]);
    }
  });
});

test("restarted provider recovers nonterminal work among retained terminal history", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "recovery-history",
      contract: participants.Provider.participant,
    });
    const connect = () =>
      TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        name: "recovery-history",
        seed: identity.seed,
      }).orThrow();
    const original = await connect();
    const deferred = Promise.withResolvers<void>();
    await original.handleWork(async ({ input, op }) => {
      if (input.value !== "resume") {
        return await op.complete({ value: input.value }).orThrow();
      }
      await op.started().orThrow();
      await op
        .progress({
          value: "checkpoint",
          nested: { count: 1n, payload: new Uint8Array([1, 2]) },
        })
        .orThrow();
      deferred.resolve();
      return op.defer();
    });
    const client = await runtime.connectClient({
      name: "recovery-history-caller",
      contract: apps.Caller.request,
    });
    let replacement: typeof original | undefined;
    try {
      for (let index = 0; index < 20; index++) {
        const terminal = await client
          .work({ value: `terminal-${index}` })
          .start()
          .orThrow();
        assert.deepEqual(
          (await terminal.wait().orThrow()).output?.value,
          `terminal-${index}`,
        );
      }
      const pending = await client.work({ value: "resume" }).start().orThrow();
      const restored = client.work.resume(JSON.parse(JSON.stringify(pending)));
      await deferred.promise;
      await original.stop();
      await original.wait();
      replacement = await connect();
      let resumed = 0;
      await replacement.handleWork(
        async ({ input, op, resuming, progress }) => {
          assert.deepEqual(input.value, "resume");
          assert.deepEqual(resuming, true);
          assert.deepEqual(progress?.value, "checkpoint");
          assert.deepEqual(progress?.nested, {
            count: 1n,
            payload: new Uint8Array([1, 2]),
          });
          resumed++;
          return await op.complete({ value: "recovered" }).orThrow();
        },
      );
      const result = await restored
        .wait({
          observationSignal: AbortSignal.timeout(45_000),
        })
        .orThrow();
      assert.deepEqual(
        result.state,
        "completed",
        inspect(result, { depth: 8 }),
      );
      assert.deepEqual(result.output?.value, "recovered");
      assert.deepEqual(resumed, 1);
    } finally {
      await client.connection.close();
      await original.stop();
      await replacement?.stop();
      await replacement?.wait();
    }
  });
});

test("generated operation control requires the local owner fence", async () => {
  await withTrellisRuntime(async (runtime) => {
    const firstIdentity = await runtime.registerService({
      name: "operation-control-replicas",
      contract: participants.Provider.participant,
    });
    const secondIdentity = await runtime.services.createInstance({
      name: "operation-control-replica-b",
      contract: participants.Provider.participant,
    });
    const services = await Promise.all(
      [firstIdentity, secondIdentity].map((identity) =>
        TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.Provider.participant,
          name: "operation-control-replicas",
          seed: identity.seed,
        }).orThrow(),
      ),
    );
    const firstStarted = Promise.withResolvers<number>();
    const recovered = Promise.withResolvers<void>();
    const releaseRecovered = Promise.withResolvers<void>();
    let executions = 0;
    for (const [index, service] of services.entries()) {
      await service.handleWork(async ({ input, op }) => {
        await op.started().orThrow();
        if (++executions === 1) {
          firstStarted.resolve(index);
          return await new Promise<never>(() => {});
        }
        recovered.resolve();
        await releaseRecovered.promise;
        return await op.complete({ value: input.value }).orThrow();
      });
    }
    const client = await runtime.connectClient({
      name: "operation-control-caller",
      contract: apps.Caller.request,
    });
    const exits = services.map((service) => service.wait());
    try {
      const operation = await client
        .work({ value: "control" })
        .start()
        .orThrow();
      const owner = await firstStarted.promise;
      const other = 1 - owner;
      const active = await services[owner].handleWork
        .control(operation.id)
        .orThrow();
      assert.deepEqual(active.id, operation.id);
      const absent =
        await services[owner].handleWork.control("missing-operation");
      assert(absent.isErr());
      assert(absent.error instanceof OperationNotFoundError);
      const nonlocal = await services[other].handleWork.control(operation.id);
      assert(nonlocal.isErr());
      assert(nonlocal.error instanceof OperationNotFoundError);

      await services[owner].stop();
      await exits[owner];
      await recovered.promise;
      await assert.rejects(() =>
        services[owner].handleWork.control(operation.id).orThrow(),
      );
      const successor = await services[other].handleWork
        .control(operation.id)
        .orThrow();
      assert.deepEqual(successor.id, operation.id);
      releaseRecovered.resolve();
      assert.deepEqual((await operation.wait().orThrow()).state, "completed");
      await runtime.waitFor(async () =>
        (await services[other].handleWork.control(operation.id)).isErr(),
      );
      const stale = await services[other].handleWork.control(operation.id);
      assert(stale.isErr());
      assert(stale.error instanceof OperationNotFoundError);
    } finally {
      releaseRecovered.resolve();
      await client.connection.close();
      await Promise.all(services.map((service) => service.stop()));
      await Promise.all(exits);
    }
  });
});

test("generated reconciliation routes to the fenced owner", async () => {
  await withTrellisRuntime(async (runtime) => {
    const firstIdentity = await runtime.registerService({
      name: "operation-reconciliation-replicas",
      contract: participants.Provider.participant,
    });
    const secondIdentity = await runtime.services.createInstance({
      name: "operation-reconciliation-replica-b",
      contract: participants.Provider.participant,
    });
    const services = await Promise.all(
      [firstIdentity, secondIdentity].map((identity) =>
        TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.Provider.participant,
          name: "operation-reconciliation-replicas",
          seed: identity.seed,
        }).orThrow(),
      ),
    );
    const initial = Promise.withResolvers<number>();
    let executions = 0;
    for (const [index, service] of services.entries()) {
      await service.handleWork(async ({ input, op }) => {
        if (++executions === 1) {
          await op.started().orThrow();
          initial.resolve(index);
          return op.defer();
        }
        assert.deepEqual(input.value, "reconcile");
        return await op.complete({ value: input.value }).orThrow();
      });
    }
    const client = await runtime.connectClient({
      name: "operation-reconciliation-caller",
      contract: apps.Caller.request,
    });
    try {
      const operation = await client
        .work({ value: "reconcile" })
        .start()
        .orThrow();
      const owner = await initial.promise;
      const other = 1 - owner;
      const nonlocal = await services[other].handleWork.control(operation.id);
      assert(nonlocal.isErr());
      assert(nonlocal.error instanceof OperationNotFoundError);
      const absent =
        await services[other].handleWork.reconcile("missing-operation");
      assert(absent.isErr());
      assert(absent.error instanceof OperationNotFoundError);
      const result = await services[other].handleWork
        .reconcile(operation.id)
        .orThrow();
      assert.deepEqual(result.state, "completed");
      assert.deepEqual(executions, 2);
      assert.deepEqual((await operation.wait().orThrow()).state, "completed");
    } finally {
      await client.connection.close();
      await Promise.all(services.map((service) => service.stop()));
      await Promise.all(services.map((service) => service.wait()));
    }
  });
});

test("handler cancellation stays nonterminal until cleanup returns", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "operation-cleanup",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      name: "operation-cleanup",
      seed: identity.seed,
    }).orThrow();
    const requested = Promise.withResolvers<{
      state: string;
      aborted: boolean;
    }>();
    const releaseCleanup = Promise.withResolvers<void>();
    await service.handleWork(async ({ op, signal }) => {
      await op.started().orThrow();
      const cancellation = await op.cancel().orThrow();
      requested.resolve({
        state: cancellation.state,
        aborted: signal.aborted,
      });
      await releaseCleanup.promise;
    });
    const client = await runtime.connectClient({
      name: "operation-cleanup-caller",
      contract: apps.Caller.request,
    });
    const exit = service.wait();
    try {
      const operation = await client
        .work({ value: "cleanup" })
        .start()
        .orThrow();
      const cancellation = await requested.promise;
      // The handler cancellation request is durable and nonterminal: its
      // terminal state follows cleanup, and the handler signal is aborted.
      assert(cancellation.state !== "cancelled");
      assert.deepEqual(cancellation.aborted, true);
      releaseCleanup.resolve();
      const terminal = await runtime.waitFor(async () => {
        const snapshot = await operation.get().orThrow();
        return snapshot.state === "cancelled" ? snapshot : false;
      });
      assert.deepEqual(terminal.state, "cancelled");
    } finally {
      releaseCleanup.resolve();
      await client.connection.close();
      await service.stop();
      await exit;
    }
  });
});

test("interrupted cancellation recovers through cleanup without rerunning business work", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const firstIdentity = await runtime.registerService({
        name: "operation-interrupted-cancel-replicas",
        contract: participants.Provider.participant,
      });
      const secondIdentity = await runtime.services.createInstance({
        name: "operation-interrupted-cancel-replica-b",
        contract: participants.Provider.participant,
      });
      const services = await Promise.all(
        [firstIdentity, secondIdentity].map((identity) =>
          TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
            participant: participants.Provider.participant,
            name: "operation-interrupted-cancel-replicas",
            seed: identity.seed,
          }).orThrow(),
        ),
      );
      const firstStarted = Promise.withResolvers<number>();
      const cancellationObserved = Promise.withResolvers<void>();
      let executions = 0;
      let businessExecutions = 0;
      for (const [index, service] of services.entries()) {
        await service.handleWork(async ({ op, signal }) => {
          const count = ++executions;
          if (signal.aborted) {
            assert.deepEqual(count, 2);
            return;
          }
          businessExecutions++;
          await op.started().orThrow();
          if (count === 1) {
            firstStarted.resolve(index);
            await new Promise<void>((resolve) => {
              if (signal.aborted) {
                resolve();
              } else {
                signal.addEventListener("abort", () => resolve(), {
                  once: true,
                });
              }
            });
            cancellationObserved.resolve();
            return await new Promise<never>(() => {});
          }
          throw new Error("recovery entered business work");
        });
      }
      const client = await runtime.connectClient({
        name: "operation-interrupted-cancel-caller",
        contract: apps.Caller.request,
      });
      const exits = services.map((service) => service.wait());
      try {
        const operation = await client
          .work({ value: "interrupted-cancel" })
          .start()
          .orThrow();
        const owner = await firstStarted.promise;
        const controlSubject = `operation.v1.${Buffer.from(apis.runtime.API.identity).toString("base64url")}.${Buffer.from(firstIdentity.deploymentId).toString("base64url")}.Work.control`;
        const reply = runtime
          .nativeTransportGate()
          .armResponseHold(controlSubject);
        try {
          // Withhold the actual signed acknowledgement. Losing the owner can
          // lose this reply; a timeout does not retract durable cancellation.
          const cancellation = operation.cancel();
          await cancellationObserved.promise;
          await reply.held;
          await services[owner].stop();
          await exits[owner];
          const terminal = runtime.waitFor(
            async () => {
              const snapshot = await operation.get().orThrow();
              return snapshot.state === "cancelled" ? snapshot : false;
            },
            { timeoutMs: 90_000 },
          );
          const [outcome, snapshot] = await Promise.all([
            cancellation,
            terminal,
          ]);
          assert(outcome.isErr());
          assert(outcome.error instanceof TransportError);
          assert.equal(outcome.error.code, "trellis.request.timeout");
          assert.equal(snapshot.state, "cancelled");
          assert.deepEqual(executions, 2);
          assert.deepEqual(businessExecutions, 1);
        } finally {
          await reply.release();
        }
      } finally {
        await client.connection.close();
        await Promise.all(services.map((service) => service.stop()));
        await Promise.all(exits);
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("cancellation request serializes with handler completion", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "operation-cancel-race",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      name: "operation-cancel-race",
      seed: identity.seed,
    }).orThrow();
    const raced = Promise.withResolvers<{
      completed: boolean;
      cancelled: boolean;
    }>();
    await service.handleWork(async ({ op }) => {
      await op.started().orThrow();
      const [completed, cancelled] = await Promise.all([
        op.complete({ value: "raced" }),
        op.cancel(),
      ]);
      raced.resolve({
        completed: isOk(completed),
        cancelled: isOk(cancelled),
      });
    });
    const client = await runtime.connectClient({
      name: "operation-cancel-race-caller",
      contract: apps.Caller.request,
    });
    const exit = service.wait();
    try {
      const operation = await client.work({ value: "race" }).start().orThrow();
      const outcome = await raced.promise;
      // Completion enqueued first on the shared operation frame, so it wins and
      // the cancellation request is rejected without mutating the record.
      assert.deepEqual(outcome.completed, true);
      assert.deepEqual(outcome.cancelled, false);
      assert.deepEqual((await operation.wait().orThrow()).state, "completed");
    } finally {
      await client.connection.close();
      await service.stop();
      await exit;
    }
  });
});

test("cancel-first admission retains failed cleanup and replays terminal cancellation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "operation-cancel-first",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      name: "operation-cancel-first",
      seed: identity.seed,
    }).orThrow();
    let cleanupAttempts = 0;
    let businessAttempts = 0;
    const entered = Promise.withResolvers<void>();
    await service.handleWork(({ signal, op, input }) => {
      if (!signal.aborted) {
        businessAttempts++;
        if (input.value === "normal-first") return op.defer();
        return { value: "business" };
      }
      cleanupAttempts++;
      if (cleanupAttempts === 1) {
        entered.resolve();
        return op.defer();
      }
      if (cleanupAttempts === 2) throw new Error("cleanup interrupted");
      if (cleanupAttempts === 3) {
        return Result.err(
          new UnexpectedError({ cause: new Error("cleanup failed") }),
        );
      }
      return;
    });
    const client = await runtime.connectClient({
      name: "operation-cancel-first-caller",
      contract: apps.Caller.request,
    });
    try {
      const invocationId = ulid();
      const operation = await client
        .work({ value: "cancel-first" })
        .start(undefined, {
          invocationId,
          cancellationRequested: true,
        })
        .orThrow();
      await entered.promise;
      const replay = await client
        .work({ value: "cancel-first" })
        .start(undefined, {
          invocationId,
        })
        .orThrow();
      assert.deepEqual(replay.id, operation.id);
      assert.deepEqual((await operation.get().orThrow()).state, "pending");
      for (let attempt = 2; attempt <= 3; attempt++) {
        const snapshot = await service.handleWork
          .reconcile(operation.id)
          .orThrow();
        assert.deepEqual(snapshot.state, "pending");
        assert.deepEqual(cleanupAttempts, attempt);
      }
      const terminal = await service.handleWork
        .reconcile(operation.id)
        .orThrow();
      assert.deepEqual(terminal.state, "cancelled");
      const terminalReplay = await client
        .work({ value: "cancel-first" })
        .start(undefined, {
          invocationId,
          cancellationRequested: true,
        })
        .orThrow();
      const replaySnapshot = await terminalReplay.get().orThrow();
      assert.deepEqual(replaySnapshot.state, terminal.state);
      assert.deepEqual(replaySnapshot.revision, terminal.revision);
      assert.deepEqual(replaySnapshot.completedAt, terminal.completedAt);
      assert.deepEqual(cleanupAttempts, 4);
      assert.deepEqual(businessAttempts, 0);

      const normalId = ulid();
      const normal = await client
        .work({ value: "normal-first" })
        .start(undefined, {
          invocationId: normalId,
        })
        .orThrow();
      await runtime.waitFor(() => businessAttempts === 1);
      const cancelledReplay = await client
        .work({ value: "normal-first" })
        .start(undefined, {
          invocationId: normalId,
          cancellationRequested: true,
        })
        .orThrow();
      assert.deepEqual(cancelledReplay.id, normal.id);
      await client
        .work({ value: "normal-first" })
        .start(undefined, {
          invocationId: normalId,
        })
        .orThrow();
      assert.deepEqual((await normal.wait().orThrow()).state, "cancelled");
      assert.deepEqual(businessAttempts, 1);
      assert.deepEqual(cleanupAttempts, 5);
    } finally {
      await client.connection.close();
      await service.stop();
      await service.wait();
    }
  });
});
