import { TrellisService } from "@oatscenter/trellis/service";
import {
  OperationNotFoundError,
  UnexpectedError,
} from "@oatscenter/trellis/errors";
import { isOk, Result } from "@oatscenter/result";
import { ulid } from "ulid";
import { assert, assertEquals, assertRejects } from "@std/assert";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("restarted provider recovers nonterminal work among retained terminal history", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "recovery-history",
      contract: participants.Provider.participant,
    });
    const connect = () =>
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
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
      await op.progress({
        value: "checkpoint",
        nested: { count: 1n, payload: new Uint8Array([1, 2]) },
      }).orThrow();
      deferred.resolve();
      return op.defer();
    });
    const client = await runtime.connectClient({
      name: "recovery-history-caller",
      contract: participants.Caller.participant,
    });
    let replacement: typeof original | undefined;
    try {
      for (let index = 0; index < 20; index++) {
        const terminal = await client.work({ value: `terminal-${index}` })
          .start().orThrow();
        assertEquals(
          (await terminal.wait().orThrow()).output?.value,
          `terminal-${index}`,
        );
      }
      const pending = await client.work({ value: "resume" }).start().orThrow();
      await deferred.promise;
      await original.stop();
      await original.wait();
      replacement = await connect();
      let resumed = 0;
      await replacement.handleWork(
        async ({ input, op, resuming, progress }) => {
          assertEquals(input.value, "resume");
          assertEquals(resuming, true);
          assertEquals(progress?.value, "checkpoint");
          assertEquals(progress?.nested, {
            count: 1n,
            payload: new Uint8Array([1, 2]),
          });
          resumed++;
          return await op.complete({ value: "recovered" }).orThrow();
        },
      );
      const result = await pending.wait({
        observationSignal: AbortSignal.timeout(45_000),
      }).orThrow();
      assertEquals(
        result.state,
        "completed",
        Deno.inspect(result, { depth: 8 }),
      );
      assertEquals(result.output?.value, "recovered");
      assertEquals(resumed, 1);
    } finally {
      await client.connection.close();
      await original.stop();
      await replacement?.stop();
      await replacement?.wait();
    }
  });
});

Deno.test("generated operation control requires the local owner fence", async () => {
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
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          name: "operation-control-replicas",
          seed: identity.seed,
        }).orThrow()
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
      contract: participants.Caller.participant,
    });
    const exits = services.map((service) => service.wait());
    try {
      const operation = await client.work({ value: "control" }).start()
        .orThrow();
      const owner = await firstStarted.promise;
      const other = 1 - owner;
      const active = await services[owner].handleWork.control(operation.id)
        .orThrow();
      assertEquals(active.id, operation.id);
      const absent = await services[owner].handleWork.control(
        "missing-operation",
      );
      assert(absent.isErr());
      assert(absent.error instanceof OperationNotFoundError);
      const nonlocal = await services[other].handleWork.control(operation.id);
      assert(nonlocal.isErr());
      assert(nonlocal.error instanceof OperationNotFoundError);

      await services[owner].stop();
      await exits[owner];
      await recovered.promise;
      await assertRejects(() =>
        services[owner].handleWork.control(operation.id).orThrow()
      );
      const successor = await services[other].handleWork.control(operation.id)
        .orThrow();
      assertEquals(successor.id, operation.id);
      releaseRecovered.resolve();
      assertEquals((await operation.wait().orThrow()).state, "completed");
      await runtime.waitFor(async () =>
        (await services[other].handleWork.control(operation.id)).isErr()
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

Deno.test("generated reconciliation routes to the fenced owner", async () => {
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
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          name: "operation-reconciliation-replicas",
          seed: identity.seed,
        }).orThrow()
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
        assertEquals(input.value, "reconcile");
        return await op.complete({ value: input.value }).orThrow();
      });
    }
    const client = await runtime.connectClient({
      name: "operation-reconciliation-caller",
      contract: participants.Caller.participant,
    });
    try {
      const operation = await client.work({ value: "reconcile" }).start()
        .orThrow();
      const owner = await initial.promise;
      const other = 1 - owner;
      const nonlocal = await services[other].handleWork.control(operation.id);
      assert(nonlocal.isErr());
      assert(nonlocal.error instanceof OperationNotFoundError);
      const absent = await services[other].handleWork.reconcile(
        "missing-operation",
      );
      assert(absent.isErr());
      assert(absent.error instanceof OperationNotFoundError);
      const result = await services[other].handleWork.reconcile(operation.id)
        .orThrow();
      assertEquals(result.state, "completed");
      assertEquals(executions, 2);
      assertEquals((await operation.wait().orThrow()).state, "completed");
    } finally {
      await client.connection.close();
      await Promise.all(services.map((service) => service.stop()));
      await Promise.all(services.map((service) => service.wait()));
    }
  });
});

Deno.test("handler cancellation stays nonterminal until cleanup returns", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "operation-cleanup",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: "operation-cleanup",
      seed: identity.seed,
    }).orThrow();
    const requested = Promise.withResolvers<
      { state: string; aborted: boolean }
    >();
    const releaseCleanup = Promise.withResolvers<void>();
    await service.handleWork(async ({ op, signal }) => {
      await op.started().orThrow();
      const cancellation = await op.cancel().orThrow();
      requested.resolve({ state: cancellation.state, aborted: signal.aborted });
      await releaseCleanup.promise;
    });
    const client = await runtime.connectClient({
      name: "operation-cleanup-caller",
      contract: participants.Caller.participant,
    });
    const exit = service.wait();
    try {
      const operation = await client.work({ value: "cleanup" }).start()
        .orThrow();
      const cancellation = await requested.promise;
      // The handler cancellation request is durable and nonterminal: its
      // terminal state follows cleanup, and the handler signal is aborted.
      assert(cancellation.state !== "cancelled");
      assertEquals(cancellation.aborted, true);
      releaseCleanup.resolve();
      const terminal = await runtime.waitFor(async () => {
        const snapshot = await operation.get().orThrow();
        return snapshot.state === "cancelled" ? snapshot : false;
      });
      assertEquals(terminal.state, "cancelled");
    } finally {
      releaseCleanup.resolve();
      await client.connection.close();
      await service.stop();
      await exit;
    }
  });
});

