import { assert, assertEquals, assertRejects } from "@std/assert";
import { AckPolicy, jetstream, jetstreamManager } from "@nats-io/jetstream";
import { Kvm } from "@nats-io/kv";
import { credsAuthenticator } from "@nats-io/nats-core";
import { connect } from "@nats-io/transport-node";
import { join } from "@std/path";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  JobCancellationToken,
  JobManager,
  JobProcessError,
} from "../packages/trellis/service/runtime/internal_jobs/job-manager.ts";
import {
  getLatestLifecycleEvent,
  startNatsWorkerHostFromBinding,
} from "../packages/trellis/service/runtime/internal_jobs/runtime-worker.ts";
import {
  createNatsJobKeyCoordinator,
  normalizeJobKeyPolicy,
} from "../packages/trellis/service/runtime/internal_jobs/key-coordinator.ts";
import type { JobKeyState } from "../packages/trellis/service/runtime/internal_jobs/key-coordinator.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import type { StartNatsWorkerHostOptions } from "../packages/trellis/service/runtime/internal_jobs/runtime-worker.ts";

Deno.test("native Rust worker starts on a provisioned queue and retries cleanup beyond the ordinary budget", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "native-job-reconciliation",
      contract: participants.Provider.participant,
    });
    const [command, ...args] = rustFixtureArgv("jobs");
    const child = new Deno.Command(command, {
      args,
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: identity.seed,
      },
      stdout: "piped",
      stderr: "piped",
    }).spawn();
    let exited = false;
    const output = child.output().then((value) => {
      exited = true;
      return value;
    });
    try {
      await runtime.waitFor(() => exited, { timeoutMs: 60_000 });
      const result = await output;
      assertEquals(
        result.code,
        0,
        new TextDecoder().decode(result.stderr),
      );
    } finally {
      if (!exited) child.kill("SIGKILL");
      await output;
    }
  });
});

