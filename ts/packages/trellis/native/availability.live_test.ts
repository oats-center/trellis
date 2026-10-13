import { ok } from "@oatscenter/result";
import { credsAuthenticator } from "@nats-io/nats-core";
import { connect } from "@nats-io/transport-node";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { test } from "node:test";
import { ulid } from "ulid";
import { z } from "zod";
import {
  apps,
  participants,
} from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  apps as adminApps,
  types as adminTypes,
} from "../../trellis-testkit/trellis/index.js";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { generateSessionSeed } from "../../trellis-testkit/src/auth/random.ts";
import { connectNativeClient } from "../native.ts";
import type { TrellisConnection } from "../connection.ts";
import { createAuth } from "../portable.ts";
import { TrellisService } from "../service/mod.ts";

test(
  "native optional Operation uses approved whole-capability availability and fails after reduction",
  { timeout: 60_000 },
  async (context) => {
    const issuer = process.env.TRELLIS_NATIVE_TEST_ISSUER;
    const trellisUrl = process.env.TRELLIS_NATIVE_TEST_URL;
    const username = process.env.TRELLIS_NATIVE_TEST_USERNAME;
    const password = process.env.TRELLIS_NATIVE_TEST_PASSWORD;
    const natsUrl = process.env.TRELLIS_NATIVE_TEST_NATS_URL;
    const monitorUrl = process.env.TRELLIS_NATIVE_TEST_MONITOR_URL;
    const systemCredentials = process.env.TRELLIS_NATIVE_TEST_SYSTEM_CREDS;
    assert.ok(
      issuer && trellisUrl && username && password,
      "provide the ordinary live runtime and seeded administrator credentials",
    );
    assert.ok(
      natsUrl && monitorUrl && systemCredentials,
      "provide the same host's actual NATS URL, monitor URL, and system credentials for broker disconnect",
    );
    const inventory = z.object({
      connections: z.array(z.object({ cid: z.number() })),
    });
    const connections = async () =>
      inventory
        .parse(
          await (
            await fetch(`${monitorUrl}/connz`, { signal: context.signal })
          ).json(),
        )
        .connections.map((connection) => connection.cid);
    const disconnect = async (
      before: readonly number[],
      connection: TrellisConnection,
    ) => {
      const current = await connections();
      const cid = Math.max(...current.filter((cid) => !before.includes(cid)));
      assert.ok(
        Number.isFinite(cid),
        "the connected fixture must have a real broker socket",
      );
      const varz = z
        .object({ server_id: z.string() })
        .parse(
          await (
            await fetch(`${monitorUrl}/varz`, { signal: context.signal })
          ).json(),
        );
      const system = await connect({
        servers: natsUrl,
        authenticator: credsAuthenticator(await readFile(systemCredentials)),
      });
      let unsubscribe = () => {};
      const recovered = new Promise<void>((resolve, reject) => {
        let lost = false;
        unsubscribe = connection.subscribe((status) => {
          if (
            status.phase === "disconnected" ||
            status.phase === "reconnecting"
          )
            lost = true;
          if (lost && status.phase === "connected") resolve();
          if (status.phase === "closed" || status.phase === "error")
            reject(new Error(`connection ${status.phase} during recovery`));
        });
        context.signal.addEventListener(
          "abort",
          () => reject(context.signal.reason),
          { once: true },
        );
      });
      try {
        const kicked = (
          await system.request(
            `$SYS.REQ.SERVER.${varz.server_id}.KICK`,
            new TextEncoder().encode(JSON.stringify({ cid })),
          )
        ).json<{ error?: unknown }>();
        assert.equal(
          kicked.error,
          undefined,
          "the broker must actually disconnect the selected socket",
        );
        await recovered;
      } finally {
        unsubscribe();
        await system.close();
      }
    };
    const directory = await mkdtemp("/tmp/opencode/native-optional-");
    const close: Array<() => Promise<unknown>> = [];
    try {
      const adminLogin = await loginNativeTestClient({
        issuer,
        username,
        password,
        directory,
        clientId: "trellis.cli",
        request: adminApps.cli.request,
        platformPrivileges: {
          required: [
            "principals.manage",
            "apis.accept",
            "clients.manage",
            "roles.manage",
          ],
        },
        signal: context.signal,
      });
      const admin = await connectNativeClient({
        ...adminLogin,
        request: adminApps.cli.request,
        signal: context.signal,
      }).orThrow();
      close.push(() => admin.connection.close());
      await context.test(
        "generated KV works after readiness and reconnect retains data",
        async () => {
          const resource = participants.ResourceProvider.participant;
          const { deployment } = await admin
            .deploymentsCreate({
              title: `kv-availability-${ulid()}`,
              kind: resource.kind,
              participantId: resource.identity,
              packageEvidence: adminTypes.AuthPackageEvidenceCodec.decode(
                resource.packageEvidence,
              ),
              idempotencyKey: ulid(),
            })
            .orThrow();
          const seed = generateSessionSeed();
          const identity = await createAuth({ sessionKeySeed: seed });
          await admin
            .serviceInstancesProvision({
              deploymentId: deployment.deploymentId,
              identityPublicKey: identity.sessionKey,
              title: "KV availability provider",
              idempotencyKey: ulid(),
            })
            .orThrow();
          const before = await connections();
          const first = await TrellisService.connect({
            trellisUrl,
            participant: resource,
            seed,
          }).orThrow();
          close.push(() => first.stop());
          const retained = first.kv.records;
          await retained
            .put("retained", { value: "before reconnect" })
            .orThrow();
          assert.equal(
            (await retained.get("retained").orThrow())?.value,
            "before reconnect",
          );
          await disconnect(before, first.connection);
          assert.equal(
            (await retained.get("retained").orThrow())?.value,
            "before reconnect",
          );
          await retained
            .put("retained", { value: "same logical connection" })
            .orThrow();
          assert.equal(
            (await retained.get("retained").orThrow())?.value,
            "same logical connection",
          );
          await first.stop();
          assert.ok((await retained.get("retained")).isErr());
          const next = await TrellisService.connect({
            trellisUrl,
            participant: resource,
            seed,
          }).orThrow();
          close.push(() => next.stop());
          assert.equal(
            (await next.kv.records.get("retained").orThrow())?.value,
            "same logical connection",
          );
          await next.kv.records
            .put("retained", { value: "after reconnect" })
            .orThrow();
          assert.equal(
            (await next.kv.records.get("retained").orThrow())?.value,
            "after reconnect",
          );
        },
      );
      const provider = participants.NativeOptionalProvider.participant;
      const api = provider.implements[0];
      const evidence = adminTypes.AuthPackageEvidenceCodec.decode(
        provider.packageEvidence,
      );
      const { review } = await admin
        .apisReview({ apiId: api.identity, packageEvidence: evidence })
        .orThrow();
      await admin
        .apisAccept({
          reviewId: review.reviewId,
          canonicalDigest: review.canonicalDigest,
          expectedGeneration: review.currentGeneration ?? 0n,
          expectedAcceptedRevision: review.currentAcceptedRevision ?? 0n,
          mappingEdits: [],
          roleEdits: [],
          idempotencyKey: ulid(),
        })
        .orThrow();
      const { deployment } = await admin
        .deploymentsCreate({
          title: `optional-${ulid()}`,
          kind: provider.kind,
          participantId: provider.identity,
          packageEvidence: evidence,
          idempotencyKey: ulid(),
        })
        .orThrow();
      const seed = generateSessionSeed();
      const identity = await createAuth({ sessionKeySeed: seed });
      await admin
        .serviceInstancesProvision({
          deploymentId: deployment.deploymentId,
          identityPublicKey: identity.sessionKey,
          title: "optional provider",
          idempotencyKey: ulid(),
        })
        .orThrow();
      const service = await TrellisService.connect({
        trellisUrl,
        participant: provider,
        seed,
      }).orThrow();
      close.push(() => service.stop());
      await service.handleRead(({ input }) => ok(input));
      let executions = 0;
      await service.handleExtend(async ({ input, op }) => {
        executions++;
        if (input.value === "live updates") {
          const emit = await op.nextSignal("Continue").orThrow();
          await op
            .emitUpdate({ value: "provider-only forwarded update" })
            .orThrow();
          await op.acknowledgeSignal(emit.sequence).orThrow();
          const finish = await op.nextSignal("Continue").orThrow();
          await op.acknowledgeSignal(finish.sequence).orThrow();
        }
        return await op.complete(input).orThrow();
      });
      const clientId = `optional-${ulid()}`;
      await admin
        .oAuthClientsPut({
          clientId,
          title: "Optional operation",
          description: "Generated optional availability acceptance.",
          kind: "native",
          redirectUris: [],
          impliedConsent: [],
          development: true,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        })
        .orThrow();
      const subjectPassword = `subject-${ulid()}`;
      const { user } = await admin
        .usersCreate({
          username: `optional-${ulid()}`,
          password: subjectPassword,
          displayName: "Optional subject",
          idempotencyKey: ulid(),
        })
        .orThrow();
      const read = `${api.identity}::read`;
      const extend = `${api.identity}::extend`;
      const principalId = user.principal.principalId;
      for (const capabilityId of [read, extend])
        await admin
          .capabilityGrantsGrant({
            principalId,
            capabilityId,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
      const request = apps.NativeOptionalExtend.request;
      const credentials = {
        issuer,
        username: user.username,
        password: subjectPassword,
        directory,
        clientId,
        request,
        signal: context.signal,
      };
      const approvedLogin = await loginNativeTestClient({
        ...credentials,
        approvedCapabilities: [read, extend],
      });
      const beforeApproved = await connections();
      const approved = await connectNativeClient({
        ...approvedLogin,
        request,
        signal: context.signal,
      }).orThrow();
      close.push(() => approved.connection.close());
      const retained = approved.extend({ value: "approved" });
      context.diagnostic("starting the approved optional Operation");
      const result = await retained.start().orThrow();
      const terminal = await result.wait().orThrow();
      assert.equal(terminal.state, "completed");
      assert.equal(terminal.output?.value, "approved");
      assert.equal(executions, 1);
      await disconnect(beforeApproved, approved.connection);
      context.diagnostic(
        "starting the optional Operation after broker recovery",
      );
      const reconnected = await approved
        .extend({ value: "same logical connection" })
        .start()
        .orThrow();
      assert.equal(
        (await reconnected.wait().orThrow()).output?.value,
        "same logical connection",
      );
      assert.equal(executions, 2);
      const declinedLogin = await loginNativeTestClient({
        ...credentials,
        approvedCapabilities: [read],
      });
      const declined = await connectNativeClient({
        ...declinedLogin,
        request,
        signal: context.signal,
      }).orThrow();
      close.push(() => declined.connection.close());
      assert.equal(
        (await declined.read({ value: "required" }).orThrow()).value,
        "required",
      );
      assert.ok((await declined.extend({ value: "declined" }).start()).isErr());
      assert.equal(executions, 2);
      const stale = approved.extend({ value: "reduced" });
      const digest = approved.ownAuthority.current?.authorityDigest;
      assert.ok(digest);
      let unsubscribe = () => {};
      const withdrew = new Promise<void>((resolve, reject) => {
        unsubscribe = approved.ownAuthority.subscribe((snapshot) => {
          if (snapshot?.authorityDigest !== digest) resolve();
        });
        context.signal.addEventListener(
          "abort",
          () => reject(context.signal.reason),
          { once: true },
        );
      });
      const grants = await admin
        .capabilityGrantsList({ principalId })
        .orThrow();
      const grant = grants.items.find(
        (item) => item.capability.capabilityId === extend,
      );
      assert.ok(grant);
      await admin
        .capabilityGrantsRevoke({
          grantId: grant.grantId,
          expectedRevision: grant.revision,
          idempotencyKey: ulid(),
        })
        .orThrow();
      await withdrew;
      unsubscribe();
      assert.ok((await stale.start()).isErr());
      assert.equal(executions, 2);
      await admin
        .capabilityGrantsGrant({
          principalId,
          capabilityId: extend,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        })
        .orThrow();
      await approved.refreshAuthority();
      context.diagnostic(
        "starting the retained optional Operation after authorized expansion",
      );
      const expanded = await retained.start().orThrow();
      assert.equal((await expanded.wait().orThrow()).output?.value, "approved");
      assert.equal(executions, 3);
      const streaming = await approved
        .extend({ value: "live updates" })
        .start()
        .orThrow();
      const observation = await streaming
        .live({ updates: true, signal: context.signal })
        .orThrow();
      const events = observation[Symbol.asyncIterator]();
      try {
        // Receiving the initial snapshot establishes the actual provider watch
        // before the signal asks the implementation-only provider to publish.
        assert.equal((await events.next()).done, false);
        await streaming.signal("Continue", { value: "emit" }).orThrow();
        while (true) {
          const next = await events.next();
          assert.equal(next.done, false, "updates must precede completion");
          if (next.value.type === "update") {
            assert.equal(
              next.value.update.value,
              "provider-only forwarded update",
            );
            break;
          }
        }
        await streaming.signal("Continue", { value: "finish" }).orThrow();
        assert.equal(
          (await streaming.wait().orThrow()).output?.value,
          "live updates",
        );
        assert.equal(executions, 4);
      } finally {
        await observation.close();
      }
      await declined.refreshAuthority();
      assert.ok(
        (await declined.extend({ value: "still declined" }).start()).isErr(),
      );
      assert.equal(executions, 4);
    } finally {
      for (const stop of close.reverse()) await stop();
      await rm(directory, { recursive: true, force: true });
    }
  },
);
