import { jetstream, jetstreamManager } from "@nats-io/jetstream";
import type { ConsumerInfo } from "@nats-io/jetstream";
import type { NatsConnection } from "@nats-io/nats-core";
import type { GenerationIntakeInstall } from "../../../session.ts";
import type { TransportGenerationManager } from "../../../transport/generations.ts";
import type { JobsRuntimeBinding } from "./bindings.ts";
import type { ActiveJobCancellationRegistry } from "./cancellation-registry.ts";
import { startWorkerHeartbeatLoop } from "./heartbeat.ts";
import { JobCancellationToken } from "./job-manager.ts";
import { createNatsJobKeyCoordinator } from "./key-coordinator.ts";
import {
  getConsumerInfo,
  getLatestLifecycleEvent,
  type JobReceivingSession,
  progressAckIntervalMs,
  startJobCancellationCoverage,
  type StartNatsWorkerHostOptions,
  startQueueWorkerLoop,
  toWorkerConsumer,
  WorkerHostStopError,
} from "./runtime-worker.ts";

/** @internal One queue host's persistent slots follow exact published intake. */
export function startManagedJobWorkerHost<TResult>(
  binding: JobsRuntimeBinding,
  options: StartNatsWorkerHostOptions<TResult> & {
    transport: TransportGenerationManager;
    intakeOwner: {
      declareFrameworkIntake(
        id: string,
        install: GenerationIntakeInstall,
      ): Promise<void>;
      retractFrameworkIntake(id: string): Promise<void>;
    };
    queueType: string;
    cancellationRegistry: ActiveJobCancellationRegistry;
  },
): { ready: Promise<void>; stop(reason?: unknown): Promise<void> } {
  const queue = binding.jobs.queues[options.queueType];
  if (!queue) throw new Error(`Unknown jobs queue '${options.queueType}'`);
  const concurrency = options.queueConcurrency?.[options.queueType] ?? 1;
  const ackInterval = progressAckIntervalMs(queue);
  if (!Number.isInteger(concurrency) || concurrency < 1) {
    throw new Error(`Invalid worker concurrency ${concurrency}`);
  }
  type Source = {
    nc: NatsConnection;
    info: ConsumerInfo;
    jsm: Awaited<ReturnType<typeof jetstreamManager>>;
    loss: AbortController;
    draining: boolean;
    count: number;
    drained: ReturnType<typeof Promise.withResolvers<void>>;
    coverage: Awaited<ReturnType<typeof startJobCancellationCoverage>>;
    drain(): Promise<void>;
    dispose(): Promise<void>;
  };
  const sources = new Map<number, Source>();
  const ownedSources = new Set<Source>();
  const retirementFailures: unknown[] = [];
  const cancellation = new JobCancellationToken();
  const setup = new AbortController();
  const setupAborted = Promise.withResolvers<never>();
  setup.signal.addEventListener(
    "abort",
    () => setupAborted.reject(setup.signal.reason),
    { once: true },
  );
  // A host can stop after setup completed, when nobody is racing this promise.
  void setupAborted.promise.catch(() => {});
  const workers: Array<{ stop(): Promise<void> }> = [];
  const slotsStopped = Promise.withResolvers<void>();
  let heartbeat:
    | Awaited<ReturnType<typeof startWorkerHeartbeatLoop>>
    | undefined;
  let stopping = false;
  let changed = Promise.withResolvers<void>();
  const notify = () => {
    changed.resolve();
    changed = Promise.withResolvers<void>();
  };
  const unsubscribe = options.transport.subscribePublication(notify);
  const id = `jobs:${binding.jobs.namespace}:${options.queueType}`;

  const acquire = async (
    signal: AbortSignal,
  ): Promise<JobReceivingSession<TResult>> => {
    while (true) {
      signal.throwIfAborted();
      const change = changed.promise;
      const published = options.transport.publishedGenerationId();
      const source = published === undefined
        ? undefined
        : sources.get(published);
      if (source && !source.draining && !source.loss.signal.aborted) {
        // Acquisition itself may classify policy asynchronously. An aborted
        // caller stops waiting, but any eventual lease still gets released.
        const pending = options.transport.acquirePublishedAttachment();
        const aborted = Promise.withResolvers<never>();
        const abort = () => aborted.reject(signal.reason);
        signal.addEventListener("abort", abort, { once: true });
        let lease;
        try {
          lease = await Promise.race([pending, aborted.promise]);
        } catch (error) {
          void pending.then((late) => late?.release(), () => {});
          throw error;
        } finally {
          signal.removeEventListener("abort", abort);
        }
        if (lease) {
          if (
            signal.aborted || stopping || source.draining ||
            source.loss.signal.aborted ||
            lease.generationId !== published || lease.nc !== source.nc ||
            options.transport.publishedGenerationId() !== published ||
            sources.get(published!) !== source
          ) {
            lease.release();
            signal.throwIfAborted();
            continue;
          }
          source.count++;
          let released = false;
          const js = jetstream(lease.nc);
          const consumer = toWorkerConsumer(
            js.consumers.getConsumerFromInfo(source.info),
          );
          return {
            nc: lease.nc,
            manager: options.manager.withTransport(
              js,
              createNatsJobKeyCoordinator(lease.nc),
            ),
            physicalLoss: source.loss.signal,
            consumer: {
              async next() {
                let receive: ReturnType<typeof consumer.next> | undefined;
                await options.transport.commitOnPublished(
                  lease.generationId,
                  () => {
                    if (
                      stopping || source.draining ||
                      source.loss.signal.aborted ||
                      sources.get(lease.generationId) !== source
                    ) return false;
                    receive = consumer.next();
                    return true;
                  },
                );
                return receive ? await receive : null;
              },
            },
            getLatestLifecycleEvent: (job) =>
              getLatestLifecycleEvent(
                source.jsm.direct,
                "JOBS",
                queue.publishPrefix,
                job,
              ),
            release() {
              if (released) return;
              released = true;
              lease.release();
              source.count--;
              if (source.draining && source.count === 0) {
                source.drained.resolve();
              }
            },
          };
        }
      }
      // Publication and installation wake immediately; the bounded retry also
      // covers same-generation authority/readiness recovery without polling pulls.
      const wake = Promise.withResolvers<void>();
      const abort = () => wake.reject(signal.reason);
      signal.addEventListener("abort", abort, { once: true });
      const timer = setTimeout(() => wake.resolve(), 100);
      try {
        signal.throwIfAborted();
        await Promise.race([change, wake.promise]);
      } finally {
        clearTimeout(timer);
        signal.removeEventListener("abort", abort);
      }
    }
  };

  // Register synchronously before any setup awaits. Candidate installation only
  // reads INFO and flushes cancellation coverage; it never leases or pulls.
  const installation = options.intakeOwner.declareFrameworkIntake(
    id,
    async (target) => {
      setup.signal.throwIfAborted();
      const jsm = await Promise.race([
        jetstreamManager(target.nc),
        setupAborted.promise,
      ]);
      setup.signal.throwIfAborted();
      const info = await Promise.race([
        getConsumerInfo(jsm, binding.workStream, queue),
        setupAborted.promise,
      ]);
      setup.signal.throwIfAborted();
      const coverage = await startJobCancellationCoverage(
        target.nc,
        `${queue.publishPrefix}.*.cancelled`,
        options.cancellationRegistry,
        setup.signal,
      );
      if (stopping) {
        await coverage.stop();
        setup.signal.throwIfAborted();
      }
      let disposal: Promise<void> | undefined;
      const source: Source = {
        nc: target.nc,
        info,
        jsm,
        coverage,
        loss: new AbortController(),
        draining: false,
        count: 0,
        drained: Promise.withResolvers<void>(),
        drain() {
          source.draining = true;
          if (source.count === 0) source.drained.resolve();
          notify();
          return Promise.resolve();
        },
        dispose() {
          return disposal ??= (async () => {
            source.draining = true;
            if (stopping) await slotsStopped.promise;
            source.loss.abort();
            if (source.count === 0) source.drained.resolve();
            if (sources.get(target.id) === source) sources.delete(target.id);
            notify();
            try {
              await coverage.stop();
            } finally {
              ownedSources.delete(source);
            }
          })();
        },
      };
      const previous = sources.get(target.id);
      ownedSources.add(source);
      sources.set(target.id, source);
      notify();
      if (previous) {
        // Reinstatement may replace preparation on the same physical id.
        // New coverage is flushed first; the old coverage survives its work.
        await previous.drain();
        void previous.drained.promise.then(() => previous.dispose()).catch((
          error,
        ) => retirementFailures.push(error));
      }
      return {
        drain: source.drain,
        done: source.drained.promise,
        dispose: source.dispose,
      };
    },
  );
  const ready = (async () => {
    await installation;
    setup.signal.throwIfAborted();
    for (let slot = 0; slot < concurrency; slot++) {
      workers.push(
        await startQueueWorkerLoop({
          acquireSession: acquire,
          cancellationRegistry: options.cancellationRegistry,
          hostCancellation: cancellation,
          payloadSchema: queue.payload,
          resultSchema: queue.result,
          validatePayload: options.validatePayload,
          validateResult: options.validateResult,
          handler: options.handler,
          instanceId: options.instanceId,
          backoffMs: queue.backoffMs,
          deferralBackoffMs: queue.backoffMs[0] ?? 1_000,
          progressAckIntervalMs: ackInterval,
        }),
      );
      setup.signal.throwIfAborted();
    }
    heartbeat = await startWorkerHeartbeatLoop({
      publisher: {
        async publish(subject, payload) {
          let session;
          try {
            session = await acquire(setup.signal);
          } catch (error) {
            if (setup.signal.aborted && error === setup.signal.reason) return;
            throw error;
          }
          try {
            session.nc!.publish(subject, payload);
            // Plain publish only queues the frame. Account for its delivery
            // before releasing the physical generation's finite lease.
            await session.nc!.flush();
          } finally {
            await session.release();
          }
        },
      },
      service: binding.jobs.serviceName,
      subjectService: binding.jobs.namespace,
      jobType: options.queueType,
      instanceId: options.instanceId,
      concurrency,
      intervalMs: options.heartbeatIntervalMs,
    });
  })();
  let stop: Promise<void> | undefined;
  return {
    ready,
    stop(reason?: unknown) {
      return stop ??= (async () => {
        stopping = true;
        setup.abort(reason);
        cancellation.cancelForShutdown();
        unsubscribe();
        notify();
        const retraction = options.intakeOwner.retractFrameworkIntake(id);
        await ready.catch(() => {});
        const results = await Promise.allSettled(
          workers.map((worker) => worker.stop()),
        );
        slotsStopped.resolve();
        const cleanup = await Promise.allSettled([
          retraction,
          heartbeat?.stop(),
          ...[...ownedSources].map((source) => source.dispose()),
        ]);
        const failures = [...results, ...cleanup].filter((
          result,
        ): result is PromiseRejectedResult => result.status === "rejected").map(
          (result) => result.reason,
        );
        if (failures.length || retirementFailures.length) {
          throw new WorkerHostStopError([...failures, ...retirementFailures]);
        }
      })();
    },
  };
}
