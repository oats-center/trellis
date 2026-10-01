/** Retained Rust peer coverage must follow G3 while G2 preparation is pending. */
import { assert, assertEquals } from "@std/assert";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { TrellisClient } from "@oatscenter/trellis";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

type ConsumerInfo = {
  name: string;
  stream_name: string;
  created: string;
  push_bound: boolean;
  config: { filter_subject: string; deliver_subject: string };
};

Deno.test("Rust retained peer coverage supersedes pending G2 setup before its INFO timeout", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.TransportGrowthOperationProvider.participant;
    const deployment = "coverage-supersession-rust";
    const extend = "runtime-trellis.transport_growth@v1::extend";
    const extend2 = "runtime-trellis.transport_growth@v1::extend2";
    await runtime.contracts.install({ contract });
    const initial = await runtime.contracts.requestApply({
      deployment,
      contract,
    });
    assert(initial.status === "approval_required");
    await runtime.contracts.approveApply(initial.pendingId, {
      excludeCapabilities: [extend, extend2],
    });
    const instance = await runtime.services.createInstance({
      deployment,
      name: deployment,
      contract,
    });
    const system = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/system.creds`),
      ),
    });
    const privileged = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/trellis-auth.creds`),
      ),
    });
    const manager = await jetstreamManager(privileged);
    const bindingHints: string[] = [];
    const hints = privileged.subscribe("_INBOX.*._trellis.authorization", {
      callback: (error, message) => {
        if (error) throw error;
        if (
          message.json<{ format: string }>().format ===
            "trellis.authorization-change.v1"
        ) bindingHints.push(message.subject);
      },
    });
    await privileged.flush();
    const gate = runtime.nativeTransportGate();
    const child = new Deno.Command("setsid", {
      args: rustFixtureArgv("transport_growth_operation"),
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: instance.seed,
      },
      stdin: "piped",
      stdout: "piped",
      stderr: "inherit",
    }).spawn();
    const stdin = child.stdin.getWriter();
    let exited = false;
    const status = child.status.then((value) => {
      exited = true;
      return value;
    });
    const lines: string[] = [];
    const output = (async () => {
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
    const send = (command: string) =>
      stdin.write(new TextEncoder().encode(`${command}\n`));
    const marker = (expected: string) =>
      runtime.waitFor(() => {
        const error = lines.find((line) => line.includes("_ERROR "));
        if (error) throw new Error(error);
        if (lines.includes(expected)) return true;
        if (exited) throw new Error(`fixture exited: ${lines.join("\n")}`);
        return undefined;
      }, { timeoutMs: 30_000, intervalMs: 10 });
    const attachments = async () => {
      const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
        items: { participantId: string; contextDigest: string }[];
      };
      return page.items.filter((item) =>
        item.participantId === contract.identity
      );
    };
    let caller:
      | Awaited<
        ReturnType<
          typeof runtime.connectClient<
            typeof participants.TransportGrowthCaller.participant
          >
        >
      >
      | undefined;
    let hold: ReturnType<typeof gate.armResponseHold> | undefined;
    let park: Promise<unknown> | undefined;
    try {
      await marker("OPERATION_PROVIDER_READY");
      await marker("JOB_STARTED 1 initial");
      // Complete the fixture's real default job before populating peer coverage:
      // its accepted execution must not supply an unrelated legitimate G1 pin.
      await send("JOB_RELEASE initial");
      await marker("JOB_DONE 1 initial");
      await send("JOB_WAIT initial");
      await marker("JOB_WAIT initial Completed 1 held-1");
      const receiving = gate.connections().filter((connection) =>
        connection.subs.some((sub) => sub.subject.endsWith(".Advance"))
      );
      assertEquals(receiving.length, 1);
      const g1 = receiving[0].id;
      const advanceRoutes = receiving[0].subs.filter((sub) =>
        sub.subject.endsWith(".Advance")
      );
      assertEquals(advanceRoutes.length, 1);
      const advanceSubject = advanceRoutes[0].subject;
      const baseline = await attachments();
      assertEquals(baseline.length, 1);
      const original = admittedConnections(
        await completeBrokerInventory(system),
        new Set([baseline[0].contextDigest]),
      );
      assertEquals(original.length, 1);
      const g1Key = brokerConnectionKey(original[0]);
      const requiredServerIds = [original[0].server];

      // Issuance awaits the first provider-binding write, but its queued
      // reevaluation can still hint a refresh after connect completes.
      // Observe that ordinary publication before creating the proof caller:
      // reevaluate_transport snapshots its recipients before publishing hints,
      // so the later caller cannot receive that observed publication. Bootstrap
      // also installs the bound routes; no warmup RPC should add a second
      // retained peer to the provider coverage experiment.
      const beforeWarmup = new Set(gate.connections().map((item) => item.id));
      const proofKey = await runtime.registerClient({
        name: "coverage-supersession-caller",
        contract: participants.TransportGrowthCaller.participant,
      });
      caller = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        name: "coverage-supersession-warmup",
        participant: participants.TransportGrowthCaller.participant,
        ...runtime.clientAuth(proofKey),
        timeout: 90_000,
      }).orThrow();
      const warmup = gate.connections().filter((connection) =>
        !beforeWarmup.has(connection.id) &&
        connection.subs.some((sub) =>
          sub.subject.endsWith("._trellis.authorization")
        )
      );
      assertEquals(warmup.length, 1);
      const warmupHints = warmup[0].subs.filter((sub) =>
        sub.subject.endsWith("._trellis.authorization")
      );
      assertEquals(warmupHints.length, 1);
      await runtime.waitFor(() =>
        bindingHints.includes(warmupHints[0].subject) ? true : undefined
      );
      await caller.connection.close();
      caller = undefined;
      hints.unsubscribe();

      // Settle slow growth preparation before the peer exists. Withhold G2's
      // initial broker admission reply: intake installation cannot start yet,
      // so the fresh baseline request below must still receive on current G1.
      const firstGrowth = await runtime.contracts.requestApply({
        deployment,
        contract,
      });
      assert(firstGrowth.status === "approval_required");
      hold = gate.armResponseHold("$SYS.REQ.USER.INFO");
      await runtime.contracts.approveApply(firstGrowth.pendingId, {
        excludeCapabilities: [extend2],
      });
      const firstRefreshStart = lines.length;
      await send("REFRESH");
      let admission: Awaited<typeof hold.held> | undefined;
      let admissionError: unknown;
      hold.held.then((value) => {
        admission = value;
      }, (cause) => {
        admissionError = cause;
      });
      await runtime.waitFor(() => {
        if (admissionError) {
          throw admissionError;
        }
        return admission;
      }, { timeoutMs: 45_000, intervalMs: 10 });
      assert(admission);
      const g2 = admission.connectionId;
      assert(g2 !== g1);
      assert(
        !gate.connection(g2)!.subs.some((sub) =>
          sub.subject === advanceSubject
        ),
        "G2 initial admission hold must precede Advance intake installation",
      );
      await runtime.waitFor(() => {
        const fresh = lines.slice(firstRefreshStart);
        const error = fresh.find((line) => line.includes("_ERROR "));
        if (error) throw new Error(error);
        if (exited) throw new Error(`fixture exited: ${lines.join("\n")}`);
        return fresh.includes("AUTH_REFRESHED") ? true : undefined;
      }, { timeoutMs: 30_000, intervalMs: 10 });
      // Registration is already complete. Connect without repeating participant
      // installation inside the same-digest coverage experiment.
      caller = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        name: "coverage-supersession-caller",
        participant: participants.TransportGrowthCaller.participant,
        ...runtime.clientAuth(proofKey),
        timeout: 90_000,
      }).orThrow();
      await caller.advance({}).orThrow();
      const requests = gate.connections().flatMap((connection) =>
        connection.outboundContexts.filter((out) =>
          out.subject.endsWith(".Advance") && out.context !== undefined
        )
      );
      assertEquals(requests.length, 1);
      assertEquals(requests[0].subject, advanceSubject);
      const received = gate.connections().filter((connection) =>
        connection.deliveries.some((delivery) =>
          delivery.subject === advanceSubject
        )
      );
      assertEquals(received.length, 1);
      assertEquals(received[0].id, g1);
      const parkRoutes = gate.connection(g1)!.subs.filter((sub) =>
        sub.subject.endsWith(".Park")
      );
      assertEquals(parkRoutes.length, 1);
      const parkSubject = parkRoutes[0].subject;
      const baselinePeerDigest = requests[0].context;
      assert(
        baselinePeerDigest,
        "retain the digest on the actual signed inbound Advance",
      );
      // Discover the exact already-bound baseline revocation consumer and bucket.
      const streams = [];
      for await (const stream of manager.streams.list()) {
        streams.push(stream);
      }
      const watches = (await Promise.all(streams.map(async (stream) => {
        const matches = [];
        for await (
          const consumer of manager.consumers.list(stream.config.name)
        ) {
          if (
            consumer.push_bound && consumer.config.filter_subject?.endsWith(
              `.revocation.${baselinePeerDigest}`,
            ) && gate.connection(g1)!.subs.some((sub) =>
              sub.subject === consumer.config.deliver_subject
            )
          ) {
            matches.push(consumer);
          }
        }
        return matches;
      }))).flat();
      assertEquals(watches.length, 1);
      const oldWatch = watches[0];
      const filter = oldWatch.config.filter_subject!;
      assertEquals(filter.split(".")[0], "$KV");
      assert(
        gate.connection(g1)!.subs.some((sub) =>
          sub.subject === oldWatch.config.deliver_subject
        ),
        "the authoritative baseline peer watch belongs to the G1 receiving socket",
      );

      // Disarm synchronously and rearm before forwarding can trigger G2's later
      // post-SUB readiness round trip. This is the same physical G2, not the
      // initial admission probe or another candidate's readiness.
      const admitting = hold.release();
      hold = gate.armResponseHold(
        "$SYS.REQ.USER.INFO",
        g2,
        () =>
          gate.connection(g2)!.subs.some((sub) =>
            sub.subject === advanceSubject
          ),
      );
      let readiness: Awaited<typeof hold.held> | undefined;
      let readinessError: unknown;
      hold.held.then((value) => {
        readiness = value;
      }, (cause) => {
        readinessError = cause;
      });
      await admitting;
      await runtime.waitFor(() => {
        if (readinessError) throw readinessError;
        return readiness;
      }, { timeoutMs: 45_000, intervalMs: 10 });
      assert(readiness);
      assertEquals(readiness.connectionId, g2);
      assert(
        gate.connection(g2)!.subs.some((sub) => sub.subject === advanceSubject),
      );
      const g2Admitted = await attachments();
      const g2Digest = g2Admitted.find((item) =>
        item.contextDigest !== baseline[0].contextDigest
      )?.contextDigest;
      assert(g2Digest);
      const g2Sockets = admittedConnections(
        await completeBrokerInventory(system, { requiredServerIds }),
        new Set([g2Digest]),
      );
      assertEquals(g2Sockets.length, 1);
      const g2Key = brokerConnectionKey(g2Sockets[0]);
      if (!requiredServerIds.includes(g2Sockets[0].server)) {
        requiredServerIds.push(g2Sockets[0].server);
      }
      let heldInfo: ConsumerInfo | undefined;
      const infoReplies: Pick<
        ConsumerInfo,
        "name" | "created" | "push_bound" | "config"
      >[] = [];
      const publishing = hold.release();
      hold = gate.armResponseHold("$JS.API.CONSUMER.INFO.", g2, (body) => {
        const info = JSON.parse(new TextDecoder().decode(body)) as ConsumerInfo;
        infoReplies.push({
          name: info.name,
          created: info.created,
          push_bound: info.push_bound,
          config: info.config,
        });
        // Match the exact subscribed replacement, not push_bound: the real
        // post-SUB, post-flush INFO can still omit that flag.
        // Holding this reply must prevent the SDK's initialization barrier.
        if (
          info.config?.filter_subject !== filter ||
          info.name === oldWatch.name ||
          !gate.connection(g2)!.subs.some((sub) =>
            sub.subject === info.config.deliver_subject
          )
        ) return false;
        heldInfo = info;
        return true;
      });
      let held: Awaited<typeof hold.held> | undefined;
      let holdError: unknown;
      hold.held.then((value) => {
        held = value;
      }, (cause) => {
        holdError = cause;
      });
      await publishing;
      try {
        await runtime.waitFor(() => {
          if (holdError) throw holdError;
          return held;
        }, { timeoutMs: 10_000, intervalMs: 10 });
      } catch (cause) {
        throw new Error(
          `replacement INFO hold failed: ${
            JSON.stringify({
              g1,
              g2,
              filter,
              oldWatch,
              infoReplies,
              connection: gate.connection(g2),
              fixture: lines,
            })
          }`,
          { cause },
        );
      }
      assert(held && heldInfo);
      assertEquals(held.connectionId, g2);
      // Consumer creation predates its INFO request. This conservative deadline
      // cannot mistakenly count a timed-out async-nats request as still pending.
      // async-nats 0.50's unchanged default request timeout is ten seconds.
      const proofDeadline = Date.parse(heldInfo.created) + 10_000 - 250;
      assert(Number.isFinite(proofDeadline));
      const beforeDeadline = () => {
        assert(
          Date.now() < proofDeadline,
          "supersession proof must finish before the held INFO can time out",
        );
        assertEquals(
          gate.isClosed(g2!),
          false,
          "G2 must stay physically open with INFO and accepted Park outstanding",
        );
      };
      beforeDeadline();
      const heldInventory = await completeBrokerInventory(system, {
        requiredServerIds,
      });
      beforeDeadline();
      assert(
        heldInventory.some((item) => brokerConnectionKey(item) === g1Key),
        "G1 must still be broker-admitted while G2 peer setup is withheld",
      );
      const authoritative = await manager.consumers.info(
        oldWatch.stream_name,
        oldWatch.name,
      );
      beforeDeadline();
      assert(
        authoritative.push_bound &&
          authoritative.created === oldWatch.created &&
          authoritative.config.filter_subject === filter &&
          authoritative.config.deliver_subject ===
            oldWatch.config.deliver_subject,
        "the exact original authoritative peer consumer must still be bound",
      );

      // Accepted G2 callback is the deliberate G2 pin, not a G1 execution.
      // Forwarding readiness is not proof of publication: first complete a real
      // public call received on G2, then park exactly one callback there.
      await runtime.waitFor(async () => {
        beforeDeadline();
        const previous = gate.connection(g2!)!.deliveries.length;
        await caller!.advance({}, {
          timeout: Math.max(1, proofDeadline - Date.now()),
        }).orThrow();
        beforeDeadline();
        return gate.connection(g2!)!.deliveries.slice(previous).some((
            delivery,
          ) => delivery.subject === advanceSubject
          )
          ? true
          : undefined;
      }, {
        timeoutMs: Math.max(1, proofDeadline - Date.now()),
        intervalMs: 10,
      });
      let parkSettled = false;
      let parkError: unknown;
      const parked = caller.park({}, { timeout: 90_000 }).orThrow();
      park = parked;
      parked.then(() => {
        parkSettled = true;
      }, (cause) => {
        parkSettled = true;
        parkError = cause;
      });
      await runtime.waitFor(async () => {
        beforeDeadline();
        if (parkError) throw parkError;
        await send("PARKS");
        return lines.includes("PARK_CALLS 1") ? true : undefined;
      }, {
        timeoutMs: Math.max(1, proofDeadline - Date.now()),
        intervalMs: 10,
      });
      assertEquals(
        gate.connections().flatMap((connection) =>
          connection.deliveries.filter((delivery) =>
            delivery.subject === parkSubject
          )
            .map(() => connection.id)
        ),
        [g2],
        "accepted Park must receive exactly once on G2",
      );
      const parkContexts = gate.connections().flatMap((connection) =>
        connection.outboundContexts.filter((out) => out.subject === parkSubject)
          .map((out) => out.context)
      );
      assertEquals(
        parkContexts,
        [baselinePeerDigest],
        "Park must use the same retained peer cache entry as baseline Advance",
      );
      assertEquals(parkSettled, false);

      console.log(JSON.stringify({
        event: "rust-coverage-g2-peer-info-held",
        g1Key,
        g2Key,
        g1,
        g2,
        g2Digest,
        baselinePeerDigest,
        heldInfo: held.requestSubject,
        consumerCreated: heldInfo.created,
        at: Date.now(),
        proofDeadline,
        parkSettled,
      }));
      const secondGrowth = await runtime.contracts.requestApply({
        deployment,
        contract,
      });
      assert(secondGrowth.status === "approval_required");
      await runtime.contracts.approveApply(secondGrowth.pendingId);
      // Refresh authorization through the public client; transport adoption and
      // retained-peer supersession remain automatic production behavior.
      const secondRefreshStart = lines.length;
      await send("REFRESH");
      await runtime.waitFor(() => {
        beforeDeadline();
        const error = lines.find((line) => line.includes("_ERROR "));
        if (error) throw new Error(error);
        if (exited) throw new Error(`fixture exited: ${lines.join("\n")}`);
        return lines.slice(secondRefreshStart).includes("AUTH_REFRESHED")
          ? true
          : undefined;
      }, {
        timeoutMs: Math.max(1, proofDeadline - Date.now()),
        intervalMs: 10,
      });
      let g3: number | undefined;
      await runtime.waitFor(() => {
        beforeDeadline();
        g3 = gate.connections().find((connection) =>
          connection.id > g2! &&
          connection.subs.some((sub) => sub.subject === advanceSubject)
        )?.id;
        return g3;
      }, {
        timeoutMs: Math.max(1, proofDeadline - Date.now()),
        intervalMs: 10,
      });
      assert(g3 !== undefined && g3 !== g2 && g3 !== g1);
      const g3Digest = (await attachments()).find((item) =>
        item.contextDigest !== baseline[0].contextDigest &&
        item.contextDigest !== g2Digest
      )?.contextDigest;
      assert(g3Digest, "G3 readiness must belong to a third admitted context");
      beforeDeadline();
      await runtime.waitFor(async () => {
        beforeDeadline();
        const previous = gate.connection(g3!)!.deliveries.length;
        await caller!.advance({}, {
          timeout: Math.max(1, proofDeadline - Date.now()),
        }).orThrow();
        beforeDeadline();
        return gate.connection(g3!)!.deliveries.slice(previous).some((
            delivery,
          ) => delivery.subject === advanceSubject
          )
          ? true
          : undefined;
      }, {
        timeoutMs: Math.max(1, proofDeadline - Date.now()),
        intervalMs: 10,
      });
      beforeDeadline();
      assertEquals(parkSettled, false);

      // The decisive proof is canonical broker absence, not proxy closure or
      // missing admission bookkeeping. Every poll includes G1's exact server.
      await runtime.waitFor(async () => {
        beforeDeadline();
        const inventory = await completeBrokerInventory(system, {
          requiredServerIds,
        });
        beforeDeadline();
        assertEquals(
          admittedConnections(inventory, new Set([g3Digest])).length,
          1,
          "fresh G3 work must have its own real broker attachment",
        );
        assert(
          gate.connections().flatMap((connection) =>
            connection.outboundContexts.filter((out) =>
              out.subject === advanceSubject
            )
          ).every((out) => out.context === baselinePeerDigest),
          "fresh requests must retain baseline peer coverage, not renew away from it",
        );
        assertEquals(
          parkSettled,
          false,
          "G2 Park must still be held at G1 reap",
        );
        assert(inventory.some((item) => brokerConnectionKey(item) === g2Key));
        return inventory.every((item) => brokerConnectionKey(item) !== g1Key)
          ? true
          : undefined;
      }, {
        timeoutMs: Math.max(1, proofDeadline - Date.now()),
        intervalMs: 10,
      });
      console.log(JSON.stringify({
        event: "rust-coverage-superseded-before-info-timeout",
        g1Key,
        g2Key,
        g1,
        g2,
        g3,
        baselinePeerDigest,
        heldInfo: held.requestSubject,
        consumerCreated: heldInfo.created,
        proofAt: Date.now(),
        proofDeadline,
        parkSettled,
      }));

      // Release only after the pre-timeout physical proof. Pending preparation
      // cleanup must not undo newer coverage or relocate the accepted callback.
      await hold.release();
      hold = undefined;
      const staleConsumer = heldInfo;
      await runtime.waitFor(async () => {
        const consumers = [];
        for await (
          const consumer of manager.consumers.list(staleConsumer.stream_name)
        ) {
          consumers.push(consumer);
        }
        return consumers.every((consumer) =>
            consumer.name !== staleConsumer.name ||
            consumer.created !== staleConsumer.created
          )
          ? true
          : undefined;
      }, { timeoutMs: 10_000, intervalMs: 25 });
      await send("RELEASE");
      assertEquals((await parked).value, "parked-1");
      const countStart = lines.length;
      await send("PARKS");
      await runtime.waitFor(() =>
        lines.slice(countStart).includes("PARK_CALLS 1") ? true : undefined
      );
      await runtime.waitFor(async () => {
        const inventory = await completeBrokerInventory(system, {
          requiredServerIds,
        });
        return inventory.every((item) => brokerConnectionKey(item) !== g2Key)
          ? true
          : undefined;
      }, { timeoutMs: 30_000, intervalMs: 50 });
      const freshBefore = gate.connection(g3)!.deliveries.length;
      await caller.advance({}).orThrow();
      assertEquals(
        gate.connection(g3)!.deliveries.slice(freshBefore).filter((delivery) =>
          delivery.subject === advanceSubject
        ).length,
        1,
        "G3 coverage must survive stale preparation cleanup and G2 reap",
      );
      const final = await completeBrokerInventory(system, {
        requiredServerIds,
      });
      assert(
        final.every((item) =>
          ![g1Key, g2Key].includes(brokerConnectionKey(item))
        ),
      );
      const current = await attachments();
      assertEquals(
        admittedConnections(
          final,
          new Set(current.map((item) => item.contextDigest)),
        ).length,
        1,
      );
      await send("EXIT");
      await marker("OPERATION_PROVIDER_DONE");
      await runtime.waitFor(() => exited ? true : undefined);
      assert((await status).success, lines.join("\n"));
    } finally {
      hints.unsubscribe();
      await gate.release().catch(() => undefined);
      await hold?.release().catch(() => undefined);
      if (!exited) await send("RELEASE").catch(() => undefined);
      await caller?.connection.close().catch(() => undefined);
      await stdin.close().catch(() => undefined);
      if (!exited) {
        try {
          Deno.kill(-child.pid, "SIGKILL");
        } catch { /* already exited */ }
      }
      await Promise.allSettled([status, output, ...(park ? [park] : [])]);
      await privileged.close();
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
