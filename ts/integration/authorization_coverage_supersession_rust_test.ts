/** Native authority supersession preserves an accepted Live observation. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

test("Rust preserves accepted Live during blocked preparation and publishes granted authority after replay", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const targetIdentity = await runtime.registerService({
        name: "native-growth-target",
        contract: participants.TransportGrowthTarget.participant,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.TransportGrowthTarget.participant,
        seed: targetIdentity.seed,
      }).orThrow();
      const targetExit = target.wait().catch((error: unknown) => error);
      await target.handleAdvance(() => Result.ok({}));
      let extensionCalls = 0;
      await target.handleExtend(() => {
        extensionCalls++;
        return Result.ok({});
      });
      const identity = await runtime.registerService({
        deployment: "native-supersession",
        name: "native-supersession",
        contract: participants.TransportGrowthSubject.participant,
      });
      const firstCapability =
        "runtime-trellis.transport_growth.v1:growth.extend";
      const firstGrants = await runtime.callAdminRpc(
        "authCapabilityGrantsList",
        { capabilityId: firstCapability },
      );
      assert.equal(firstGrants.items.length, 1);
      const first = firstGrants.items[0];
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: first.grantId,
        expectedRevision: first.revision,
        idempotencyKey: ulid(),
      });
      const argv = rustFixtureArgv("transport_growth_subject");
      const child = spawnTracked(argv[0], argv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
          TRELLIS_IDENTITY_SEED: identity.seed,
          RUST_LOG: "trellis_rs=info",
        },
      });
      let output = "";
      child.stdout.setEncoding("utf8");
      child.stdout.on("data", (chunk: string) => {
        output += chunk;
      });
      child.stderr.pipe(process.stderr);
      const caller = await runtime.connectClient({
        name: "native-supersession-observer",
        contract: apps.LiveProbeCaller.request,
      });
      const gate = runtime.nativeTransportGate();
      const abort = new AbortController();
      let hold: ReturnType<typeof gate.armResponseHold> | undefined;
      let framesDone: Promise<void> | undefined;
      try {
        await runtime.waitFor(() =>
          output.includes("TRANSPORT_GROWTH_ADVANCE_OK"),
        );
        const observation = await caller
          .watch(
            { runId: ulid(), streamId: "native-supersession" },
            { signal: abort.signal },
          )
          .orThrow();
        const before = (await deadline(observation.next(), 10_000)).value;
        assert(before);
        let latest = before;
        let streamFailure: unknown;
        framesDone = (async () => {
          for await (const frame of observation) latest = frame;
        })().catch((error: unknown) => {
          if (!abort.signal.aborted) streamFailure = error;
        });
        const route = apis.liveprobe.API.actions["live:Watch"].subject;
        const originals = gate
          .connections()
          .filter((connection) =>
            connection.deliveries.some((message) => message.subject === route),
          );
        assert.equal(originals.length, 1);
        const original = originals[0];
        const existing = new Set(
          gate.connections().map((connection) => connection.id),
        );
        hold = gate.armResponseHold(
          "$JS.API.CONSUMER.INFO.KV_TRELLIS_AUTH_REVOCATIONS.",
          undefined,
          (body) => {
            const info: { config?: { description?: string } } = JSON.parse(
              new TextDecoder().decode(body),
            );
            return (
              info.config?.description ===
              "Trellis logical-session retirement mirror"
            );
          },
        );
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: first.principalId,
          capabilityId: firstCapability,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        const candidate = await deadline(hold.held, 90_000);
        assert(!existing.has(candidate.connectionId));
        assert.notEqual(candidate.connectionId, original.id);
        const accepting = gate.connection(original.id);
        assert(accepting);
        const emittedBeforeHold = accepting.outboundContexts.filter((message) =>
          message.subject.startsWith("live.v1.data."),
        ).length;
        assert(emittedBeforeHold > 0);
        await runtime.waitFor(
          () => {
            if (streamFailure) throw streamFailure;
            return latest.index > BigInt(emittedBeforeHold);
          },
          { timeoutMs: 10_000 },
        );
        const during = latest;
        assert.equal(during.sourceGeneration, before.sourceGeneration);
        assert(during.index > before.index);
        assert.equal(
          extensionCalls,
          0,
          "private preparation must not publish the new RPC",
        );
        await hold.release();
        hold = undefined;
        child.stdin.write("WAIT_EXTEND\n");
        await runtime.waitFor(
          () => output.includes("TRANSPORT_GROWTH_EXTEND_OK"),
          { timeoutMs: 20_000 },
        );
        assert.equal(extensionCalls, 1);
        assert(
          gate
            .connection(candidate.connectionId)
            ?.outboundContexts.some(
              (message) =>
                message.subject ===
                apis.transport_growth.API.actions["rpc:Extend"].subject,
            ),
        );
        await runtime.waitFor(
          () => {
            if (streamFailure) throw streamFailure;
            return latest.index > during.index;
          },
          { timeoutMs: 10_000 },
        );
        const after = latest;
        assert.equal(after.sourceGeneration, before.sourceGeneration);
        assert(after.index > before.index);
        abort.abort();
        await deadline(gate.waitForClose(original.id), 15_000);
        assert.equal(gate.connection(candidate.connectionId)?.closed, false);
        child.stdin.end("EXIT\n");
        assert.equal((await deadline(child.exited, 10_000)).code, 0, output);
      } catch (cause) {
        const sessions = await runtime.callAdminRpc(
          "authAuthorizationSessionsList",
          {},
        );
        const own = sessions.items
          .filter(
            (session) =>
              session.binding.kind === "service" &&
              session.binding.deploymentId === identity.deploymentId,
          )
          .map((session) => ({
            id: session.authorizationSessionId,
            created: String(session.createdAt),
            expiry: String(session.maximumContextExpiresAt),
            state: session.state,
          }));
        const sockets = gate
          .connections()
          .filter((socket) =>
            socket.subs.some(
              (subscription) =>
                subscription.subject ===
                apis.liveprobe.API.actions["live:Watch"].subject,
            ),
          )
          .map((socket) => ({
            id: socket.id,
            closed: socket.closed,
            contexts: [
              ...new Set(
                socket.outboundContexts
                  .map((message) => message.context)
                  .filter(Boolean),
              ),
            ],
            maintenance: [
              ...new Set(
                socket.outboundContexts
                  .filter((message) => !message.subject.startsWith("live.v1."))
                  .map((message) => message.subject),
              ),
            ].slice(-12),
          }));
        throw new Error(
          `native handover failed; fixture output: ${output}; sessions=${JSON.stringify(own)}; sockets=${JSON.stringify(sockets)}`,
          { cause },
        );
      } finally {
        abort.abort();
        await framesDone;
        await hold?.release();
        if (!child.stdin.writableEnded && !child.stdin.destroyed)
          child.stdin.end("EXIT\n");
        child.kill("SIGTERM");
        await caller.connection.close();
        await child.exited;
        await target.stop();
        await targetExit;
      }
    },
    {
      interruptibleNativeProxy: true,
      authorization: {
        contextLifetimeSeconds: 38,
      },
    },
  );
});