Deno.test("interrupted cancellation recovers through cleanup without rerunning business work", async () => {
  await withTrellisRuntime(async (runtime) => {
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
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          name: "operation-interrupted-cancel-replicas",
          seed: identity.seed,
        }).orThrow()
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
          assertEquals(count, 2);
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
              signal.addEventListener("abort", () => resolve(), { once: true });
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
      contract: participants.Caller.participant,
    });
    const exits = services.map((service) => service.wait());
    try {
      const operation = await client.work({ value: "interrupted-cancel" })
        .start().orThrow();
      const owner = await firstStarted.promise;
      // The caller-visible cancel request resolves only after cleanup, so fire
      // it without awaiting and wait for the handler to observe the abort.
      const cancellation = operation.cancel();
      await cancellationObserved.promise;
      // The owner dies mid-cleanup. The durable cancellation request survives,
      // so the successor must re-enter cleanup with an already-aborted signal.
      await services[owner].stop();
      await exits[owner];
      const terminal = await Promise.race([
        cancellation.orThrow(),
        new Promise<never>((_, reject) =>
          setTimeout(
            () => reject(new Error("cancellation did not finalize")),
            90_000,
          )
        ),
      ]);
      assertEquals(terminal.state, "cancelled");
      assertEquals(executions, 2);
      assertEquals(businessExecutions, 1);
    } finally {
      await client.connection.close();
      await Promise.all(services.map((service) => service.stop()));
      await Promise.all(exits);
    }
  });
});

Deno.test("cancellation request serializes with handler completion", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "operation-cancel-race",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: "operation-cancel-race",
      seed: identity.seed,
    }).orThrow();
    const raced = Promise.withResolvers<
      { completed: boolean; cancelled: boolean }
    >();
    await service.handleWork(async ({ op }) => {
      await op.started().orThrow();
      const [completed, cancelled] = await Promise.all([
        op.complete({ value: "raced" }),
        op.cancel(),
      ]);
      raced.resolve({ completed: isOk(completed), cancelled: isOk(cancelled) });
    });
    const client = await runtime.connectClient({
      name: "operation-cancel-race-caller",
      contract: participants.Caller.participant,
    });
    const exit = service.wait();
    try {
      const operation = await client.work({ value: "race" }).start().orThrow();
      const outcome = await raced.promise;
      // Completion enqueued first on the shared operation frame, so it wins and
      // the cancellation request is rejected without mutating the record.
      assertEquals(outcome.completed, true);
      assertEquals(outcome.cancelled, false);
      assertEquals((await operation.wait().orThrow()).state, "completed");
    } finally {
      await client.connection.close();
      await service.stop();
      await exit;
    }
  });
});

Deno.test("cancel-first admission retains failed cleanup and replays terminal cancellation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "operation-cancel-first",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
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
      contract: participants.Caller.participant,
    });
    try {
      const invocationId = ulid();
      const operation = await client.work({ value: "cancel-first" }).start(
        undefined,
        {
          invocationId,
          cancellationRequested: true,
        },
      ).orThrow();
      await entered.promise;
      const replay = await client.work({ value: "cancel-first" }).start(
        undefined,
        {
          invocationId,
        },
      ).orThrow();
      assertEquals(replay.id, operation.id);
      assertEquals((await operation.get().orThrow()).state, "pending");
      for (let attempt = 2; attempt <= 3; attempt++) {
        const snapshot = await service.handleWork.reconcile(operation.id)
          .orThrow();
        assertEquals(snapshot.state, "pending");
        assertEquals(cleanupAttempts, attempt);
      }
      const terminal = await service.handleWork.reconcile(operation.id)
        .orThrow();
      assertEquals(terminal.state, "cancelled");
      const terminalReplay = await client.work({ value: "cancel-first" }).start(
        undefined,
        {
          invocationId,
          cancellationRequested: true,
        },
      ).orThrow();
      const replaySnapshot = await terminalReplay.get().orThrow();
      assertEquals(replaySnapshot.state, terminal.state);
      assertEquals(replaySnapshot.revision, terminal.revision);
      assertEquals(replaySnapshot.completedAt, terminal.completedAt);
      assertEquals(cleanupAttempts, 4);
      assertEquals(businessAttempts, 0);

      const normalId = ulid();
      const normal = await client.work({ value: "normal-first" }).start(
        undefined,
        {
          invocationId: normalId,
        },
      ).orThrow();
      await runtime.waitFor(() => businessAttempts === 1);
      const cancelledReplay = await client.work({ value: "normal-first" })
        .start(undefined, {
          invocationId: normalId,
          cancellationRequested: true,
        }).orThrow();
      assertEquals(cancelledReplay.id, normal.id);
      await client.work({ value: "normal-first" }).start(undefined, {
        invocationId: normalId,
      }).orThrow();
      assertEquals((await normal.wait().orThrow()).state, "cancelled");
      assertEquals(businessAttempts, 1);
      assertEquals(cleanupAttempts, 5);
    } finally {
      await client.connection.close();
      await service.stop();
      await service.wait();
    }
  });
});
