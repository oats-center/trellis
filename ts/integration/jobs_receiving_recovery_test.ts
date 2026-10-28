import {
  AckPolicy,
  jetstream,
  JetStreamError,
  jetstreamManager,
} from "@nats-io/jetstream";
import assert from "node:assert/strict";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { NativeTransportGate } from "../packages/trellis-testkit/src/native_gate.ts";
import { startTrellisRuntime } from "./_support/runtime.ts";
import { TcpProxy } from "../packages/trellis-testkit/src/runtime.ts";
import { ActiveJobCancellationRegistry } from "../packages/trellis/service/runtime/internal_jobs/cancellation-registry.ts";
import { JobManager } from "../packages/trellis/service/runtime/internal_jobs/job-manager.ts";
import {
  startQueueWorkerLoop,
  toWorkerConsumer,
  WorkerLoopStopError,
} from "../packages/trellis/service/runtime/internal_jobs/runtime-worker.ts";

test("managed Jobs slot recovers a real missed-heartbeat receive and stops during recovery", async () => {
  const nats = await startTrellisRuntime();
  const gate = new NativeTransportGate();
  const proxy = await TcpProxy.start(nats.natsUrl, {
    scheme: "tls",
    gate,
    tls: () => nats.brokerTls(),
  });
  const nc = await nats.connectNats({
    servers: proxy.url,
  });
  const managerNc = await nats.connectNats();
  let slot: Awaited<ReturnType<typeof startQueueWorkerLoop>> | undefined;
  let hold = gate.armResponseHold(
    "$JS.API.CONSUMER.MSG.NEXT.ADAPTER_JOBS.recovery",
  );
  const finishHandler = Promise.withResolvers<void>();
  try {
    const js = jetstream(nc);
    const jsm = await jetstreamManager(managerNc);
    const prefix = "adapter.jobs.recovery.work";
    await jsm.streams.add({
      name: "ADAPTER_JOBS",
      subjects: [`${prefix}.>`],
      allow_direct: true,
    });
    const info = await jsm.consumers.add("ADAPTER_JOBS", {
      durable_name: "recovery",
      ack_policy: AckPolicy.Explicit,
      ack_wait: 30_000_000_000,
      filter_subject: `${prefix}.*.created`,
    });
    const manager = new JobManager({
      nc: js,
      jobs: {
        serviceName: "recovery",
        namespace: "recovery",
        queues: {
          work: {
            queueType: "work",
            publishPrefix: prefix,
            workSubject: `${prefix}.*.created`,
            consumerName: "recovery",
            payload: { schema: "RecoveryPayload" },
            maxDeliver: 5,
            backoffMs: [],
            ackWaitMs: 30_000,
          },
        },
      },
    });
    const failures = [
      Promise.withResolvers<unknown>(),
      Promise.withResolvers<unknown>(),
    ];
    const handled = Promise.withResolvers<void>();
    const disposed = Promise.withResolvers<void>();
    const recoveryReleased = Promise.withResolvers<void>();
    let acquisitions = 0;
    let releases = 0;
    let failureCount = 0;
    let handlerCount = 0;
    slot = await startQueueWorkerLoop({
      cancellationRegistry: new ActiveJobCancellationRegistry(),
      acquireSession(signal) {
        signal.throwIfAborted();
        assert.deepEqual(
          releases,
          acquisitions,
          "the settled session must be released before reacquisition",
        );
        acquisitions++;
        const consumer = toWorkerConsumer(
          js.consumers.getConsumerFromInfo(info),
        );
        return Promise.resolve({
          nc,
          manager,
          physicalLoss: new AbortController().signal,
          consumer: {
            async next() {
              try {
                return await consumer.next();
              } catch (error) {
                failures[failureCount++]!.resolve(error);
                throw error;
              }
            },
          },
          release() {
            releases++;
            if (handlerCount === 1) disposed.resolve();
            if (failureCount === 2) recoveryReleased.resolve();
          },
        });
      },
      handler: async (_job, session) => {
        assert.deepEqual(session.nc, nc);
        handlerCount++;
        handled.resolve();
        await finishHandler.promise;
        return { recovered: true };
      },
    });
    await deadline(hold.held, 5_000);
    const firstError = await deadline(failures[0]!.promise, 5_000);
    assert(firstError instanceof JetStreamError);
    assert.deepEqual(firstError.message, "heartbeats missed");
    assert.deepEqual(nc.isClosed(), false);
    await hold.release();
    const created = await manager.create("work", {});
    await waitFor(() => handlerCount === 1);
    await handled.promise;
    assert.deepEqual(
      releases,
      acquisitions - 1,
      "accepted handler retains its receiving session",
    );
    finishHandler.resolve();
    await deadline(disposed.promise, 5_000);
    const completed = await jsm.direct.getMessage("ADAPTER_JOBS", {
      last_by_subj: `${prefix}.${created.id}.completed`,
    });
    assert(completed);
    assert.deepEqual(JSON.parse(completed.string()).result, {
      recovered: true,
    });
    await nc.flush();
    assert.deepEqual(
      (await jsm.consumers.info("ADAPTER_JOBS", "recovery")).num_ack_pending,
      0,
    );
    hold = gate.armResponseHold(
      "$JS.API.CONSUMER.MSG.NEXT.ADAPTER_JOBS.recovery",
    );
    await deadline(hold.held, 5_000);
    const secondError = await deadline(failures[1]!.promise, 5_000);
    assert(secondError instanceof JetStreamError);
    assert.deepEqual(secondError.message, "heartbeats missed");
    await deadline(recoveryReleased.promise, 5_000);
    // Let the slot enter its post-release recovery wait, without a timer sleep.
    await Promise.resolve();
    await deadline(slot.stop(), 5_000);
    const stoppedAcquisitions = acquisitions;
    await hold.release();
    await nc.flush();
    assert.deepEqual(
      acquisitions,
      stoppedAcquisitions,
      "stop during recovery must not acquire another receive",
    );
    assert.deepEqual(releases, acquisitions);
    assert.deepEqual(handlerCount, 1);
    hold = gate.armResponseHold(
      "$JS.API.CONSUMER.MSG.NEXT.ADAPTER_JOBS.recovery",
    );
    const fixedFailure = Promise.withResolvers<void>();
    const fixedConsumer = toWorkerConsumer(
      js.consumers.getConsumerFromInfo(info),
    );
    let fixedReleases = 0;
    slot = await startQueueWorkerLoop({
      cancellationRegistry: new ActiveJobCancellationRegistry(),
      acquireSession: () =>
        Promise.resolve({
          manager,
          consumer: {
            async next() {
              try {
                return await fixedConsumer.next();
              } catch (error) {
                fixedFailure.resolve();
                throw error;
              }
            },
          },
          release() {
            fixedReleases++;
          },
        }),
      handler: () => {
        throw new Error("an empty fixed receive must not start a handler");
      },
    });
    await deadline(hold.held, 5_000);
    await deadline(fixedFailure.promise, 5_000);
    await assert.rejects(
      () => deadline(slot!.stop(), 5_000),
      WorkerLoopStopError,
    );
    assert.deepEqual(
      fixedReleases,
      1,
      "fixed host reports the receive failure after releasing ownership",
    );
  } finally {
    finishHandler.resolve();
    await hold.release();
    await slot?.stop().catch(() => {});
    await nc.close();
    await managerNc.close();
    await proxy.stop();
    await nats.stop();
  }
});

async function waitFor(predicate: () => boolean): Promise<void> {
  const deadline = performance.now() + 5_000;
  while (!predicate()) {
    if (performance.now() >= deadline) {
      throw new Error("receiving recovery did not converge");
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
