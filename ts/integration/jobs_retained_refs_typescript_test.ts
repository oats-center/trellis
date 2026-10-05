/** Retained native JobRefs use finite logical exchanges, not creation sockets. */
import { assert, assertEquals } from "@std/assert";
import { jetstreamManager } from "@nats-io/jetstream";
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

Deno.test("TS retained Jobs refs get/wait/cancel after broker-confirmed G1 reap", async () => {
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
      name: "retained-jobs-ts",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "retained-jobs-ts",
      seed: instance.seed,
    }).orThrow();
    const system = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          `${runtime.workdir}/config/trellis/nats/creds/system.creds`,
        ),
      ),
    });
    const observer = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          `${runtime.workdir}/config/trellis/nats/creds/trellis-auth.creds`,
        ),
      ),
    });
    const jsm = await jetstreamManager(observer);
    const gate = runtime.nativeTransportGate();
    const releases = new Map<
      string,
      ReturnType<typeof Promise.withResolvers<void>>
    >();
    const started: string[] = [];
    const cancelled: string[] = [];
    let exited: Promise<unknown> | undefined;
    let waiting: Promise<unknown> | undefined;
    const attachments = async () => {
      const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
        items: { participantId: string; contextDigest: string }[];
      };
      return page.items.filter((item) =>
        item.participantId === contract.identity
      );
    };
    try {
      // Retention spans authorization growth, not the work queue's 5s deadline.
      service.jobs.keyedWork.handle(async ({ job }) => {
        const name = job.payload.value;
        started.push(name);
        if (name === "cancel") {
          await new Promise<void>((resolve) => {
            const aborted = () => {
              cancelled.push(name);
              resolve();
            };
            job.signal.addEventListener("abort", aborted, { once: true });
            if (job.signal.aborted) aborted();
          });
        } else {
          await releases.get(name)!.promise;
        }
        return Result.ok({ key: job.payload.key, value: `result:${name}` });
      }, { concurrency: 2 });
      exited = service.wait().catch((error: unknown) => error);
      await runtime.waitFor(() =>
        gate.connections().some((connection) =>
          connection.outboundContexts.some((out) =>
            out.subject.startsWith("$JS.API.CONSUMER.MSG.NEXT.")
          )
        )
      ).catch((cause) => {
        console.log(JSON.stringify(gate.connections()));
        throw cause;
      });
      const create = async (name: string) => {
        releases.set(name, Promise.withResolvers<void>());
        return await service.jobs.keyedWork.create({ key: name, value: name })
          .orThrow();
      };
      const a = await create("A");
      await runtime.waitFor(() => started.includes("A"));
      const g1 = gate.connections().find((connection) =>
        connection.deliveries.some((delivery) =>
          delivery.subject.endsWith(".keyedWork")
        )
      )!.id;
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
      const eventPrefix = gate.connection(g1)!.outboundContexts.find((out) =>
        out.subject.startsWith("trellis.jobs.") &&
        out.subject.endsWith(`.${a.id}.started`)
      )!.subject.split(".").slice(0, -2).join(".");
      // Observe before growth: this finite wait must remain selected on G1.
      waiting = a.wait().orThrow();
      await runtime.waitFor(() =>
        gate.connection(g1)!.subs.some((sub) =>
          sub.subject === `${eventPrefix}.${a.id}.*`
        )
      );
      await runtime.contracts.apply({ contract });
      await runtime.waitFor(async () =>
        (await attachments()).some((item) =>
          item.contextDigest !== initial.contextDigest
        ), { timeoutMs: 90_000 });
      await runtime.waitFor(() =>
        gate.connections().some((connection) =>
          connection.id !== g1 && connection.outboundContexts.some((out) =>
            out.subject.startsWith("$JS.API.CONSUMER.MSG.NEXT.")
          )
        )
      );
      const b = await create("B");
      await runtime.waitFor(() =>
        started.includes("B")
      );
      const g2 = gate.connections().find((connection) =>
        connection.id !== g1 && connection.deliveries.some((delivery) =>
          delivery.subject.endsWith(".keyedWork")
        )
      )!.id;
      assert(g2 !== g1);
      assert(
        !gate.connection(g2)!.subs.some((sub) =>
          sub.subject === `${eventPrefix}.${a.id}.*`
        ),
        "an already selected wait must not migrate",
      );
      releases.get("A")!.resolve();
      assertEquals(
        (await waiting as { result?: { key: string; value: string } }).result,
        {
          key: "A",
          value: "result:A",
        },
      );
      waiting = undefined;
      releases.get("B")!.resolve();
      await runtime.waitFor(async () =>
        !(await inventory()).some((connection) =>
          brokerConnectionKey(connection) === originalKey
        )
      );
      const current = await attachments();
      const survivors = admittedConnections(
        await inventory(),
        new Set(current.map((item) =>
          item.contextDigest
        )),
      );
      assertEquals(survivors.length, 1);
      const survivingKey = brokerConnectionKey(survivors[0]);
      console.log(
        JSON.stringify({
          phase: "G1-reaped",
          originalKey,
          survivingKey,
          g1,
          g2,
        }),
      );
      assertEquals((await b.wait().orThrow()).result, {
        key: "B",
        value: "result:B",
      });

      // Created after G1 is absent: baseline caches cannot have terminal results.
      const f = await create("F");
      await runtime.waitFor(() =>
        started.includes("F")
      );
      releases.get("F")!.resolve();
      await runtime.waitFor(async () => {
        const stream = await jsm.streams.find(
          `${eventPrefix}.${f.id}.completed`,
        );
        try {
          const completed = await jsm.streams.getMessage(stream, {
            last_by_subj: `${eventPrefix}.${f.id}.completed`,
          });
          return completed !== null;
        } catch {
          return false;
        }
      });
      const got = await f.get().orThrow();
      assertEquals(got.state, "completed");
      assertEquals(got.result, { key: "F", value: "result:F" });
      assertEquals((await f.wait().orThrow()).result, {
        key: "F",
        value: "result:F",
      });

      const h = await create("H");
      await runtime.waitFor(() =>
        started.includes("H")
      );
      waiting = h.wait().orThrow();
      await runtime.waitFor(() =>
        gate.connection(g2)!.subs.some((sub) =>
          sub.subject === `${eventPrefix}.${h.id}.*`
        )
      );
      releases.get("H")!.resolve();
      assertEquals(
        (await waiting as { result?: { key: string; value: string } }).result,
        {
          key: "H",
          value: "result:H",
        },
      );
      waiting = undefined;
      const c = await create("cancel");
      await runtime.waitFor(() => started.includes("cancel"));
      assertEquals((await c.cancel().orThrow()).state, "cancelled");
      await runtime.waitFor(() => cancelled.includes("cancel"));
      assertEquals(cancelled, ["cancel"]);
      assertEquals(
        gate.connection(g2)!.outboundContexts.filter((out) =>
          out.subject === `${eventPrefix}.${c.id}.cancelled`
        ).length,
        1,
      );
      assertEquals((await c.get().orThrow()).state, "cancelled");
      assertEquals((await c.wait().orThrow()).state, "cancelled");
      const fresh = await create("fresh");
      await runtime.waitFor(() => started.includes("fresh"));
      releases.get("fresh")!.resolve();
      assertEquals((await fresh.wait().orThrow()).result, {
        key: "fresh",
        value: "result:fresh",
      });
      const final = admittedConnections(
        await inventory(),
        new Set((await attachments()).map((item) => item.contextDigest)),
      );
      assertEquals(final.map(brokerConnectionKey), [survivingKey]);
      console.log(
        JSON.stringify({
          phase: "retained-refs-pass",
          originalKey,
          survivingKey,
          cancelled,
        }),
      );
    } finally {
      for (const release of releases.values()) release.resolve();
      await service.stop();
      await exited;
      await waiting?.catch(() => {});
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
});
