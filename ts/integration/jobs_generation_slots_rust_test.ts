/** Real broker proof of two persistent Rust Jobs slots across publication. */
import { assert, assertEquals } from "@std/assert";
import { createClient } from "@libsql/client";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

type Leg = {
  send(command: string): Promise<void>;
  waitFor(marker: string): Promise<void>;
  lines: string[];
};

/** Keeps the ordinary fixture process and its command pipe bounded and reaped. */
async function withLeg(
  runtime: TrellisTestRuntime,
  seed: string,
  body: (leg: Leg) => Promise<void>,
  concurrency = "2",
): Promise<void> {
  const child = new Deno.Command("setsid", {
    args: rustFixtureArgv("transport_growth_operation"),
    env: {
      TRELLIS_URL: runtime.trellisUrl,
      TRELLIS_IDENTITY_SEED: seed,
      TRELLIS_JOB_CONCURRENCY: concurrency,
    },
    stdin: "piped",
    stdout: "piped",
    stderr: "inherit",
  }).spawn();
  let exited = false;
  const status = child.status.then((value) => {
    exited = true;
    return value;
  });
  const stdin = child.stdin.getWriter();
  const lines: string[] = [];
  const drain = (async () => {
    let pending = "";
    for await (
      const chunk of child.stdout.pipeThrough(new TextDecoderStream())
    ) {
      pending += chunk;
      const complete = pending.split("\n");
      pending = complete.pop() ?? "";
      lines.push(...complete.map((line) => line.trim()));
    }
  })();
  const leg: Leg = {
    lines,
    async send(command) {
      await stdin.write(new TextEncoder().encode(`${command}\n`));
    },
    async waitFor(marker) {
      await runtime.waitFor(() => {
        const error = lines.find((line) => /_ERROR /.test(line));
        if (error !== undefined) throw new Error(error);
        if (lines.includes(marker)) return true;
        if (exited) throw new Error(`fixture exited: ${lines.join("\n")}`);
        return undefined;
      }, { timeoutMs: 30_000, intervalMs: 50 });
    },
  };
  try {
    await body(leg);
    await leg.send("EXIT");
    await leg.waitFor("OPERATION_PROVIDER_DONE");
    await runtime.waitFor(() => exited ? true : undefined);
    assert((await status).success, lines.join("\n"));
  } catch (cause) {
    throw new Error(`Jobs fixture markers:\n${lines.join("\n")}`, { cause });
  } finally {
    await stdin.close().catch(() => undefined);
    if (!exited) {
      try {
        Deno.kill(-child.pid, "SIGKILL");
      } catch {
        // The process may have already exited.
      }
    }
    await Promise.allSettled([status, drain]);
  }
}

