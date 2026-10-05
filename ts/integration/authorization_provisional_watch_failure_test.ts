/** A failed replacement watch must not withdraw a healthy predecessor. */
import { assert, assertEquals, assertRejects } from "@std/assert";
import { join } from "@std/path";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { headers } from "@nats-io/nats-core";
import { type ConsumerInfo, jetstream } from "@nats-io/jetstream";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type { ResponseHoldBarrier } from "../packages/trellis-testkit/src/native_gate.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";
import { AuthorizationRegistryReader } from "../packages/trellis/auth/authorization/nats_registry.ts";

async function bounded<T>(promise: Promise<T>, ms: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("boundary wait timed out")),
          ms,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

Deno.test("failed provisional peer coverage preserves work and real revocation enforcement", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.TransportGrowthSubject.participant;
    const targetContract = participants.TransportGrowthTarget.participant;
    await runtime.contracts.apply({
      deployment: "failure-target",
      contract: targetContract,
    });
    const targetInstance = await runtime.services.createInstance({
      deployment: "failure-target",
      name: "failure-target",
      contract: targetContract,
    });
    const target = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: targetContract,
      name: "failure-target",
      seed: targetInstance.seed,
    }).orThrow();
    const targetExit = target.wait().catch(() => undefined);
    await target.handleAdvance(() => Result.ok({}));
    await target.handleExtend(() => Result.ok({}));
    await runtime.contracts.install({ contract });
    const proposal = await runtime.contracts.requestApply({
      deployment: "failure-subject",
      contract,
    });
    assert(proposal.status === "approval_required");
    await runtime.contracts.approveApply(proposal.pendingId, {
      excludeCapabilities: ["runtime-trellis.transport_growth@v1::extend"],
    });
    const instance = await runtime.services.createInstance({
      deployment: "failure-subject",
      name: "failure-subject",
      contract,
    });
    const subject = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "failure-subject",
      seed: instance.seed,
    }).orThrow();
    const subjectExit = subject.wait().catch(() => undefined);
    let inspections = 0;
    await subject.handleInspect(() =>
      Result.ok({
        starts: BigInt(++inspections),
        cleanups: 0n,
        active: 1n,
        emitted: 0n,
      })
    );
    let stopped = false;
    let peerDigest: string | undefined;
    const wakes = new Set<() => void>();
    await subject.handleWatch(async ({ emit, signal, caller }) => {
      assert(caller.type === "verified");
      peerDigest = caller.contextDigest;
      let index = 0n;
      const abort = () => {
        stopped = true;
        for (const wake of wakes) wake();
      };
      signal.addEventListener("abort", abort, { once: true });
      try {
        while (!signal.aborted) {
          await emit({
            runId: "failure",
            streamId: "failure",
            sourceGeneration: 1n,
            index: ++index,
            payload: new Uint8Array(),
            padding: "",
          });
          await new Promise<void>((resolve) => {
            const wake = () => {
              clearTimeout(timer);
              wakes.delete(wake);
              resolve();
            };
            const timer = setTimeout(wake, 100);
            wakes.add(wake);
          });
        }
      } finally {
        signal.removeEventListener("abort", abort);
      }
    });
    const caller = await runtime.connectClient({
      name: "failure-caller",
      contract: participants.LiveProbeCaller.participant,
    });
    const privileged = await connect({
      servers: runtime.nativeProxyUrl(),
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "config/trellis/nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    const js = jetstream(privileged);
    const jsm = await js.jetstreamManager();
    const system = await connect({
      servers: runtime.nativeProxyUrl(),
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
        ),
      ),
    });
    const gate = runtime.nativeTransportGate();
    let hold: ResponseHoldBarrier | undefined;
    let feedTask: Promise<void> | undefined;
    try {
      const feed = await caller.watch({ runId: "failure", streamId: "failure" })
        .orThrow();
      let frames = 0;
      feedTask = (async () => {
        for await (const _ of feed) frames++;
      })().catch(() => undefined);
      await runtime.waitFor(() => frames > 0);
      const callerSocket = gate.connections().find((socket) =>
        !socket.closed &&
        socket.outboundContexts.some((request) =>
          request.subject.endsWith(".Watch")
        )
      );
      assert(
        callerSocket,
        "the real observation caller socket must be identified",
      );
      assert(peerDigest);
      const digest = peerDigest;
      const consumers: ConsumerInfo[] = [];
      for (const stream of await jsm.streams.list().next()) {
        consumers.push(...await jsm.consumers.list(stream.config.name).next());
      }
      const old = consumers.find((info) =>
        info.config.filter_subject?.endsWith(`.revocation.${digest}`) &&
        gate.connections().some((socket) =>
          !socket.closed &&
          socket.subs.some((sub) => sub.subject.endsWith(".Inspect")) &&
          socket.subs.some((sub) => sub.subject === info.config.deliver_subject)
        )
      );
      assert(old, "the served peer digest must have an actual provider watch");
      const oldSocket = gate.connections().find((socket) =>
        !socket.closed &&
        socket.subs.some((sub) => sub.subject.endsWith(".Inspect")) &&
        socket.subs.some((sub) => sub.subject === old.config.deliver_subject)
      );
      assert(oldSocket);
      const grants = await runtime.callAdminRpc("authGrantsList", {
        participantId: contract.identity,
      }) as {
        items: {
          revision: bigint;
          installedRevision: bigint;
          expiresAt: bigint | null;
          ownerId: string;
          ownerKind: string;
          participantId: string;
          platformPrivileges: string[];
          grants: {
            format: string;
            permissions: { action: string; target: Uint8Array }[];
          };
        }[];
      };
      const binding = grants.items.find((item) =>
        item.participantId === contract.identity
      );
      assert(binding);
      gate.arm(".Inspect");
      await runtime.callAdminRpc("authGrantsSet", {
        expiresAt: binding.expiresAt,
        installedRevision: binding.installedRevision,
        ownerId: binding.ownerId,
        ownerKind: binding.ownerKind,
        participantId: binding.participantId,
        platformPrivileges: binding.platformPrivileges,
        expectedRevision: binding.revision,
        idempotencyKey: crypto.randomUUID(),
        grants: {
          format: binding.grants.format,
          permissions: [...binding.grants.permissions, {
            action: "call",
            target: await encodePermissionTargetWasm({
              kind: "apiSurface",
              api: "runtime-trellis.transport_growth@v1",
              surface: "rpc",
              name: "Extend",
            }),
          }],
        },
      });
      const nextSocket = await bounded(gate.barrierHeld(), 30_000);
      let replacement: ConsumerInfo | undefined;
      const matches = (body: Uint8Array) => {
        const info = JSON.parse(new TextDecoder().decode(body)) as ConsumerInfo;
        if (
          info.push_bound &&
          info.config.filter_subject === old.config.filter_subject
        ) {
          replacement = info;
          return true;
        }
        return false;
      };
      hold = gate.armResponseHold(
        "$JS.API.CONSUMER.INFO.",
        nextSocket,
        matches,
      );
      await gate.release();
      await bounded(hold.held, 10_000);
      assert(replacement);
      const failed = replacement;
      assert(
        gate.connection(nextSocket)?.subs.some((sub) =>
          sub.subject === failed.config.deliver_subject
        ),
      );
      assert(await jsm.consumers.delete(failed.stream_name, failed.name));
      await assertRejects(() =>
        jsm.consumers.info(failed.stream_name, failed.name)
      );
      // The native gate records SUB history, not UNSUB. Read the broker's
      // complete live subscription inventory to observe the actual SDK teardown.
      const teardownStarted = Date.now();
      await runtime.waitFor(async () => {
        const reply = await system.request(
          `$SYS.REQ.SERVER.${system.info!.server_id}.CONNZ`,
          JSON.stringify({ subscriptions: true, limit: 1024, offset: 0 }),
        );
        const inventory = reply.json<{
          server: { id: string };
          error?: unknown;
          data: {
            server_id: string;
            offset: number;
            limit: number;
            total: number;
            num_connections: number;
            connections: {
              subscriptions: number;
              subscriptions_list?: string[];
            }[];
          };
        }>();
        assertEquals(inventory.error, undefined);
        assertEquals(inventory.server.id, system.info!.server_id);
        assertEquals(inventory.data.server_id, system.info!.server_id);
        assertEquals(inventory.data.offset, 0);
        assertEquals(inventory.data.limit, 1024);
        assert(
          Number.isSafeInteger(inventory.data.total) &&
            inventory.data.total > 0,
        );
        assertEquals(inventory.data.num_connections, inventory.data.total);
        assertEquals(inventory.data.connections.length, inventory.data.total);
        assert(
          inventory.data.connections.every((connection) =>
            (connection.subscriptions === 0 &&
              connection.subscriptions_list === undefined) ||
            (Array.isArray(connection.subscriptions_list) &&
              connection.subscriptions_list.length === connection.subscriptions)
          ),
        );
        return !inventory.data.connections.some((connection) =>
          connection.subscriptions_list?.includes(
            failed.config.deliver_subject!,
          )
        );
      }, { timeoutMs: 10_000, intervalMs: 10 });
      console.log(
        `SDK replacement subscription teardown observed ${
          Date.now() - teardownStarted
        }ms after consumer deletion; original INFO remains unreleased`,
      );
      // INFO remains unreleased. Check the predecessor immediately at the
      // observed teardown boundary, before any successful retry can migrate it.
      assert((await jsm.consumers.info(old.stream_name, old.name)).push_bound);
      assert(!gate.connection(oldSocket.id)?.closed);
      assert(!gate.connection(nextSocket)?.closed);
      assert(!gate.connection(callerSocket.id)?.closed);
      assert(
        gate.connection(oldSocket.id)?.subs.some((sub) =>
          sub.subject === old.config.deliver_subject
        ),
      );
      const before = frames;
      assertEquals(
        (await caller.inspect({ runId: "failure", streamId: "failure" })
          .orThrow()).starts,
        1n,
      );
      await runtime.waitFor(() => frames > before);
      assert(
        !stopped,
        "failed replacement must not stop the authorized observation",
      );
      // Real persisted revocation still reaches the authoritative predecessor
      // after failed setup teardown, terminating the observation. A later
      // successful retry may migrate normally; no old-binding claim follows it.
      await js.publish(
        old.config.filter_subject!,
        new TextEncoder().encode(
          JSON.stringify({ revokedAt: Math.floor(Date.now() / 1000) }),
        ),
      );
      await runtime.waitFor(() => stopped, { timeoutMs: 10_000 });
      await bounded(feedTask, 10_000);

      // A real recorded operation error must fence coverage immediately without
      // hiding the genuine revocation already queued ahead of that error.
      const evidenceDigest = btoa(
        String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32))),
      )
        .replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
      const reader = await AuthorizationRegistryReader.open(privileged, {
        contextBucket: old.stream_name.slice(3),
      }, "_INBOX.failure-evidence");
      const evidence = await reader.watchRevocation(evidenceDigest);
      try {
        assertEquals(
          (await evidence.iterator.next()).value?.operation,
          "initialized",
        );
        const evidenceSubject = `$KV.${
          old.stream_name.slice(3)
        }.revocation.${evidenceDigest}`;
        await js.publish(
          evidenceSubject,
          new TextEncoder().encode(JSON.stringify({ revokedAt: 123 })),
        );
        const invalid = headers();
        invalid.set("KV-Operation", "INVALID");
        await js.publish(evidenceSubject, new Uint8Array(), {
          headers: invalid,
        });
        await runtime.waitFor(() => !evidence.usable());
        const recorded = await evidence.iterator.next();
        assertEquals(recorded.value?.operation, "put");
        assert(recorded.value?.operation === "put");
        assertEquals(
          JSON.parse(new TextDecoder().decode(recorded.value.value)).revokedAt,
          123,
        );
        await assertRejects(
          () => evidence.iterator.next(),
          Error,
          "operation is invalid",
        );
      } finally {
        await evidence.close();
      }
    } finally {
      await gate.release().catch(() => undefined);
      await hold?.release();
      for (const wake of wakes) wake();
      await caller.connection.close();
      await subject.stop();
      await subjectExit;
      await target.stop();
      await targetExit;
      await privileged.close();
      await system.close();
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
