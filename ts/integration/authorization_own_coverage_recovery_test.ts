/** Own watch recovery must not depend on (or permit) fresh application IO. */
import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { type ConsumerInfo, jetstream } from "@nats-io/jetstream";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type { ResponseHoldBarrier } from "../packages/trellis-testkit/src/native_gate.ts";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

async function bounded<T>(
  promise: Promise<T>,
  ms: number,
  message: string,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

Deno.test("own coverage resumes on the original publication while fresh KV waits", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const request = await runtime.contracts.requestApply({ contract });
    assert(request.status === "approval_required");
    await runtime.contracts.approveApply(request.pendingId);
    const instance = await runtime.services.createInstance({
      name: "own-watch-provider",
      contract,
    });
    const system = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
        ),
      ),
    });
    const privileged = await connect({
      servers: runtime.nativeProxyUrl(),
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "config/trellis/nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    try {
      const jsm = await jetstream(privileged).jetstreamManager();
      const gate = runtime.nativeTransportGate();
      // First bootstrap resolves provider bindings and queues a refresh hint.
      // Observe that real publication before constructing the fault-test service:
      // its recipient snapshot cannot include a connection that does not exist yet.
      const hints = privileged.subscribe("_INBOX.*._trellis.authorization");
      const seenHints = new Set<string>();
      const hintPump = (async () => {
        for await (const message of hints) seenHints.add(message.subject);
      })();
      void hintPump.catch(() => undefined);
      try {
        await privileged.flush();
        const beforeWarmup = new Set(gate.connections().map((item) => item.id));
        const warmup = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: contract,
          name: "own-watch-provider",
          seed: instance.seed,
        }).orThrow();
        const warmupExit = warmup.wait().catch((error: unknown) => error);
        try {
          const warmupHints = gate.connections().filter((connection) =>
            !beforeWarmup.has(connection.id)
          ).flatMap((connection) =>
            connection.subs.filter((sub) =>
              sub.subject.endsWith("._trellis.authorization")
            )
          );
          assertEquals(warmupHints.length, 1);
          await runtime.waitFor(() => seenHints.has(warmupHints[0].subject));
        } finally {
          try {
            await warmup.stop();
          } finally {
            await warmupExit;
          }
        }
      } finally {
        hints.unsubscribe();
        await hintPump;
      }
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        name: "own-watch-provider",
        seed: instance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      let hold: ResponseHoldBarrier | undefined;
      try {
        const records = service.kv.records;
        assert(records);
        assertEquals(service.connection.availability().resources.extras, true);
        await records.put("own-recovery", { value: "baseline" }).orThrow();
        const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
          items: { participantId: string; contextDigest: string }[];
        };
        const own = page.items.filter((item) =>
          item.participantId === contract.identity
        );
        assertEquals(own.length, 1);
        const digest = own[0].contextDigest;
        const sockets = admittedConnections(
          await completeBrokerInventory(system),
          new Set([digest]),
        );
        assertEquals(sockets.length, 1);
        const brokerIdentity = brokerConnectionKey(sockets[0]);
        const proxy = gate.connections().find((connection) =>
          connection.outboundContexts.some((pub) =>
            pub.subject.startsWith("$KV.") &&
            pub.subject.endsWith(".own-recovery")
          )
        );
        assert(proxy);
        const kvSubject = proxy.outboundContexts.find((pub) =>
          pub.subject.startsWith("$KV.") &&
          pub.subject.endsWith(".own-recovery")
        )!.subject;
        const stream = await jsm.streams.find(kvSubject);
        const baseline = await jsm.streams.getMessage(stream, {
          last_by_subj: kvSubject,
        });
        assert(baseline);
        const consumers: ConsumerInfo[] = [];
        for (const info of await jsm.streams.list().next()) {
          consumers.push(...await jsm.consumers.list(info.config.name).next());
        }
        const watches = consumers.filter((info) =>
          info.push_bound && info.stream_name.startsWith("KV_") &&
          info.config.filter_subject ===
            `$KV.${info.stream_name.slice(3)}.revocation.${digest}` &&
          proxy.subs.some((sub) =>
            sub.subject === info.config.deliver_subject
          )
        );
        assertEquals(
          watches.length,
          1,
          "full own filter and actual native deliver inbox identify exactly one watch",
        );
        const old = watches[0];
        assert(old.push_bound);
        assertEquals(old.config.idle_heartbeat, 5_000_000_000);
        const physicalConnections = gate.connections().length;
        let replacement: ConsumerInfo | undefined;
        hold = gate.armResponseHold(
          "$JS.API.CONSUMER.INFO.",
          proxy.id,
          (body) => {
            const info = JSON.parse(
              new TextDecoder().decode(body),
            ) as ConsumerInfo;
            if (
              info.push_bound && info.name !== old.name &&
              info.config.filter_subject === old.config.filter_subject &&
              service.connection.availability().resources.extras === false
            ) {
              // Bootstrap hints may prepare a same-digest replacement while the
              // predecessor is healthy. Hold recovery only after coverage loss,
              // not that unrelated preparation whose INFO could already expire.
              replacement = info;
              return true;
            }
            return false;
          },
        );
        const held = hold.held;
        void held.catch(() => undefined);
        assert(await jsm.consumers.delete(old.stream_name, old.name));
        await runtime.waitFor(
          () => service.connection.availability().resources.extras === false,
          { timeoutMs: 20_000 },
        );
        await bounded(
          held,
          10_000,
          "own replacement INFO must be reached without fresh acquisition",
        );
        assert(replacement);
        assertEquals(
          replacement.config.idle_heartbeat,
          old.config.idle_heartbeat,
        );
        assert(!gate.connection(proxy.id)?.closed);
        assertEquals(
          gate.connections().length,
          physicalConnections,
          "coverage repair must not open another socket",
        );
        // The final exact broker identity proves this socket survived throughout.
        // Do not spend the held INFO's request budget on a remote inventory here.
        const publishes =
          gate.connection(proxy.id)!.outboundContexts.filter((pub) =>
            pub.subject === kvSubject
          ).length;
        let outcome: "success" | "error" | undefined;
        const write = records.put("own-recovery", { value: "recovered" })
          .orThrow().then(
            (value) => {
              outcome = "success";
              return value;
            },
            (error: unknown) => {
              outcome = "error";
              throw error;
            },
          );
        void write.catch(() => undefined);
        const checkingUntil = performance.now() + 400;
        await runtime.waitFor(async () => {
          assertEquals(
            service.connection.availability().resources.extras,
            false,
          );
          assertEquals(
            outcome,
            undefined,
            "fresh IO must suspend, not fail immediately or succeed",
          );
          assertEquals(
            gate.connection(proxy.id)!.outboundContexts.filter((pub) =>
              pub.subject === kvSubject
            ).length,
            publishes,
            "no application KV PUB while own coverage is unavailable",
          );
          const persisted = await jsm.streams.getMessage(stream, {
            last_by_subj: kvSubject,
          });
          assert(persisted);
          assertEquals(persisted.seq, baseline.seq);
          assertEquals(persisted.data, baseline.data);
          return performance.now() >= checkingUntil;
        }, { timeoutMs: 1_500, intervalMs: 10 });
        await hold.release();
        await bounded(
          write,
          5_000,
          "fresh waiter must wake after same-publication coverage initializes",
        );
        await runtime.waitFor(() =>
          service.connection.availability().resources.extras === true
        );
        assertEquals(await records.get("own-recovery").orThrow(), {
          value: "recovered",
        });
        const persisted = await jsm.streams.getMessage(stream, {
          last_by_subj: kvSubject,
        });
        assert(persisted);
        assert(persisted.seq > baseline.seq);
        const restored = await jsm.consumers.info(
          replacement.stream_name,
          replacement.name,
        );
        assert(restored.push_bound);
        assert(
          gate.connection(proxy.id)!.subs.some((sub) =>
            sub.subject === restored.config.deliver_subject
          ),
        );
        assert(!gate.connection(proxy.id)?.closed);
        assertEquals(gate.connections().length, physicalConnections);
        assertEquals(
          admittedConnections(
            await completeBrokerInventory(system),
            new Set([digest]),
          ).map(brokerConnectionKey),
          [brokerIdentity],
        );
        assertEquals(
          gate.connections().filter((connection) =>
            connection.outboundContexts.some((pub) => pub.subject === kvSubject)
          ).map((connection) => connection.id),
          [proxy.id],
        );
      } finally {
        try {
          await hold?.release();
        } finally {
          try {
            await service.stop();
          } finally {
            await serviceExit;
          }
        }
      }
    } finally {
      try {
        await privileged.close();
      } finally {
        await system.close();
      }
    }
  }, {
    interruptibleNativeProxy: true,
    authorization: {
      contextLifetimeSeconds: 300,
      minimumContextLifetimeSeconds: 240,
      refreshLeadSeconds: 20,
      refreshJitterSeconds: 0,
    },
  });
});