async function verifyJobsSlots(cancelOverlap: boolean): Promise<void> {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.TransportGrowthOperationProvider.participant;
    const deployment = "jobs-slots-rust";
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({
      deployment,
      contract,
    });
    assert(requested.status === "approval_required");
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeCapabilities: ["runtime-trellis.transport_growth@v1::extend"],
    });
    const instance = await runtime.services.createInstance({
      deployment,
      name: deployment,
      contract,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          `${runtime.workdir}/nats/creds/trellis-auth.creds`,
        ),
      ),
    });
    const manager = await jetstreamManager(nats);
    const database = createClient({
      url: `file:${runtime.workdir}/trellis/trellis.sqlite.jobs`,
    });
    const gate = runtime.nativeTransportGate();
    const attachments = async () => {
      const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
        items: { participantId: string; contextDigest: string }[];
      };
      return page.items.filter((item) =>
        item.participantId === contract.identity
      );
    };
    try {
      await withLeg(runtime, instance.seed, async (leg) => {
        await leg.waitFor("OPERATION_PROVIDER_READY");
        await leg.waitFor("JOB_STARTED 1 initial");
        // Discover the real provisioned queue; do not invent consumer names.
        const queues = [];
        for await (const stream of manager.streams.list()) {
          for await (
            const consumer of manager.consumers.list(stream.config.name)
          ) {
            if (consumer.config.filter_subject?.endsWith(".held")) {
              queues.push({ stream: stream.config.name, consumer });
            }
          }
        }
        assertEquals(queues.length, 1);
        const { stream, consumer } = queues[0];
        const consumerName = consumer.name;
        const workSubject = consumer.config.filter_subject!;
        const pull = `$JS.API.CONSUMER.MSG.NEXT.${stream}.${consumerName}`;
        const info = () => manager.consumers.info(stream, consumerName);
        const receivers = gate.connections().filter((connection) =>
          connection.deliveries.some((delivery) =>
            delivery.subject === workSubject
          )
        );
        assertEquals(receivers.length, 1);
        const g1 = receivers[0].id;
        const worker = await runtime.waitFor(async () => {
          const result = await database.execute({
            sql:
              "SELECT service, instance_id, heartbeat_at FROM worker_presence_projection WHERE job_type = ?",
            args: ["held"],
          });
          assert(
            result.rows.length <= 1,
            "the fixture owns one held worker host",
          );
          return result.rows[0];
        });
        const workerId = String(worker.instance_id);
        const workerService = String(worker.service);
        const heartbeatSubject = gate.connection(g1)!.outboundContexts.find(
          (out) => out.subject.endsWith(`.held.${workerId}.heartbeat`),
        )?.subject;
        assert(heartbeatSubject !== undefined);
        const baseline = await attachments();
        assertEquals(baseline.length, 1);
        const originalSockets = admittedConnections(
          await readRuntimeBrokerInventory(runtime),
          new Set([baseline[0].contextDigest]),
        );
        assertEquals(originalSockets.length, 1);
        const originalKey = brokerConnectionKey(originalSockets[0]);
        const server = originalSockets[0].server;
        await leg.send("JOB_WAIT initial");
        await runtime.waitFor(() =>
          gate.connection(g1)!.subs.some((sub) =>
              /\.held\.[^.]+\.\*$/.test(sub.subject)
            )
            ? true
            : undefined
        );

        // Jobs registration precedes runtime.run()'s provider owner. Hold the
        // exact candidate consumer INFO reply, then the later provider readiness
        // reply on that same wire connection. Reaching the latter proves Jobs
        // preparation finished; CONNECT alone would not prove it.
        const preparation = gate.armResponseHold(
          `$JS.API.CONSUMER.INFO.${stream}.${consumerName}`,
        );
        let readiness: ReturnType<typeof gate.armResponseHold> | undefined;
        let g2 = 0;
        try {
          await runtime.contracts.apply({ deployment, contract });
          let prepared: Awaited<typeof preparation.held> | undefined;
          preparation.held.then((value) => {
            prepared = value;
          });
          await runtime.waitFor(() => prepared, {
            timeoutMs: 45_000,
            intervalMs: 50,
          });
          assert(prepared !== undefined);
          g2 = prepared.connectionId;
          assert(g2 !== g1);
          const releasing = preparation.release();
          readiness = gate.armResponseHold("$SYS.REQ.USER.INFO", g2);
          await releasing;
          let ready = false;
          readiness.held.then(() => {
            ready = true;
          });
          await runtime.waitFor(() => ready ? true : undefined);
          const candidate = gate.connection(g2);
          assert(candidate !== undefined && !candidate.closed);
          assert(
            candidate.subs.some((sub) => sub.subject.endsWith(".Advance")),
            "the withheld readiness must follow normal provider route subscription",
          );
          assert(
            candidate.outboundContexts.some((out) =>
              out.subject === `$JS.API.CONSUMER.INFO.${stream}.${consumerName}`
            ),
          );
          // A real broker round trip while readiness is still withheld. G1 may
          // have an older bounded pull, but G2 must not receive before publication.
          assertEquals((await info()).num_ack_pending, 1);
          assertEquals(
            candidate.outboundContexts.filter((out) => out.subject === pull)
              .length,
            0,
          );
          await leg.send("JOB_STATUS initial");
          await leg.waitFor("JOB_STATUS initial Active 1");
          assertEquals(
            gate.connection(g2)!.outboundContexts.filter((out) =>
              out.subject === pull
            ).length,
            0,
          );
        } finally {
          await readiness?.release();
          await preparation.release();
        }

        // The free slot can only issue its first G2 pull after its old G1
        // reservation settles. Do not assume old pulls instantly disappear.
        await runtime.waitFor(async () => {
          const pulls = gate.connection(g2)!.outboundContexts.filter((out) =>
            out.subject === pull
          );
          return pulls.length > 0 && (await info()).num_waiting === 1
            ? true
            : undefined;
        });
        await leg.send("JOB_SUBMIT B");
        await leg.waitFor("JOB_STARTED 2 B");
        assertEquals(
          gate.connection(g2)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length,
          1,
        );
        if (cancelOverlap) {
          // Both logical tokens are active on different receiving generations.
          // Control uses retained public JobRefs, not direct broker publication.
          for (const value of ["initial", "B"]) {
            await leg.send(`JOB_CANCEL ${value}`);
            await leg.waitFor(`JOB_CANCEL ${value} Cancelled 1`);
          }
          assertEquals((await info()).num_ack_pending, 2);
          assert(!leg.lines.includes("JOB_DONE 1 initial"));
          assert(!leg.lines.includes("JOB_DONE 2 B"));
          const held = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [server],
          });
          assert(
            held.some((connection) =>
              brokerConnectionKey(connection) === originalKey
            ),
          );
          await leg.send("JOB_RELEASE B");
          await leg.waitFor("JOB_DONE 2 B");
          await runtime.waitFor(async () =>
            (await info()).num_ack_pending === 1 ? true : undefined
          );
          // A's token must retain its cancellation after B's receive completes.
          await leg.send("JOB_RELEASE initial");
          await leg.waitFor("JOB_DONE 1 initial");
          await leg.waitFor("JOB_WAIT initial Cancelled 1");
          const settled = await runtime.waitFor(async () => {
            const value = await info();
            return value.num_ack_pending === 0 &&
                value.ack_floor.consumer_seq === 2
              ? value
              : undefined;
          });
          assertEquals(settled.num_pending, 0);
          for (
            const [value, receiver, sequence] of [
              ["initial", g1, 1],
              ["B", g2, 2],
            ] as const
          ) {
            await leg.send(`JOB_STATUS ${value}`);
            await leg.waitFor(`JOB_STATUS ${value} Cancelled 1`);
            const id = leg.lines.find((line) =>
              line.startsWith(`JOB_ID ${value} `)
            )!.split(" ")[2];
            const writes = gate.connections().flatMap((connection) =>
              connection.outboundContexts.filter((out) =>
                out.subject.startsWith("trellis.jobs.") &&
                out.subject.endsWith(`.held.${id}.completed`)
              )
            );
            assertEquals(
              writes.length,
              0,
              `cancelled active token must suppress handler success publication: ${
                JSON.stringify(writes)
              }`,
            );
            const acks = gate.connections().flatMap((connection) =>
              connection.outboundContexts.filter((out) =>
                out.subject.startsWith(`$JS.ACK.${stream}.${consumerName}.`) &&
                out.subject.split(".")[5] === String(sequence)
              ).map(() => connection.id)
            );
            assert(
              acks.length > 0 && acks.every((source) => source === receiver),
            );
          }
          await runtime.waitFor(async () => {
            const inventory = await readRuntimeBrokerInventory(runtime, {
              requiredServerIds: [server],
            });
            return inventory.every((connection) =>
                brokerConnectionKey(connection) !== originalKey
              )
              ? true
              : undefined;
          }, { timeoutMs: 45_000, intervalMs: 200 });
          console.log(
            `PASS: retained JobRef cancellation suppresses both G1/G2 handler successes; immutable receiving ACKs settle two jobs; canonical ${originalKey} reaped after last use`,
          );
          return;
        }
        await leg.send("JOB_RELEASE B");
        await leg.waitFor("JOB_DONE 2 B");
        await runtime.waitFor(async () =>
          (await info()).num_ack_pending === 1 ? true : undefined
        );
        await leg.send("JOB_STATUS B");
        await leg.waitFor("JOB_STATUS B Completed 1 held-2");
        assert(
          !leg.lines.includes("JOB_DONE 1 initial"),
          "A must remain held while B completes",
        );
        const heldInventory = await readRuntimeBrokerInventory(runtime, {
          requiredServerIds: [server],
        });
        assert(
          heldInventory.some((connection) =>
            brokerConnectionKey(connection) === originalKey
          ),
          "the real receiving socket must remain present while A is held",
        );

        // Hold both slots, then leave two extra jobs queued at the broker. This
        // catches per-generation capacity duplication and speculative prefetch.
        await leg.send("JOB_SUBMIT C");
        await leg.waitFor("JOB_STARTED 3 C");
        await leg.send("JOB_SUBMIT D");
        await leg.waitFor("JOB_SUBMITTED D");
        await leg.send("JOB_SUBMIT E");
        await leg.waitFor("JOB_SUBMITTED E");
        // Submission queues an ordinary publish; JOBS_WORK receives it through
        // a stream source. Observe both queued jobs at the broker before taking
        // the capacity snapshot, without tolerating an extra delivery or pull.
        const full = await runtime.waitFor(async () => {
          const current = await info();
          assertEquals(current.num_ack_pending, 2);
          assertEquals(current.num_waiting, 0);
          assertEquals(current.delivered.consumer_seq, 3);
          return current.num_pending >= 2 ? current : undefined;
        });
        assertEquals(full.num_ack_pending, 2);
        assertEquals(full.num_pending, 2);
        assertEquals(full.num_waiting, 0);
        assertEquals(full.delivered.consumer_seq, 3);
        assertEquals(
          gate.connection(g1)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length,
          1,
        );
        assertEquals(
          gate.connection(g2)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length,
          2,
        );
        const settledG1Pulls =
          gate.connection(g1)!.outboundContexts.filter((out) =>
            out.subject === pull
          ).length;

        // Only A's slot is freed: C stays held. Its next receive must be on G2,
        // while A's final acknowledgement is emitted on its original G1 socket.
        await leg.send("JOB_RELEASE initial");
        await leg.waitFor("JOB_DONE 1 initial");
        await leg.waitFor("JOB_WAIT initial Completed 1 held-1");
        await leg.waitFor("JOB_STARTED 4 D");
        assertEquals(
          gate.connection(g2)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length,
          3,
        );
        assert(
          gate.connection(g1)!.outboundContexts.some((out) =>
            out.subject.startsWith(`$JS.ACK.${stream}.${consumerName}.`) &&
            out.subject.split(".")[5] === "1"
          ),
          "A disposition must use its actual receiving connection",
        );
        assert(
          !gate.connection(g2)!.outboundContexts.some((out) =>
            out.subject.startsWith(`$JS.ACK.${stream}.${consumerName}.`) &&
            out.subject.split(".")[5] === "1"
          ),
          "A must not be acknowledged through G2",
        );
        const afterA = await info();
        assertEquals(afterA.num_ack_pending, 2);
        assertEquals(afterA.num_pending, 1);
        assertEquals(afterA.ack_floor.stream_seq, 2);
        await leg.send("JOB_STATUS initial");
        await leg.waitFor("JOB_STATUS initial Completed 1 held-1");

        await leg.send("JOB_RELEASE D");
        await leg.waitFor("JOB_DONE 4 D");
        await leg.waitFor("JOB_STARTED 5 E");
        await leg.send("JOB_RELEASE C");
        await leg.send("JOB_RELEASE E");
        await leg.waitFor("JOB_DONE 3 C");
        await leg.waitFor("JOB_DONE 5 E");
        const finalConsumer = await runtime.waitFor(async () => {
          const final = await info();
          return final.num_ack_pending === 0 &&
              final.ack_floor.consumer_seq === 5
            ? final
            : undefined;
        });
        assertEquals(finalConsumer.num_pending, 0);
        assertEquals(finalConsumer.delivered.consumer_seq, 5);
        for (
          const [value, execution] of [["C", 3], ["D", 4], ["E", 5]] as const
        ) {
          await leg.send(`JOB_STATUS ${value}`);
          await leg.waitFor(
            `JOB_STATUS ${value} Completed 1 held-${execution}`,
          );
        }
        // Completed lifecycle publications, not merely handler-return markers,
        // must use each job's receiving wire connection exactly once.
        for (const value of ["initial", "B", "C", "D", "E"]) {
          const idLine = leg.lines.find((line) =>
            line.startsWith(`JOB_ID ${value} `)
          );
          assert(idLine !== undefined);
          const id = idLine.split(" ")[2];
          if (value === "initial") {
            assert(
              gate.connection(g1)!.subs.some((sub) =>
                sub.subject.endsWith(`.held.${id}.*`)
              ),
            );
            assert(
              !gate.connection(g2)!.subs.some((sub) =>
                sub.subject.endsWith(`.held.${id}.*`)
              ),
              "the already-started wait must not reroute onto G2",
            );
          }
          const terminals = gate.connections().flatMap((connection) =>
            connection.outboundContexts.filter((out) =>
              out.subject.startsWith("trellis.jobs.") &&
              out.subject.endsWith(`.held.${id}.completed`)
            ).map(() => connection.id)
          );
          assertEquals(
            terminals,
            [value === "initial" ? g1 : g2],
            `${value}'s terminal outcome must be published once on its receiving connection`,
          );
        }
        assertEquals(
          leg.lines.filter((line) => line.startsWith("JOB_STARTED ")).length,
          5,
        );
        assertEquals(
          leg.lines.filter((line) => line.startsWith("JOB_DONE ")).length,
          5,
        );
        await runtime.waitFor(async () => {
          const inventory = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [server],
          });
          return inventory.every((connection) =>
              brokerConnectionKey(connection) !== originalKey
            )
            ? true
            : undefined;
        }, { timeoutMs: 45_000, intervalMs: 200 });
        const reapObservedAt = Date.now();
        assertEquals(
          gate.connection(g1)!.outboundContexts.filter((out) =>
            out.subject === pull
          ).length,
          settledG1Pulls,
          "G1 must not resume intake after its original reservations settle",
        );
        const currentAttachments = await attachments();
        const currentSockets = admittedConnections(
          await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [server],
          }),
          new Set(
            currentAttachments.map((attachment) => attachment.contextDigest),
          ),
        );
        assertEquals(currentSockets.length, 1);
        assert(brokerConnectionKey(currentSockets[0]) !== originalKey);
        const survivingKey = brokerConnectionKey(currentSockets[0]);

        // A healthy=true projection can be up to 90s old. Require the same
        // logical worker's actual persisted heartbeat to advance after G1 is gone.
        const postReapPresence = await database.execute({
          sql:
            "SELECT heartbeat_at FROM worker_presence_projection WHERE service = ? AND job_type = ? AND instance_id = ?",
          args: [workerService, "held", workerId],
        });
        assertEquals(postReapPresence.rows.length, 1);
        const priorHeartbeat = Date.parse(
          String(postReapPresence.rows[0].heartbeat_at),
        );
        await runtime.waitFor(async () => {
          const result = await database.execute({
            sql:
              "SELECT heartbeat_at FROM worker_presence_projection WHERE service = ? AND job_type = ? AND instance_id = ?",
            args: [workerService, "held", workerId],
          });
          assertEquals(result.rows.length, 1);
          const timestamp = Date.parse(String(result.rows[0].heartbeat_at));
          return timestamp > priorHeartbeat && timestamp > reapObservedAt
            ? true
            : undefined;
        }, { timeoutMs: 40_000, intervalMs: 200 });
        assert(
          gate.connection(g2)!.outboundContexts.some((out) =>
            out.subject === heartbeatSubject
          ),
          "the stable host heartbeat must publish on G2 after baseline reap",
        );

        // Only now submit fresh work: a retained baseline publisher cannot hide
        // behind G1 still being alive while the earlier five jobs complete.
        await leg.send("JOB_SUBMIT F");
        await leg.waitFor("JOB_STARTED 6 F");
        assertEquals(
          gate.connection(g2)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length,
          5,
        );
        await leg.send("JOB_RELEASE F");
        await leg.waitFor("JOB_DONE 6 F");
        const postReapConsumer = await runtime.waitFor(async () => {
          const final = await info();
          return final.num_ack_pending === 0 &&
              final.ack_floor.consumer_seq === 6
            ? final
            : undefined;
        });
        assertEquals(postReapConsumer.num_pending, 0);
        assertEquals(postReapConsumer.delivered.consumer_seq, 6);
        assertEquals(postReapConsumer.ack_floor.stream_seq, 6);
        await leg.send("JOB_STATUS F");
        await leg.waitFor("JOB_STATUS F Completed 1 held-6");
        const freshIdLine = leg.lines.find((line) =>
          line.startsWith("JOB_ID F ")
        );
        assert(freshIdLine !== undefined);
        const freshId = freshIdLine.split(" ")[2];
        assertEquals(
          gate.connections().flatMap((connection) =>
            connection.outboundContexts.filter((out) =>
              out.subject.startsWith("trellis.jobs.") &&
              out.subject.endsWith(`.held.${freshId}.completed`)
            ).map(() => connection.id)
          ),
          [g2],
          "fresh post-reap outcome must publish once on its G2 receiving connection",
        );
        assertEquals(
          gate.connections().flatMap((connection) =>
            connection.outboundContexts.filter((out) =>
              out.subject.startsWith(`$JS.ACK.${stream}.${consumerName}.`) &&
              out.subject.split(".")[5] === "6"
            ).map(() => connection.id)
          ),
          [g2],
          "fresh post-reap disposition must use its G2 receiving connection",
        );
        assertEquals(
          leg.lines.filter((line) => line.startsWith("JOB_STARTED ")).length,
          6,
        );
        assertEquals(
          leg.lines.filter((line) => line.startsWith("JOB_DONE ")).length,
          6,
        );
        const postReapInventory = await readRuntimeBrokerInventory(runtime, {
          requiredServerIds: [server],
        });
        assert(
          postReapInventory.every((connection) =>
            brokerConnectionKey(connection) !== originalKey
          ),
        );
        assertEquals(
          admittedConnections(
            postReapInventory,
            new Set(
              currentAttachments.map((attachment) => attachment.contextDigest),
            ),
          ).map(brokerConnectionKey),
          [survivingKey],
          "the surviving broker attachment must remain stable through fresh work",
        );
        console.log(
          `PASS: prepared G2 has no prepublication pull; A held while B finishes on G2; broker capacity=2 with D/E queued; A terminal/ACK on wire ${g1}, then its slot receives D on wire ${g2}; G1 ${originalKey} reaped before fresh F starts/completes/publishes/ACKs on wire ${g2}; six completed/acked once, ack floor=6 pending=0; current ${survivingKey} stable`,
        );
      });
    } finally {
      database.close();
      await nats.close();
    }
  }, {
    authorization: {
      contextLifetimeSeconds: 62,
      refreshLeadSeconds: 25,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 32,
    },
    interruptibleNativeProxy: true,
  });
}

