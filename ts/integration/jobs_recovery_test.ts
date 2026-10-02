import { assert, assertEquals } from "@std/assert";
import { jetstream, jetstreamManager } from "@nats-io/jetstream";
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
import type { Job } from "../packages/trellis/service/runtime/internal_jobs/types.ts";
import {
  createNatsJobKeyCoordinator,
  normalizeJobKeyPolicy,
} from "../packages/trellis/service/runtime/internal_jobs/key-coordinator.ts";
import type { JobKeyState } from "../packages/trellis/service/runtime/internal_jobs/key-coordinator.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

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
      worker = start();
      await runtime.waitFor(
        async () => (await abandoned.get().orThrow()).state === "expired",
        {
          timeoutMs: 150_000,
        },
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

Deno.test("TypeScript takeover preserves cleanup across failure and cannot release the new owner's fence", async () => {
  await withTrellisRuntime(async (runtime) => {
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
    const runningJobs: Promise<unknown>[] = [];
    try {
      const kvm = new Kvm(nc);
      const keys = await kvm.create("JOBS_KEYS_takeover");
      const effects = await kvm.create("TAKEOVER_EFFECTS");
      await (await jetstreamManager(nc)).streams.add({
        name: "TAKEOVER_EVENTS",
        subjects: ["takeover.>"],
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
            publishPrefix: "takeover.work",
            workSubject: "takeover.work.run",
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
      const owner = new JobManager<{ key: string }, { key: string }>({
        nc: publisher,
        jobs,
        keyCoordinator: createNatsJobKeyCoordinator(nc),
        meta: { nextJobId: () => crypto.randomUUID(), nowIso: () => now },
      });
      const a = await owner.create("work", { key: "shared" });
      const aStarted = Promise.withResolvers<void>();
      let snapshotA: Job<{ key: string }, { key: string }> | undefined;
      const runningA = owner.processWithHeartbeat(
        a,
        new JobCancellationToken(),
        () => Promise.resolve(),
        async (active) => {
          snapshotA = { ...active.job() };
          await effects.put("A", new TextEncoder().encode("working"));
          aStarted.resolve();
          return await finishA.promise;
        },
      );
      runningJobs.push(runningA);
      await Promise.race([
        aStarted.promise,
        runningA.then(() => {
          throw new Error("A settled without entering its handler");
        }),
      ]);
      assert(snapshotA);
      const b = await owner.create("work", { key: "shared" });
      now = "2026-10-01T00:00:31.000Z";
      const bStarted = Promise.withResolvers<void>();
      const runningB = owner.processWithHeartbeat(
        b,
        new JobCancellationToken(),
        () => Promise.resolve(),
        async () => {
          await effects.put("B", new TextEncoder().encode("working"));
          bStarted.resolve();
          return await finishB.promise;
        },
      );
      runningJobs.push(runningB);
      await Promise.race([
        bStarted.promise,
        runningB.then(() => {
          throw new Error("B settled without entering its handler");
        }),
      ]);
      const afterTakeover = await (await jetstreamManager(nc)).streams
        .getMessage(
          "TAKEOVER_EVENTS",
          { last_by_subj: `takeover.work.${a.id}.>` },
        );
      assert(afterTakeover);
      assertEquals(afterTakeover.json<{ state: string }>().state, "active");
      const keyNames = await keys.keys();
      const key = (await Array.fromAsync(keyNames))[0];
      assert(key);
      const taken = (await keys.get(key))!.json<JobKeyState>();
      assert(taken.cleanupPending.includes(a.id));
      const bToken = taken.active.find((slot) => slot.jobId === b.id)
        ?.slotToken;
      assert(bToken);
      finishA.resolve(a.payload);
      assertEquals((await runningA).outcome, "stale_completion_ignored");
      assertEquals(
        (await keys.get(key))!.json<JobKeyState>().active.find((slot) =>
          slot.jobId === b.id
        )?.slotToken,
        bToken,
      );
      const recovered = new JobManager<{ key: string }, { key: string }>({
        nc: publisher,
        jobs,
        keyCoordinator: createNatsJobKeyCoordinator(nc),
        meta: { nextJobId: () => crypto.randomUUID(), nowIso: () => now },
      });
      const blocked = await recovered.processWithHeartbeat(
        snapshotA,
        new JobCancellationToken(),
        () => Promise.resolve(),
        () => {
          throw new Error("cleanup ran without capacity");
        },
      );
      assertEquals(blocked.outcome, "deferred");
      finishB.resolve(b.payload);
      assertEquals((await runningB).outcome, "completed");
      const failed = await recovered.processWithHeartbeat(
        snapshotA,
        new JobCancellationToken(),
        () => Promise.resolve(),
        (active) => {
          snapshotA = { ...active.job() };
          assertEquals(active.cancellationToken().reason(), "stale-attempt");
          throw JobProcessError.retryable("cleanup interrupted");
        },
      );
      assertEquals(failed.outcome, "interrupted");
      assert(
        (await keys.get(key))!.json<JobKeyState>().cleanupPending.includes(
          a.id,
        ),
      );
      assert(await effects.get("A"));
      const settled = await recovered.processWithHeartbeat(
        snapshotA,
        new JobCancellationToken(),
        () => Promise.resolve(),
        async (active) => {
          assertEquals(active.cancellationToken().reason(), "stale-attempt");
          await effects.delete("A");
          return active.job().payload;
        },
      );
      assertEquals(settled.outcome, "stale");
      const terminal = await (await jetstreamManager(nc)).streams.getMessage(
        "TAKEOVER_EVENTS",
        { last_by_subj: `takeover.work.${a.id}.>` },
      );
      assert(terminal);
      assertEquals(terminal.json<{ state: string }>().state, "stale");
      assertEquals((await effects.get("A"))?.operation, "DEL");
      assertEquals((await effects.get("B"))?.string(), "working");
      const final = (await keys.get(key))!.json<JobKeyState>();
      assert(!final.cleanupPending.includes(a.id));
      assert(!final.active.some((slot) => slot.jobId === a.id));
    } finally {
      finishA.resolve({ key: "shared" });
      finishB.resolve({ key: "shared" });
      await Promise.allSettled(runningJobs);
      await nc.close();
    }
  });
});

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
            workSubject: "recovery.work.run",
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
      const manager = new JobManager<{ key: string }, { key: string }>({
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
      const successor = new JobManager<{ key: string }, { key: string }>({
        nc: publisher,
        jobs,
        keyCoordinator: createNatsJobKeyCoordinator(nc),
        meta: { nextJobId: () => crypto.randomUUID(), nowIso: () => now },
      });
      now = "2026-10-01T00:00:02.000Z";
      const expired = await successor.processWithHeartbeat(
        abandoned,
        new JobCancellationToken(),
        () => Promise.resolve(),
        () => {
          throw new Error("expired, never-started handler ran");
        },
      );
      assertEquals(expired.outcome, "expired");
      const next = await successor.create("work", { key: "shared" });
      const completed = await successor.processWithHeartbeat(
        next,
        new JobCancellationToken(),
        () => Promise.resolve(),
        (active) => Promise.resolve(active.job().payload),
      );
      assertEquals(completed.outcome, "completed");
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
