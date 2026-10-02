/** Real managed facade: two logical Jobs slots across provider growth. */
import { assert, assertEquals } from "@std/assert";
import { createClient } from "@libsql/client";
import { jetstream, jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("TS managed Jobs keeps two slots and receiving ownership across published growth", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    assertEquals(requested.status, "approval_required");
    if (requested.status !== "approval_required") {
      throw new Error("consent required");
    }
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeResources: ["extras"],
    });
    const instance = await runtime.services.createInstance({
      name: "jobs-generation-ts",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "jobs-generation-ts",
      seed: instance.seed,
    }).orThrow();
    let exited: Promise<unknown> | undefined;
    const system = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/system.creds`),
      ),
    });
    const observer = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/trellis-auth.creds`),
      ),
    });
    const jsm = await jetstreamManager(observer);
    const database = createClient({
      url: `file:${runtime.workdir}/trellis/trellis.sqlite.jobs`,
    });
    const gate = runtime.nativeTransportGate();
    const releases = new Map<
      string,
      ReturnType<typeof Promise.withResolvers<void>>
    >();
    const started: string[] = [];
    const completed: string[] = [];
    let hold: ReturnType<typeof gate.armResponseHold> | undefined;
    let applied: ReturnType<typeof runtime.contracts.apply> | undefined;
    const attachments = async () => {
      const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
        items: { participantId: string; contextDigest: string }[];
      };
      return page.items.filter((item) =>
        item.participantId === contract.identity
      );
    };
    try {
      service.jobs.keyedWork.handle(async ({ job }) => {
        const name = job.payload.value;
        started.push(name);
        await releases.get(name)!.promise;
        completed.push(name);
        return Result.ok(job.payload);
      }, { concurrency: 2 });
      // The later ordinary queue supplies a broker-readiness gate after
      // keyedWork's INFO and flushed cancellation preparation have completed.
      service.jobs.work.handle(async ({ job }) => {
        await job.emitUpdate({
          value: job.payload.value,
          nested: {
            count: 1n,
            payload: new TextEncoder().encode(job.payload.value),
          },
        }).orThrow();
        return Result.ok(job.payload);
      });
      exited = service.wait().catch((error: unknown) => error);
      const submit = async (name: string) => {
        releases.set(name, Promise.withResolvers<void>());
        return await service.jobs.keyedWork.create({ key: name, value: name })
          .orThrow();
      };
      const a = await submit("A");
      await runtime.waitFor(() => started.includes("A"));
      const worker = await runtime.waitFor(async () => {
        const result = await database.execute({
          sql:
            "SELECT service, instance_id FROM worker_presence_projection WHERE job_type = ?",
          args: ["keyedWork"],
        });
        assert(result.rows.length <= 1);
        return result.rows[0];
      });
      assert(worker);
      assert(typeof worker.service === "string");
      assert(typeof worker.instance_id === "string");
      const workerService = worker.service;
      const workerId = worker.instance_id;
      const queues = [];
      let readinessQueue: { stream: string; name: string } | undefined;
      for await (const stream of jsm.streams.list()) {
        for await (const consumer of jsm.consumers.list(stream.config.name)) {
          if (consumer.config.filter_subject?.endsWith(".keyedWork")) {
            queues.push({ stream: stream.config.name, consumer });
          }
          if (consumer.config.filter_subject?.endsWith(".work")) {
            readinessQueue = {
              stream: stream.config.name,
              name: consumer.name,
            };
          }
        }
      }
      assertEquals(queues.length, 1);
      assert(readinessQueue);
      const { stream, consumer } = queues[0];
      const workSubject = consumer.config.filter_subject!;
      const pull = `$JS.API.CONSUMER.MSG.NEXT.${stream}.${consumer.name}`;
      const info = () => jsm.consumers.info(stream, consumer.name);
      const receiving = gate.connections().filter((connection) =>
        connection.deliveries.some((delivery) =>
          delivery.subject === workSubject
        )
      );
      assertEquals(receiving.length, 1);
      const g1 = receiving[0].id;
      const [initial] = await attachments();
      assert(initial);
      const originals = admittedConnections(
        await completeBrokerInventory(system),
        new Set([initial.contextDigest]),
      );
      assertEquals(originals.length, 1);
      const originalKey = brokerConnectionKey(originals[0]);
      const inventory = () =>
        completeBrokerInventory(system, {
          requiredServerIds: [originals[0].server],
        });
      hold = gate.armResponseHold(
        `$JS.API.CONSUMER.INFO.${readinessQueue.stream}.${readinessQueue.name}`,
      );
      let prepared: Awaited<typeof hold.held> | undefined;
      void hold.held.then((value) => prepared = value);
      applied = runtime.contracts.apply({ contract });
      // Drive the real readiness barrier independently of the admin client's
      // binding/revision convergence: it may outlast the held INFO request.
      void applied.catch(() => undefined);
      await runtime.waitFor(() => prepared, { timeoutMs: 90_000 });
      assert(prepared);
      const g2 = prepared.connectionId;
      assert(g2 !== g1);
      assert(
        gate.connection(g2)!.subs.some((sub) =>
          sub.subject.endsWith(".keyedWork.*.cancelled")
        ),
        "the tested queue must already have cancellation coverage before the later readiness gate",
      );
      assertEquals((await info()).num_ack_pending, 1);
      assertEquals(
        gate.connection(g2)!.outboundContexts.filter((out) =>
          out.subject === pull
        ).length,
        0,
        "an unpublished candidate must never pull",
      );
      await hold.release();
      hold = undefined;
      await applied;
      await runtime.waitFor(async () =>
        gate.connection(g2)!.outboundContexts.some((out) =>
          out.subject === pull
        ) && (await info()).num_waiting === 1
      );
      const b = await submit("B");
      await runtime.waitFor(() => started.includes("B"));
      releases.get("B")!.resolve();
      assertEquals((await b.wait().orThrow()).state, "completed");
      assert(!completed.includes("A"));
      assert(
        (await inventory()).some((connection) =>
          brokerConnectionKey(connection) === originalKey
        ),
      );
      await submit("C");
      await runtime.waitFor(() => started.includes("C"));
      await submit("D");
      await submit("E");
      const full = await info();
      assertEquals(
        full.num_ack_pending,
        2,
        "global capacity must remain two across generations",
      );
      assertEquals(full.num_pending, 2);
      assertEquals(full.num_waiting, 0);
      assertEquals(started, ["A", "B", "C"]);
      releases.get("A")!.resolve();
      assertEquals((await a.wait().orThrow()).state, "completed");
      await runtime.waitFor(() => started.includes("D"));
      assert(
        gate.connection(g1)!.outboundContexts.some((out) =>
          out.subject.startsWith("trellis.jobs.") &&
          out.subject.endsWith(`.${a.id}.completed`)
        ),
        "A terminal publication must use G1",
      );
      assert(
        gate.connection(g1)!.outboundContexts.some((out) =>
          out.subject.startsWith("$JS.ACK.")
        ),
        "A disposition must use G1",
      );
      assertEquals(
        gate.connection(g1)!.deliveries.filter((delivery) =>
          delivery.subject === workSubject
        ).length,
        1,
      );
      await runtime.waitFor(async () =>
        !(await inventory()).some((connection) =>
          brokerConnectionKey(connection) === originalKey
        )
      );
      const reapObservedAt = Date.now();
      for (const name of ["C", "D", "E"]) releases.get(name)!.resolve();
      await runtime.waitFor(async () =>
        (await info()).num_ack_pending === 0 && (await info()).num_pending === 0
      );
      assert(
        gate.connection(g2)!.outboundContexts.some((out) =>
          out.subject.startsWith("trellis.jobs.") &&
          out.subject.endsWith(`.${b.id}.completed`)
        ),
        "B terminal publication must use G2",
      );
      const currentAttachments = await attachments();
      assertEquals(currentAttachments.length, 1);
      const currentDigests = new Set(
        currentAttachments.map((attachment) => attachment.contextDigest),
      );
      const currentConnections = admittedConnections(
        await inventory(),
        currentDigests,
      );
      assertEquals(currentConnections.length, 1);
      const currentKey = brokerConnectionKey(currentConnections[0]);
      // A still-healthy projection can be stale for 90 seconds. Require a new
      // persisted heartbeat from this same logical worker after physical reap.
      const postReapPresence = await database.execute({
        sql:
          "SELECT heartbeat_at FROM worker_presence_projection WHERE service = ? AND job_type = ? AND instance_id = ?",
        args: [workerService, "keyedWork", workerId],
      });
      assertEquals(postReapPresence.rows.length, 1);
      const priorHeartbeat = Date.parse(
        String(postReapPresence.rows[0].heartbeat_at),
      );
      await runtime.waitFor(async () => {
        const result = await database.execute({
          sql:
            "SELECT heartbeat_at FROM worker_presence_projection WHERE service = ? AND job_type = ? AND instance_id = ?",
          args: [workerService, "keyedWork", workerId],
        });
        assertEquals(result.rows.length, 1);
        const timestamp = Date.parse(String(result.rows[0].heartbeat_at));
        return timestamp > priorHeartbeat && timestamp > reapObservedAt;
      }, { timeoutMs: 40_000, intervalMs: 200 });
      assert(
        gate.connection(g2)!.outboundContexts.some((out) =>
          out.subject.endsWith(`.keyedWork.${workerId}.heartbeat`)
        ),
        "the same worker must publish fresh presence on G2 after G1 is gone",
      );
      const ackCount = gate.connection(g2)!.outboundContexts.filter((out) =>
        out.subject.startsWith("$JS.ACK.")
      ).length;
      const f = await submit("F");
      await runtime.waitFor(() =>
        started.includes("F")
      );
      releases.get("F")!.resolve();
      releases.set("G", Promise.withResolvers<void>());
      const submitted = await service.jobs.keyedWork.submit({
        key: "G",
        value: "G",
      }).orThrow();
      assertEquals(submitted.kind, "accepted");
      if (submitted.kind !== "accepted") throw new Error("G must be accepted");
      await runtime.waitFor(() => started.includes("G"));
      releases.get("G")!.resolve();
      await runtime.waitFor(async () => {
        const state = await info();
        return completed.includes("F") && completed.includes("G") &&
          state.num_ack_pending === 0 && state.num_pending === 0;
      });
      // Read durable terminal events independently of the facade's result tracking.
      for (const [name, ref] of [["F", f], ["G", submitted.ref]] as const) {
        const terminal = gate.connection(g2)!.outboundContexts.filter((out) =>
          out.subject.startsWith("trellis.jobs.") &&
          out.subject.endsWith(`.${ref.id}.completed`)
        );
        assertEquals(terminal.length, 1);
        const eventStream = await jsm.streams.find(terminal[0].subject);
        const stored = await jsm.streams.getMessage(eventStream, {
          last_by_subj: terminal[0].subject,
        });
        assert(stored, "completed result must persist in the lifecycle stream");
        const event = JSON.parse(new TextDecoder().decode(stored.data));
        assertEquals(event.state, "completed");
        assertEquals(event.result, { key: name, value: name });
      }
      assertEquals(
        gate.connection(g2)!.outboundContexts.filter((out) =>
          out.subject.startsWith("$JS.ACK.")
        ).length - ackCount,
        2,
      );
      const updating = await service.jobs.work.create({
        value: "post-reap-update",
      }).orThrow();
      await runtime.waitFor(() =>
        gate.connection(g2)!.outboundContexts.some((out) =>
          out.subject.startsWith("trellis.jobs.") &&
          out.subject.endsWith(`.${updating.id}.completed`)
        )
      );
      assertEquals(
        gate.connections().flatMap((connection) =>
          connection.outboundContexts.filter((out) =>
            out.subject.startsWith("trellis.job_updates.") &&
            out.subject.endsWith(`.${updating.id}`)
          ).map(() => connection.id)
        ),
        [g2],
        "attempt-scoped typed update must publish on the receiving G2, not the reaped facade baseline",
      );
      assertEquals(started, [
        "A",
        "B",
        "C",
        "D",
        "E",
        "F",
        "G",
      ]);
      assertEquals((await attachments()).length, 1);
      assertEquals(
        admittedConnections(await inventory(), currentDigests).map(
          brokerConnectionKey,
        ),
        [currentKey],
        "finite submissions must retain the existing G2 broker identity without another attachment",
      );
      console.log(
        "managed Jobs: candidate pulls=0; capacity=2; exact G1 reaped; retained create F + submit G completed on unchanged G2; durable results and broker-settled PUB/ACK",
      );
    } finally {
      await hold?.release();
      await applied?.catch(() => undefined);
      for (const release of releases.values()) release.resolve();
      await service.stop();
      await exited;
      await observer.close();
      await system.close();
      database.close();
    }
  }, {
    interruptibleNativeProxy: true,
    authorization: {
      contextLifetimeSeconds: 76,
      refreshLeadSeconds: 15,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 46,
    },
  });
});