Deno.test("native worker killed after key persistence but before Started recovers expiration and unblocks the next job", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "native-pre-started",
      contract: participants.Provider.participant,
    });
    const submitterIdentity = await runtime.services.createInstance({
      name: "native-pre-started-submitter",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: submitterIdentity.seed,
    }).orThrow();
    const serviceExit = service.wait();
    const nc = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    const [command, ...args] = rustFixtureArgv("jobs");
    const start = () => {
      const child = new Deno.Command(command, {
        args,
        env: {
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_IDENTITY_SEED: identity.seed,
          TRELLIS_PRE_STARTED_WORK: "1",
        },
        stdin: "piped",
        stdout: "piped",
        stderr: "inherit",
      }).spawn();
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
      })();
      let exited = false;
      const status = child.status.then((value) => {
        exited = true;
        return value;
      });
      return { child, lines, output, status, exited: () => exited };
    };
    let worker = start();
    const gate = runtime.nativeTransportGate();
    let hold: ReturnType<typeof gate.armResponseHold> | undefined;
    try {
      await runtime.waitFor(() => {
        if (worker.exited()) {
          throw new Error("native worker exited before readiness");
        }
        return worker.lines.includes("ready");
      }, { timeoutMs: 60_000 });
      const connection = gate.connections().find((candidate) =>
        !candidate.closed &&
        candidate.subs.some((sub) =>
          sub.subject.includes(".preStartedWork.") &&
          sub.subject.endsWith(".cancelled")
        )
      );
      assert(connection);
      hold = gate.armResponseHold("$KV.JOBS_KEYS_", connection.id);
      const abandoned = await service.jobs.preStartedWork.create({
        key: "shared",
        value: "abandoned",
      }).orThrow();
      let held: Awaited<typeof hold.held> | undefined;
      const response = hold.held.then((value) => {
        held = value;
      });
      await runtime.waitFor(() => held !== undefined);
      await response;
      assert(held);
      const [bucket, ...keyParts] = held.requestSubject.slice("$KV.".length)
        .split(".");
      const key = keyParts.join(".");
      const kv = await new Kvm(nc).open(bucket);
      const entry = await kv.get(key);
      assert(entry);
      const persisted = JSON.parse(entry.string()) as JobKeyState;
      assert(persisted.active.some((slot) => slot.jobId === abandoned.id));
      assertEquals((await abandoned.get().orThrow()).tries, 0);
      // The real broker persisted acquisition, but its ACK never reached the
      // worker, so this SIGKILL deterministically precedes Started.
      worker.child.kill("SIGKILL");
      await Promise.all([worker.status, worker.output]);
      await worker.child.stdin.close();
      await hold.release();
      hold = undefined;
      const deadline = (await abandoned.get().orThrow()).deadline;
      assert(deadline);
      await runtime.waitFor(() => Date.now() >= Date.parse(deadline), {
        timeoutMs: 10_000,
      });
      // Expired is durable before the recovering worker receives its publish
      // ACK. Kill it in that second window, then let terminal redelivery clean
      // the still-owned slot rather than ACKing it away.
      hold = gate.armResponseHold("trellis.jobs.");
      let expirationHeld = false;
      void hold.held.then(() => expirationHeld = true);
      worker = start();
      await runtime.waitFor(() => expirationHeld, { timeoutMs: 150_000 });
      assert(
        (await kv.get(key))!.json<JobKeyState>().active.some((slot) =>
          slot.jobId === abandoned.id
        ),
      );
      worker.child.kill("SIGKILL");
      await Promise.all([worker.status, worker.output]);
      await worker.child.stdin.close();
      await hold.release();
      hold = undefined;
      worker = start();
      await runtime.waitFor(
        async () => (await abandoned.get().orThrow()).state === "expired",
        {
          timeoutMs: 150_000,
        },
      );
      await runtime.waitFor(
        async () =>
          !(await kv.get(key))!.json<JobKeyState>().active.some((slot) =>
            slot.jobId === abandoned.id
          ),
        { timeoutMs: 150_000 },
      );
      const settledEntry = await kv.get(key);
      assert(settledEntry);
      const settled = JSON.parse(settledEntry.string()) as JobKeyState;
      assert(!settled.active.some((slot) => slot.jobId === abandoned.id));
      assert(!worker.lines.includes("handler abandoned"));
      const next = await service.jobs.preStartedWork.create({
        key: "shared",
        value: "next",
      }).orThrow();
      assertEquals((await next.wait().orThrow()).state, "completed");
      const writer = worker.child.stdin.getWriter();
      await writer.write(new TextEncoder().encode("stop\n"));
      await writer.close();
      await runtime.waitFor(worker.exited, { timeoutMs: 10_000 });
      assert((await worker.status).success);
    } finally {
      if (!worker.exited()) worker.child.kill("SIGKILL");
      await Promise.allSettled([
        hold?.release(),
        worker.status,
        worker.output,
        worker.child.stdin.close(),
      ]);
      await nc.close();
      await service.stop();
      await serviceExit;
    }
  }, { interruptibleNativeProxy: true });
});