Deno.test("Rust two Jobs slots retain receiving ownership and follow published growth", () =>
  verifyJobsSlots(false));
Deno.test("Rust logical Jobs cancellation spans overlapping receiving generations", () =>
  verifyJobsSlots(true));

Deno.test("Rust retained G1 JobRefs read wait and cancel after physical G1 reap", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.TransportGrowthOperationProvider.participant;
    const deployment = "jobs-retained-rust";
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({
      deployment,
      contract,
    });
    assert(requested.status === "approval_required");
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeCapabilities: ["runtime-trellis.transport_growth@v1::extend"],
    });
    const instance = await runtime.services.createInstance({
      deployment,
      name: deployment,
      contract,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/trellis-auth.creds`),
      ),
    });
    const manager = await jetstreamManager(nats);
    const gate = runtime.nativeTransportGate();
    try {
      await withLeg(runtime, instance.seed, async (leg) => {
        await leg.waitFor("OPERATION_PROVIDER_READY");
        await leg.waitFor("JOB_STARTED 1 initial");
        const queues = [];
        for await (const stream of manager.streams.list()) {
          for await (
            const consumer of manager.consumers.list(stream.config.name)
          ) {
            if (consumer.config.filter_subject?.endsWith(".held")) {
              queues.push({ stream: stream.config.name, consumer });
            }
          }
        }
        assertEquals(queues.length, 1);
        const { stream, consumer } = queues[0];
        const workSubject = consumer.config.filter_subject!;
        const info = () => manager.consumers.info(stream, consumer.name);
        const receivers = gate.connections().filter((connection) =>
          connection.deliveries.some((delivery) =>
            delivery.subject === workSubject
          )
        );
        assertEquals(receivers.length, 1);
        const g1 = receivers[0].id;
        const attachments = async () => {
          const page = await runtime.callAdminRpc(
            "authConnectionsList",
            {},
          ) as { items: { participantId: string; contextDigest: string }[] };
          return page.items.filter((item) =>
            item.participantId === contract.identity
          );
        };
        const baseline = await attachments();
        const sockets = admittedConnections(
          await readRuntimeBrokerInventory(runtime),
          new Set(baseline.map((item) => item.contextDigest)),
        );
        assertEquals(sockets.length, 1);
        const originalKey = brokerConnectionKey(sockets[0]);
        const server = sockets[0].server;
        await leg.send("JOB_SUBMIT B");
        await leg.waitFor("JOB_SUBMITTED B");
        await leg.send("JOB_STATUS B");
        await leg.waitFor("JOB_STATUS B Pending 0");
        const bId = leg.lines.find((line) =>
          line.startsWith("JOB_ID B ")
        )!.split(" ")[2];
        assertEquals((await info()).num_ack_pending, 1);
        assertEquals((await info()).num_pending, 1);

        const preparation = gate.armResponseHold(
          `$JS.API.CONSUMER.INFO.${stream}.${consumer.name}`,
        );
        let readiness: ReturnType<typeof gate.armResponseHold> | undefined;
        let g2 = 0;
        try {
          await runtime.contracts.apply({ deployment, contract });
          let prepared: Awaited<typeof preparation.held> | undefined;
          preparation.held.then((value) => {
            prepared = value;
          });
          await runtime.waitFor(() => prepared, {
            timeoutMs: 45_000,
            intervalMs: 50,
          });
          assert(prepared !== undefined);
          g2 = prepared.connectionId;
          assert(g2 !== g1);
          const releasing = preparation.release();
          readiness = gate.armResponseHold("$SYS.REQ.USER.INFO", g2);
          await releasing;
          let ready = false;
          readiness.held.then(() => {
            ready = true;
          });
          await runtime.waitFor(() => ready, {
            timeoutMs: 45_000,
            intervalMs: 50,
          });
          assert(gate.connection(g2)!.subs.some((sub) =>
            sub.subject.endsWith(".Advance")
          ));
        } finally {
          await readiness?.release();
          await preparation.release();
        }
        // Candidate preparation and provider readiness have finished. B's actual
        // receiving socket below proves intake adopted the published generation.
        await runtime.waitFor(async () => {
          const current = await attachments();
          return current.some((item) =>
              !baseline.some((old) =>
                old.contextDigest === item.contextDigest
              )
            )
            ? true
            : undefined;
        });
        await leg.send("JOB_RELEASE initial");
        await leg.waitFor("JOB_DONE 1 initial");
        await leg.waitFor("JOB_STARTED 2 B");
        assertEquals(
          gate.connection(g2)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length,
          1,
        );
        await runtime.waitFor(async () => {
          const inventory = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [server],
          });
          return inventory.every((connection) =>
              brokerConnectionKey(connection) !== originalKey
            )
            ? true
            : undefined;
        }, { timeoutMs: 45_000, intervalMs: 200 });
        assertEquals((await info()).num_ack_pending, 1);
        assertEquals((await info()).num_pending, 0);
        const oldWrites = gate.connection(g1)!.outboundContexts.length;
        const currentWrites = gate.connection(g2)!.outboundContexts.length;
        await leg.send("JOB_STATUS initial");
        await leg.waitFor("JOB_STATUS initial Completed 1 held-1");
        await leg.send("JOB_WAIT initial");
        await leg.waitFor("JOB_WAIT initial Completed 1 held-1");
        await leg.send("JOB_STATUS B");
        await leg.waitFor("JOB_STATUS B Active 1");
        await leg.send("JOB_CANCEL B");
        await leg.waitFor("JOB_CANCEL B Cancelled 1");
        assertEquals(
          gate.connections().flatMap((connection) =>
            connection.outboundContexts.filter((out) =>
              out.subject.startsWith("trellis.jobs.") &&
              out.subject.endsWith(`.held.${bId}.cancelled`)
            ).map(() => connection.id)
          ),
          [g2],
        );
        const observationWrites = gate.connection(g2)!.outboundContexts.slice(
          currentWrites,
        );
        assert(
          observationWrites.some((out) =>
            out.subject === "$JS.API.STREAM.MSG.GET.JOBS"
          ),
          "retained refs must project persisted lifecycle through G2",
        );
        assertEquals(
          gate.connection(g1)!.outboundContexts.length,
          oldWrites,
          "retained refs must not publish or query on reaped G1",
        );
        assert(
          !leg.lines.includes("JOB_DONE 2 B"),
          "cancel publishes terminal lifecycle; it does not manufacture handler completion",
        );
        await leg.send("JOB_RELEASE B");
        await leg.waitFor("JOB_DONE 2 B");
        await runtime.waitFor(async () =>
          (await info()).num_ack_pending === 0 ? true : undefined
        );
        await leg.send("JOB_SUBMIT F");
        await leg.waitFor("JOB_STARTED 3 F");
        await leg.send("JOB_RELEASE F");
        await leg.waitFor("JOB_DONE 3 F");
        const final = await runtime.waitFor(async () => {
          const value = await info();
          return value.num_ack_pending === 0 &&
              value.ack_floor.consumer_seq === 3
            ? value
            : undefined;
        });
        assertEquals(final.num_pending, 0);
        assertEquals(final.delivered.consumer_seq, 3);
        await leg.send("JOB_STATUS F");
        await leg.waitFor("JOB_STATUS F Completed 1 held-3");
        for (const sequence of [2, 3]) {
          const sources = gate.connections().flatMap((connection) =>
            connection.outboundContexts.filter((out) =>
              out.subject.startsWith(`$JS.ACK.${stream}.${consumer.name}.`) &&
              out.subject.split(".")[5] === String(sequence)
            ).map(() => connection.id)
          );
          // ACK subjects also carry +WPI heartbeats; the broker floor above
          // proves settlement, while every wire write must use the receiver.
          assert(
            sources.length > 0 && sources.every((source) => source === g2),
            `B/F disposition ${sequence} must use G2`,
          );
        }
        const fId = leg.lines.find((line) =>
          line.startsWith("JOB_ID F ")
        )!.split(" ")[2];
        assertEquals(
          gate.connections().flatMap((connection) =>
            connection.outboundContexts.filter((out) =>
              out.subject.startsWith("trellis.jobs.") &&
              out.subject.endsWith(`.held.${fId}.completed`)
            ).map(() => connection.id)
          ),
          [g2],
        );
        const current = await attachments();
        const inventory = await readRuntimeBrokerInventory(runtime, {
          requiredServerIds: [server],
        });
        assertEquals(
          admittedConnections(
            inventory,
            new Set(current.map((item) => item.contextDigest)),
          ).length,
          1,
        );
        assert(
          inventory.every((connection) =>
            brokerConnectionKey(connection) !== originalKey
          ),
        );
        assertEquals(
          gate.connection(g2)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length,
          2,
        );
        assertEquals(gate.connection(g1)!.outboundContexts.length, oldWrites);
        console.log(
          `PASS: retained G1 refs read/wait A and cancel active B through wire ${g2} after canonical ${originalKey} absence; B released and ACKed, fresh F completed; one current attachment, ack floor=3 pending=0`,
        );
      }, "1");
    } finally {
      await nats.close();
    }
  }, {
    authorization: {
      contextLifetimeSeconds: 62,
      refreshLeadSeconds: 25,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 32,
    },
    interruptibleNativeProxy: true,
  });
});
