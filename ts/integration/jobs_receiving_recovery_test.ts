import {
  AckPolicy,
  jetstream,
  JetStreamError,
  jetstreamManager,
} from "@nats-io/jetstream";
import { assert, assertEquals, assertRejects } from "@std/assert";
import { deadline } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { join } from "@std/path";
import { NativeTransportGate } from "../packages/trellis-testkit/src/native_gate.ts";
import { NatsTestContainer } from "../packages/trellis-testkit/src/nats_container.ts";
import { TcpProxy } from "../packages/trellis-testkit/src/runtime.ts";
import { ActiveJobCancellationRegistry } from "../packages/trellis/service/runtime/internal_jobs/cancellation-registry.ts";
import { JobManager } from "../packages/trellis/service/runtime/internal_jobs/job-manager.ts";
import {
  startQueueWorkerLoop,
  toWorkerConsumer,
  WorkerLoopStopError,
} from "../packages/trellis/service/runtime/internal_jobs/runtime-worker.ts";

Deno.test("managed Jobs slot recovers a real missed-heartbeat receive and stops during recovery", async () => {
  const workdir = await Deno.makeTempDir({
    prefix: "jobs-receiving-recovery-",
  });
  const nats = await NatsTestContainer.start(workdir);
  const gate = new NativeTransportGate();
  const proxy = TcpProxy.start(nats.natsUrl, { scheme: "nats", gate });
  const nc = await connect({
    servers: proxy.url,
    authenticator: credsAuthenticator(
      await Deno.readFile(
        join(workdir, "nats", nats.manifest.paths.creds.trellisService),
      ),
    ),
  });
  let slot: Awaited<ReturnType<typeof startQueueWorkerLoop>> | undefined;
  let hold = gate.armResponseHold("$JS.API.CONSUMER.MSG.NEXT.JOBS.recovery");
  const finishHandler = Promise.withResolvers<void>();
  try {
    const js = jetstream(nc);
    const jsm = await jetstreamManager(nats.nc);
    const prefix = "trellis.jobs.recovery.work";
    await jsm.streams.add({
      name: "JOBS",
      subjects: [`${prefix}.>`],
      allow_direct: true,
    });
    const info = await jsm.consumers.add("JOBS", {
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
        assertEquals(
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
        assertEquals(session.nc, nc);
        handlerCount++;
        handled.resolve();
        await finishHandler.promise;
        return { recovered: true };
      },
    });
    await deadline(hold.held, 5_000);
    const firstError = await deadline(failures[0]!.promise, 5_000);
    assert(firstError instanceof JetStreamError);
    assertEquals(firstError.message, "heartbeats missed");
    assertEquals(nc.isClosed(), false);
    await hold.release();
    const created = await manager.create("work", {});
    await waitFor(() => handlerCount === 1);
    await handled.promise;
    assertEquals(
      releases,
      acquisitions - 1,
      "accepted handler retains its receiving session",
    );
    finishHandler.resolve();
    await deadline(disposed.promise, 5_000);
    const completed = await jsm.direct.getMessage("JOBS", {
      last_by_subj: `${prefix}.${created.id}.completed`,
    });
    assert(completed);
    assertEquals(JSON.parse(completed.string()).result, { recovered: true });
    await nc.flush();
    assertEquals(
      (await jsm.consumers.info("JOBS", "recovery")).num_ack_pending,
      0,
    );
    hold = gate.armResponseHold("$JS.API.CONSUMER.MSG.NEXT.JOBS.recovery");
    await deadline(hold.held, 5_000);
    const secondError = await deadline(failures[1]!.promise, 5_000);
    assert(secondError instanceof JetStreamError);
    assertEquals(secondError.message, "heartbeats missed");
    await deadline(recoveryReleased.promise, 5_000);
    // Let the slot enter its post-release recovery wait, without a timer sleep.
    await Promise.resolve();
    await deadline(slot.stop(), 5_000);
    const stoppedAcquisitions = acquisitions;
    await hold.release();
    await nc.flush();
    assertEquals(
      acquisitions,
      stoppedAcquisitions,
      "stop during recovery must not acquire another receive",
    );
    assertEquals(releases, acquisitions);
    assertEquals(handlerCount, 1);
    hold = gate.armResponseHold("$JS.API.CONSUMER.MSG.NEXT.JOBS.recovery");
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
    await assertRejects(
      () => deadline(slot!.stop(), 5_000),
      WorkerLoopStopError,
    );
    assertEquals(
      fixedReleases,
      1,
      "fixed host reports the receive failure after releasing ownership",
    );
  } finally {
    finishHandler.resolve();
    await hold.release();
    await slot?.stop().catch(() => {});
    await nc.close();
    proxy.stop();
    await nats.stop();
    await Deno.remove(workdir, { recursive: true });
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