for (const wasStarted of [true, false]) {
  Deno.test(
    wasStarted
      ? "TypeScript takeover preserves cleanup across failure and cannot release the new owner's fence"
      : "pre-Started takeover settles through the worker and Jobs admin projection without execution",
    async () => {
      await withTrellisRuntime(async (runtime) => {
        const admin = await runtime.connectClient({
          name: "takeover-admin",
          contract: adminParticipant,
        });
        const nc = await connect({
          servers: runtime.natsUrl,
          authenticator: credsAuthenticator(
            await Deno.readFile(
              join(runtime.workdir, "nats/creds/trellis-auth.creds"),
            ),
          ),
        });
        const finishA = Promise.withResolvers<{ key: string }>();
        const finishB = Promise.withResolvers<{ key: string }>();
        let worker:
          | Awaited<ReturnType<typeof startNatsWorkerHostFromBinding>>
          | undefined;
        let recoveryWorker: typeof worker;
        try {
          const kvm = new Kvm(nc);
          const keys = await kvm.create("JOBS_KEYS_takeover");
          const effects = await kvm.create("TAKEOVER_EFFECTS");
          const jsm = await jetstreamManager(nc);
          await jsm.consumers.add("JOBS", {
            durable_name: "takeover-work",
            ack_policy: AckPolicy.Explicit,
            ack_wait: 1_000_000_000,
            max_deliver: -1,
            filter_subject: "trellis.jobs.takeover.work.*.created",
          });
          const keyConcurrency = {
            key: ["key"],
            maxActive: 1,
            heartbeatIntervalMs: 10_000,
            heartbeatTtlMs: 30_000,
            stalePolicy: "fail-stale" as const,
          };
          const queue = { maxQueuedPerKey: 2, whenFull: "reject" as const };
          const jobs = {
            serviceName: "takeover",
            namespace: "takeover",
            queues: {
              work: {
                queueType: "work",
                publishPrefix: "trellis.jobs.takeover.work",
                workSubject: "trellis.jobs.takeover.work.*.created",
                consumerName: "takeover-work",
                payload: { schema: "KeyedValue" },
                result: { schema: "KeyedValue" },
                maxDeliver: 2,
                backoffMs: [250],
                ackWaitMs: 1_000,
                keyConcurrency,
                queue,
              },
            },
          };
          let now = "2026-10-01T00:00:00.000Z";
          const publisher = jetstream(nc);
          const owner = new JobManager<unknown, { key: string }>({
            nc: publisher,
            jobs,
            keyCoordinator: createNatsJobKeyCoordinator(nc),
            meta: { nextJobId: () => crypto.randomUUID(), nowIso: () => now },
          });
          const a = await owner.create("work", { key: "shared" });
          const aStarted = Promise.withResolvers<void>();
          const bStarted = Promise.withResolvers<void>();
          let cleanupAttempts = 0;
          let aExecutions = 0;
          if (!wasStarted) {
            const acquired = await createNatsJobKeyCoordinator(nc)
              .acquireActiveSlot({
                service: "takeover",
                jobType: "work",
                jobId: a.id,
                payload: a.payload,
                context: a.context,
                lifecycleState: a.state,
                tries: 1,
                instanceId: "crashed-before-started",
                now,
                policy: normalizeJobKeyPolicy({ keyConcurrency, queue }),
              });
            assertEquals(acquired.kind, "acquired");
            now = "2026-10-01T00:00:31.000Z";
          }
          let b = !wasStarted
            ? await owner.create("work", { key: "shared" })
            : undefined;
          if (b) {
            await jsm.consumers.update("JOBS", "takeover-work", {
              filter_subject: `trellis.jobs.takeover.work.${b.id}.created`,
            });
          }
          const options = {
            nats: nc,
            instanceId: "takeover-worker",
            queueConcurrency: { work: 2 },
            manager: owner,
            getLatestLifecycleEvent: (job) =>
              getLatestLifecycleEvent(
                jsm.direct,
                "JOBS",
                "trellis.jobs.takeover.work",
                job,
              ),
            handler: async (active) => {
              if (active.job().id === a.id) {
                if (active.cancellationToken().reason() === "stale-attempt") {
                  cleanupAttempts++;
                  if (cleanupAttempts === 1) {
                    throw JobProcessError.retryable("cleanup interrupted");
                  }
                  await effects.delete("A");
                  return { key: "shared" };
                }
                aExecutions++;
                await effects.put("A", new TextEncoder().encode("working"));
                aStarted.resolve();
                return await finishA.promise;
              }
              await effects.put("B", new TextEncoder().encode("working"));
              bStarted.resolve();
              return await finishB.promise;
            },
          } satisfies StartNatsWorkerHostOptions<{ key: string }>;
          worker = await startNatsWorkerHostFromBinding({
            workStream: "JOBS",
            jobs,
          }, options);
          let enteredA = false;
          void aStarted.promise.then(() => enteredA = true);
          if (wasStarted) {
            await runtime.waitFor(() => enteredA);
            now = "2026-10-01T00:00:31.000Z";
            b = await owner.create("work", { key: "shared" });
          }
          assert(b);
          let enteredB = false;
          void bStarted.promise.then(() => enteredB = true);
          await runtime.waitFor(() => enteredB);
          const afterTakeover = await (await jetstreamManager(nc)).streams
            .getMessage(
              "JOBS",
              { last_by_subj: `trellis.jobs.takeover.work.${a.id}.>` },
            );
          assert(afterTakeover);
          assertEquals(
            afterTakeover.json<{ state: string }>().state,
            wasStarted ? "active" : "pending",
          );
          const keyNames = await keys.keys();
          const key = (await Array.fromAsync(keyNames))[0];
          assert(key);
          const taken = (await keys.get(key))!.json<JobKeyState>();
          assert(taken.cleanupPending.includes(a.id));
          const bToken = taken.active.find((slot) => slot.jobId === b.id)
            ?.slotToken;
          assert(bToken);
          finishA.resolve({ key: "shared" });
          if (wasStarted) {
            await runtime.waitFor(async () =>
              (await getLatestLifecycleEvent(
                jsm.direct,
                "JOBS",
                "trellis.jobs.takeover.work",
                a,
              ))?.eventType === "staleCompletionIgnored"
            );
            await runtime.waitFor(async () =>
              (await jsm.consumers.info("JOBS", "takeover-work")).delivered
                .consumer_seq >= 3
            );
            assertEquals(
              (await jsm.consumers.info("JOBS", "takeover-work"))
                .num_ack_pending,
              2,
            );
          } else {
            await jsm.consumers.add("JOBS", {
              durable_name: "takeover-recovery",
              ack_policy: AckPolicy.Explicit,
              max_deliver: -1,
              ack_wait: 1_000_000_000,
              filter_subject: `trellis.jobs.takeover.work.${a.id}.created`,
            });
            recoveryWorker = await startNatsWorkerHostFromBinding({
              workStream: "JOBS",
              jobs: {
                ...jobs,
                queues: {
                  work: {
                    ...jobs.queues.work,
                    consumerName: "takeover-recovery",
                    workSubject: `trellis.jobs.takeover.work.${a.id}.created`,
                  },
                },
              },
            }, {
              ...options,
              instanceId: "recovered-before-started",
              manager: new JobManager<unknown, { key: string }>({
                nc: publisher,
                jobs,
                keyCoordinator: createNatsJobKeyCoordinator(nc),
                meta: {
                  nextJobId: () => crypto.randomUUID(),
                  nowIso: () => now,
                },
              }),
            });
            await runtime.waitFor(async () =>
              (await jsm.consumers.info("JOBS", "takeover-recovery")).delivered
                .consumer_seq >= 1
            );
          }
          assertEquals(
            (await keys.get(key))!.json<JobKeyState>().active.find((slot) =>
              slot.jobId === b.id
            )?.slotToken,
            bToken,
          );
          assertEquals(cleanupAttempts, 0, "B still owns the key");
          if (wasStarted) assert(await effects.get("A"));
          finishB.resolve({ key: "shared" });
          await runtime.waitFor(async () =>
            (await getLatestLifecycleEvent(
              jsm.direct,
              "JOBS",
              "trellis.jobs.takeover.work",
              a,
            ))?.eventType === "stale"
          );
          await runtime.waitFor(async () =>
            (await jsm.consumers.info("JOBS", "takeover-work"))
              .num_ack_pending === 0
          );
          if (recoveryWorker) {
            await runtime.waitFor(async () =>
              (await jsm.consumers.info("JOBS", "takeover-recovery"))
                .num_ack_pending === 0
            );
          }
          assertEquals(cleanupAttempts, wasStarted ? 2 : 0);
          assertEquals(aExecutions, wasStarted ? 1 : 0);
          await runtime.waitFor(async () =>
            (await admin.jobsQuery({}).orThrow()).items.find((item) =>
              item.id === a.id
            )?.state === "stale"
          );
          const terminal = await (await jetstreamManager(nc)).streams
            .getMessage(
              "JOBS",
              { last_by_subj: `trellis.jobs.takeover.work.${a.id}.>` },
            );
          assert(terminal);
          assertEquals(terminal.json<{ state: string }>().state, "stale");
          if (wasStarted) {
            assertEquals((await effects.get("A"))?.operation, "DEL");
          }
          assertEquals((await effects.get("B"))?.string(), "working");
          const final = (await keys.get(key))!.json<JobKeyState>();
          assert(!final.cleanupPending.includes(a.id));
          assert(!final.active.some((slot) => slot.jobId === a.id));
        } finally {
          finishA.resolve({ key: "shared" });
          finishB.resolve({ key: "shared" });
          await worker?.stop();
          await recoveryWorker?.stop();
          await nc.close();
        }
      });
    },
  );
}

