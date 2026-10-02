/** Real managed Jobs receiving-source loss and exact safe-survivor reuse. */
import { assert, assertEquals } from "@std/assert";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants as webParticipants } from "trellis-web-generated";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

type GrantBinding = {
  revision: bigint;
  expiresAt: bigint | null;
  grants: {
    format: string;
    permissions: { action: string; target: Uint8Array }[];
  };
  installedRevision: bigint;
  ownerId: string;
  ownerKind: string;
  participantId: string;
  platformPrivileges: string[];
};

Deno.test(
  "TS managed Jobs source loss keeps slots and reinstates the exact survivor",
  async () => {
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
        name: "jobs-reduction-ts",
        contract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        name: "jobs-reduction-ts",
        seed: instance.seed,
      }).orThrow();
      const system = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(`${runtime.workdir}/nats/creds/system.creds`),
        ),
      });
      const observer = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            `${runtime.workdir}/nats/creds/trellis-auth.creds`,
          ),
        ),
      });
      const jsm = await jetstreamManager(observer);
      const gate = runtime.nativeTransportGate();
      const consoleClient = await runtime.connectClient({
        name: "jobs-reduction-observer",
        contract: webParticipants.Console.participant,
      });
      const releaseA = Promise.withResolvers<void>();
      const releaseB = Promise.withResolvers<void>();
      const releaseC = Promise.withResolvers<void>();
      const started: string[] = [];
      const signals = new Map<string, AbortSignal>();
      let bReturned = false;
      let exited: Promise<unknown> | undefined;
      let hold: ReturnType<typeof gate.armResponseHold> | undefined;
      const binding = async () => {
        const page = await runtime.callAdminRpc("authGrantsList", {
          participantId: contract.identity,
        }) as { items: GrantBinding[] };
        const found = page.items.find((item) =>
          item.participantId === contract.identity
        );
        assert(found);
        return found;
      };
      const atomKey = (atom: GrantBinding["grants"]["permissions"][number]) =>
        `${atom.action}:${btoa(String.fromCharCode(...atom.target))}`;
      const attachments = async () => {
        const page = await runtime.callAdminRpc(
          "authConnectionsList",
          {},
        ) as {
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
          signals.set(name, job.signal);
          if (name === "A") await releaseA.promise;
          if (name === "B") {
            const aborted = Promise.withResolvers<void>();
            job.signal.addEventListener("abort", () => aborted.resolve(), {
              once: true,
            });
            if (job.signal.aborted) aborted.resolve();
            await aborted.promise;
            // Cancellation does not free this slot until the handler returns.
            await releaseB.promise;
            bReturned = true;
          }
          if (name === "C") await releaseC.promise;
          return Result.ok(job.payload);
        }, { concurrency: 2 });
        service.jobs.work.handle(({ job }) =>
          Promise.resolve(Result.ok(job.payload))
        );
        exited = service.wait().catch((error: unknown) => error);
        const a = await service.jobs.keyedWork.create({
          key: "A",
          value: "A",
        })
          .orThrow();
        await runtime.waitFor(() => started.includes("A"));
        const queues = [];
        for await (const stream of jsm.streams.list()) {
          for await (
            const consumer of jsm.consumers.list(stream.config.name)
          ) {
            if (consumer.config.filter_subject?.endsWith(".keyedWork")) {
              queues.push({ stream: stream.config.name, consumer });
            }
          }
        }
        assertEquals(queues.length, 1);
        const { stream, consumer } = queues[0];
        const workSubject = consumer.config.filter_subject!;
        const lifecyclePrefix = workSubject.replace(
          "trellis.work.",
          "trellis.jobs.",
        );
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
        const original = originals[0];
        const originalKey = brokerConnectionKey(original);
        const adopted = new Set(
          (await binding()).grants.permissions.map(atomKey),
        );

        hold = gate.armResponseHold(
          `$JS.API.CONSUMER.INFO.${stream}.${consumer.name}`,
        );
        let prepared: Awaited<typeof hold.held> | undefined;
        void hold.held.then((value) => prepared = value);
        await runtime.contracts.apply({ contract });
        await runtime.waitFor(() => prepared, { timeoutMs: 90_000 });
        assert(prepared);
        const g2 = prepared.connectionId;
        assert(g2 !== g1);
        assertEquals(
          gate.connection(g2)!.outboundContexts.filter((out) =>
            out.subject === pull
          ).length,
          0,
          "an unpublished candidate cannot reserve a slot",
        );
        await hold.release();
        hold = undefined;
        await runtime.waitFor(async () =>
          gate.connection(g2)!.outboundContexts.some((out) =>
            out.subject === pull
          ) && (await info()).num_waiting === 1
        );
        const b = await service.jobs.keyedWork.create({
          key: "B",
          value: "B",
        })
          .orThrow();
        await runtime.waitFor(() => started.includes("B"));
        assertEquals(
          gate.connection(g2)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length,
          1,
          "B must actually receive on G2",
        );
        const grown = admittedConnections(
          await completeBrokerInventory(system, {
            requiredServerIds: [original.server],
          }),
          new Set((await attachments()).map((item) => item.contextDigest)),
        );
        assertEquals(grown.length, 2);
        const wider = grown.find((connection) =>
          brokerConnectionKey(connection) !== originalKey
        );
        assert(wider);
        const widerKey = brokerConnectionKey(wider);
        const inventory = () =>
          completeBrokerInventory(system, {
            requiredServerIds: [original.server, wider.server],
          });
        // Queue C before reduction so this proof does not depend on finite API
        // migration after a facade's original attachment has been reaped.
        const c = await service.jobs.keyedWork.create({
          key: "C",
          value: "C",
        })
          .orThrow();
        const cWork = await jsm.direct.getMessage(stream, {
          last_by_subj: workSubject,
        });
        assert(cWork);
        assertEquals(JSON.parse(cWork.string()).jobId, c.id);
        const full = await info();
        assertEquals(full.num_ack_pending, 2);
        assertEquals(full.num_pending, 1);
        assertEquals(full.num_waiting, 0);
        assertEquals(started, ["A", "B"]);
        const pullsBefore = [g1, g2].map((id) =>
          gate.connection(id)!.outboundContexts.filter((out) =>
            out.subject === pull
          ).length
        );
        const current = await binding();
        const retained = current.grants.permissions.filter((atom) =>
          adopted.has(atomKey(atom))
        );
        assertEquals(retained.length, adopted.size);
        assert(current.grants.permissions.length > retained.length);
        // Same supported narrowing as F3: remove precisely the newly approved
        // optional authority, preserving every originally adopted queue atom.
        hold = gate.armResponseHold(
          `$JS.API.CONSUMER.INFO.${stream}.${consumer.name}`,
          g1,
        );
        let reinstated: Awaited<typeof hold.held> | undefined;
        void hold.held.then((value) => reinstated = value);
        await runtime.callAdminRpc("authGrantsSet", {
          expectedRevision: current.revision,
          expiresAt: current.expiresAt,
          grants: { format: current.grants.format, permissions: retained },
          idempotencyKey: crypto.randomUUID(),
          installedRevision: current.installedRevision,
          ownerId: current.ownerId,
          ownerKind: current.ownerKind,
          participantId: current.participantId,
          platformPrivileges: current.platformPrivileges,
        });
        await runtime.waitFor(async () => {
          const connections = await inventory();
          assert(
            connections.some((item) =>
              brokerConnectionKey(item) === originalKey
            ),
            "the exact G1 must remain admitted during source loss",
          );
          return !connections.some((item) =>
            brokerConnectionKey(item) === widerKey
          );
        });
        await runtime.waitFor(() => signals.get("B")!.aborted);
        // Observe the actual broker reply to G1's replacement preparation,
        // not just a later delivery that could use an unreplaced old source.
        await runtime.waitFor(() => reinstated);
        assertEquals(reinstated!.connectionId, g1);
        await hold.release();
        hold = undefined;
        assertEquals(signals.get("B")!.reason, "lease-lost");
        assert(
          !signals.get("A")!.aborted,
          "G2 loss must not cancel G1's job",
        );
        assert(!bReturned);
        assertEquals(started, ["A", "B"]);
        const heldAfterLoss = await info();
        assertEquals(heldAfterLoss.num_pending, 1);
        assertEquals(heldAfterLoss.num_waiting, 0);
        assert(heldAfterLoss.num_ack_pending <= 2);
        assertEquals(
          [g1, g2].map((id) =>
            gate.connection(id)!.outboundContexts.filter((out) =>
              out.subject === pull
            ).length
          ),
          pullsBefore,
          "reinstatement cannot restart/duplicate slots while A and B still own them",
        );
        // Physical interruption is not a durable terminal state. End the job
        // through its public cancellation API after letting the interrupted
        // attempt return, so later broker redelivery can settle the tombstone.
        const bLast = await jsm.direct.getMessage("JOBS", {
          last_by_subj: `${lifecyclePrefix}.${b.id}.*`,
        });
        assert(bLast);
        const bInterrupted = JSON.parse(bLast.string());
        assertEquals(bInterrupted.state, "active");
        assertEquals(bInterrupted.tries, 1);
        releaseB.resolve();
        await runtime.waitFor(() => started.includes("C"));
        assert(
          bReturned,
          "the interrupted handler must return before C starts",
        );
        assert(!signals.get("A")!.aborted);
        assertEquals(started, ["A", "B", "C"]);
        await consoleClient.cancel({ id: b.id }).orThrow();
        assert(
          gate.connection(g1)!.deliveries.filter((delivery) =>
            delivery.subject === workSubject
          ).length >= 2,
          "new work must receive on the same reinstated G1",
        );
        releaseC.resolve();
        const cTerminal = await runtime.waitFor(async () => {
          const inspected = await consoleClient.jobsInspect({ id: c.id })
            .orThrow();
          return inspected.job.state === "completed" ? inspected.job : false;
        });
        assertEquals(cTerminal.state, "completed");
        assert(cTerminal.result);
        assertEquals(JSON.parse(new TextDecoder().decode(cTerminal.result)), {
          key: "C",
          value: "C",
        });
        releaseA.resolve();
        const aTerminal = await runtime.waitFor(async () => {
          const inspected = await consoleClient.jobsInspect({ id: a.id })
            .orThrow();
          return inspected.job.state === "completed" ? inspected.job : false;
        });
        assertEquals(aTerminal.state, "completed");
        assert(aTerminal.result);
        assertEquals(JSON.parse(new TextDecoder().decode(aTerminal.result)), {
          key: "A",
          value: "A",
        });
        await runtime.waitFor(async () => {
          const settled = await info();
          assert(settled.num_ack_pending + settled.num_waiting <= 2);
          return settled.num_ack_pending === 0 && settled.num_pending === 0 &&
            settled.ack_floor.stream_seq >= cWork.seq;
        });
        const finalInventory = await inventory();
        assert(
          finalInventory.some((item) =>
            brokerConnectionKey(item) === originalKey
          ),
          "the exact G1 must remain admitted after its receipts settle",
        );
        assert(
          !signals.get("A")!.aborted,
          `A completed once and its broker ACK floor settled on live G1; replaced-source disposal must not cancel its accounted receipt (reason: ${
            signals.get("A")!.reason
          })`,
        );
        assertEquals(
          started.filter((name) => name === "A").length,
          1,
          "A must execute exactly once through replacement and broker settlement",
        );
        assert(
          !finalInventory.some((item) =>
            brokerConnectionKey(item) === widerKey
          ),
        );
        for (const job of [a, c]) {
          assert(
            gate.connection(g1)!.outboundContexts.some((out) =>
              out.subject.startsWith("trellis.jobs.") &&
              out.subject.endsWith(`.${job.id}.completed`)
            ),
            "survivor jobs must publish their terminal result on receiving G1",
          );
        }
        assert(
          gate.connection(g1)!.outboundContexts.some((out) =>
            out.subject.startsWith(`$JS.ACK.${stream}.`) &&
            Number(out.subject.split(".").at(-4)) === cWork.seq
          ),
          "C's receiving G1 must send its disposition",
        );
        // Progress frames share the ACK subject; the broker's settled ack floor
        // above, not a count of raw ACK-prefixed frames, proves settlement.
        assert(
          !gate.connections().some((connection) =>
            connection.outboundContexts.some((out) =>
              out.subject.startsWith("trellis.jobs.") &&
              out.subject.endsWith(`.${b.id}.completed`)
            )
          ),
          "an interrupted B must never complete on a survivor or lost source",
        );
        const bLifecycle = await jsm.direct.getMessage("JOBS", {
          last_by_subj: `${lifecyclePrefix}.${b.id}.*`,
        });
        assert(bLifecycle);
        assertEquals(JSON.parse(bLifecycle.string()).state, "cancelled");
        const bPersisted = await consoleClient.jobsInspect({ id: b.id })
          .orThrow();
        assertEquals(bPersisted.job.state, "cancelled");
        console.log(
          `managed Jobs reduction: G2=${widerKey} absent; G1=${originalKey} stable; B lease-loss cancellation held its slot; C received/completed/settled on G1; A uncancelled completed`,
        );
      } finally {
        await hold?.release();
        releaseA.resolve();
        releaseB.resolve();
        releaseC.resolve();
        await service.stop();
        await exited;
        await consoleClient.connection.close();
        await observer.close();
        await system.close();
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
  },
);