Deno.test("TS managed Jobs stop interrupts withheld preparation without late intake", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.apply({ contract });
    const instance = await runtime.services.createInstance({
      name: "jobs-setup-stop-ts",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "jobs-setup-stop-ts",
      seed: instance.seed,
    }).orThrow();
    const observer = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/trellis-auth.creds`),
      ),
    });
    const jsm = await jetstreamManager(observer);
    const gate = runtime.nativeTransportGate();
    let exited: Promise<unknown> | undefined;
    let hold: ReturnType<typeof gate.armResponseHold> | undefined;
    let handled = 0;
    try {
      const queues = [];
      for await (const stream of jsm.streams.list()) {
        for await (const consumer of jsm.consumers.list(stream.config.name)) {
          if (consumer.config.filter_subject?.endsWith(".keyedWork")) {
            queues.push({ stream: stream.config.name, consumer });
          }
        }
      }
      assertEquals(queues.length, 1);
      const { stream, consumer } = queues[0];
      const job = await service.jobs.keyedWork.create({
        key: "queued",
        value: "queued",
      }).orThrow();
      service.jobs.keyedWork.handle(({ job }) => {
        handled++;
        return Promise.resolve(Result.ok(job.payload));
      }, { concurrency: 2 });
      hold = gate.armResponseHold(
        `$JS.API.CONSUMER.INFO.${stream}.${consumer.name}`,
      );
      let held: Awaited<typeof hold.held> | undefined;
      void hold.held.then((value) => held = value);
      exited = service.wait().catch((error: unknown) => error);
      await runtime.waitFor(() => held);
      assert(held);
      let stopped = false;
      const stopping = service.stop().then(() => stopped = true);
      // The real response is still withheld: stop must not need it to finish.
      await runtime.waitFor(() => stopped, { timeoutMs: 10_000 });
      await stopping;
      await hold.release();
      hold = undefined;
      assertEquals(
        await exited,
        undefined,
        "requested startup stop must settle wait normally",
      );
      const info = await jsm.consumers.info(stream, consumer.name);
      assertEquals(info.num_ack_pending, 0);
      assertEquals(info.num_pending, 1);
      assertEquals(handled, 0);
      assertEquals(
        gate.connection(held.connectionId)!.outboundContexts.filter((out) =>
          out.subject === `$JS.API.CONSUMER.MSG.NEXT.${stream}.${consumer.name}`
        ).length,
        0,
      );
      const native = jetstream(observer).consumers.getConsumerFromInfo(info);
      const remaining = await native.next({ expires: 1_000 });
      assert(remaining);
      assertEquals(JSON.parse(remaining.string()).jobId, job.id);
      assertEquals(
        remaining.info.deliveryCount,
        1,
        "stopped startup must leave the real job untouched",
      );
      assert(await remaining.ackAck());
      console.log(
        "managed Jobs setup stop: completed before held INFO release; no pull or handler; queued job remained first delivery",
      );
    } finally {
      await hold?.release();
      await service.stop();
      await exited;
      await observer.close();
    }
  }, { interruptibleNativeProxy: true });
});