Deno.test("expired work with a persisted pre-Started slot releases a block-policy key without running its handler", async () => {
  await withTrellisRuntime(async (runtime) => {
    const nc = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    try {
      // This exercises the production manager and coordination adapter at the
      // real KV boundary. Acquire persists the crash window before Started;
      // the successor opens a new coordinator rather than retaining the lease.
      await new Kvm(nc).create("JOBS_KEYS_recovery");
      await (await jetstreamManager(nc)).streams.add({
        name: "RECOVERY_EVENTS",
        subjects: ["recovery.>"],
        allow_direct: true,
      });
      const jsm = await jetstreamManager(nc);
      await jsm.consumers.add("RECOVERY_EVENTS", {
        durable_name: "recovery-work",
        ack_policy: AckPolicy.Explicit,
        ack_wait: 1_000_000_000,
        max_deliver: -1,
        filter_subject: "recovery.work.*.created",
      });
      const keyConcurrency = {
        key: ["key"],
        maxActive: 1,
        heartbeatIntervalMs: 100,
        heartbeatTtlMs: 1_000,
        stalePolicy: "block" as const,
      };
      const queue = { maxQueuedPerKey: 1, whenFull: "reject" as const };
      const policy = normalizeJobKeyPolicy({ keyConcurrency, queue });
      let now = "2026-10-01T00:00:00.000Z";
      const jobs = {
        serviceName: "recovery",
        namespace: "recovery",
        queues: {
          work: {
            queueType: "work",
            publishPrefix: "recovery.work",
            workSubject: "recovery.work.*.created",
            consumerName: "recovery-work",
            payload: { schema: "KeyedValue" },
            result: { schema: "KeyedValue" },
            maxDeliver: 2,
            backoffMs: [250],
            ackWaitMs: 1_000,
            defaultDeadlineMs: 1_000,
            keyConcurrency,
            queue,
          },
        },
      };
      const publisher = jetstream(nc);
      const original = createNatsJobKeyCoordinator(nc);
      const manager = new JobManager<unknown, { key: string }>({
        nc: publisher,
        jobs,
        keyCoordinator: original,
        meta: { nextJobId: () => crypto.randomUUID(), nowIso: () => now },
      });
      const abandoned = await manager.create("work", { key: "shared" });
      const acquired = await original.acquireActiveSlot({
        service: "recovery",
        jobType: "work",
        jobId: abandoned.id,
        payload: abandoned.payload,
        context: abandoned.context,
        lifecycleState: abandoned.state,
        tries: 1,
        instanceId: "lost-owner",
        now,
        policy,
      });
      assertEquals(acquired.kind, "acquired");
      const keys = await new Kvm(nc).open("JOBS_KEYS_recovery");
      const names = await Array.fromAsync(await keys.keys());
      const retained = (await keys.get(names[0]))!;
      const invalid: Record<string, unknown> = retained.json();
      delete invalid.cleanupPending;
      const invalidBytes = JSON.stringify(invalid);
      await keys.update(names[0], invalidBytes, retained.revision);
      const fresh = new JobManager<unknown, { key: string }>({
        nc: publisher,
        jobs,
        keyCoordinator: createNatsJobKeyCoordinator(nc),
        meta: { nextJobId: () => crypto.randomUUID(), nowIso: () => now },
      });
      await assertRejects(
        () => fresh.create("work", { key: "shared" }),
        Error,
        "Invalid keyed job state",
      );
      assertEquals(
        (await keys.get(names[0]))!.string(),
        invalidBytes,
        "rejected admission must not overwrite retained ownership",
      );
      await keys.put(names[0], retained.value);
      now = "2026-10-01T00:00:02.000Z";
      // Real broker publication failure: no stream captures Expired until the
      // lifecycle route is restored. The work consumer and key KV stay intact.
      await jsm.streams.update("RECOVERY_EVENTS", {
        subjects: ["recovery.work.*.created", "recovery.work.*.heartbeat"],
      });
      let abandonedExecutions = 0;
      const start = () => {
        const successor = new JobManager<unknown, { key: string }>({
          nc: publisher,
          jobs,
          keyCoordinator: createNatsJobKeyCoordinator(nc),
          meta: { nextJobId: () => crypto.randomUUID(), nowIso: () => now },
        });
        return startNatsWorkerHostFromBinding({
          workStream: "RECOVERY_EVENTS",
          jobs,
        }, {
          nats: nc,
          instanceId: "recovery-worker",
          manager: successor,
          getLatestLifecycleEvent: (job) =>
            getLatestLifecycleEvent(
              jsm.direct,
              "RECOVERY_EVENTS",
              "recovery.work",
              job,
            ),
          handler: (active) => {
            if (active.job().id === abandoned.id) abandonedExecutions++;
            return Promise.resolve({ key: "shared" });
          },
        });
      };
      let worker = await start();
      try {
        await runtime.waitFor(async () =>
          (await jsm.consumers.info("RECOVERY_EVENTS", "recovery-work"))
            .delivered.consumer_seq >= 2
        );
        await worker.stop();
        assert(
          (await keys.get(names[0]))!.json<JobKeyState>().active.some((slot) =>
            slot.jobId === abandoned.id
          ),
          "failed expiration must retain a usable fenced reservation",
        );
        now = "2026-10-01T00:00:04.000Z";
        await jsm.streams.update("RECOVERY_EVENTS", {
          subjects: ["recovery.>"],
        });
        worker = await start();
        await runtime.waitFor(async () =>
          (await getLatestLifecycleEvent(
            jsm.direct,
            "RECOVERY_EVENTS",
            "recovery.work",
            abandoned,
          ))?.state === "expired"
        );
        await runtime.waitFor(async () =>
          (await jsm.consumers.info("RECOVERY_EVENTS", "recovery-work"))
            .num_ack_pending === 0
        );
        assertEquals(abandonedExecutions, 0);
        const next = await fresh.create("work", { key: "shared" });
        await runtime.waitFor(async () =>
          (await getLatestLifecycleEvent(
            jsm.direct,
            "RECOVERY_EVENTS",
            "recovery.work",
            next,
          ))?.state === "completed"
        );
      } finally {
        await worker.stop();
      }
      // The old fence cannot mutate the successor's now-settled coordination.
      assert(acquired.kind === "acquired");
      assertEquals(
        (await original.releaseActiveSlot({
          service: "recovery",
          jobType: "work",
          jobId: abandoned.id,
          lease: {
            key: acquired.key,
            keyHash: acquired.keyHash,
            slotToken: acquired.slotToken,
            policy,
          },
          now,
        })).kind,
        "staleCompletion",
      );
    } finally {
      await nc.close();
    }
  });
});
