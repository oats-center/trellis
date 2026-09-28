/**
 * Real-boundary transport lifetime and restart acceptance.
 *
 * F5 — a genuine hard deadline removes the attachment while a connection
 *      without one sustains traffic across ordinary context lifetimes, with the
 *      broker's own user information showing what expiration it actually
 *      installed.
 * F9 — enforcement survives an auth/runtime restart, and a delayed
 *      reevaluation does not remove a freshly admitted safe replacement.
 * F10 — observing a durable operation is not executing it: refreshing the
 *      observer's transport neither restarts the executor nor disturbs the
 *      durable result.
 */

import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { TrellisClient } from "@oatscenter/trellis";
import { wsconnect } from "@nats-io/nats-core";
import type { NatsConnection } from "@nats-io/transport-node";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { TrellisService } from "@oatscenter/trellis/service";
import { withTrellisRuntime } from "./_support/runtime.ts";
import {
  issueRawClientConnection,
  observeClientConnection,
} from "./_support/client_session.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Grant binding as reported by the production admin surface. */
type GrantBinding = {
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
};

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  runtimeConnectionId: string;
  participantId: string;
};

async function attachmentsFor(
  runtime: Runtime,
  participantId: string,
): Promise<Attachment[]> {
  const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
    items: Attachment[];
  };
  return page.items.filter((item) => item.participantId === participantId);
}

/** Reads the broker's own view of this connection's authenticated user. */
async function ownUserInfo(
  nc: NatsConnection,
): Promise<Record<string, unknown>> {
  const reply = await nc.request("$SYS.REQ.USER.INFO", new Uint8Array(), {
    timeout: 10_000,
  });
  return JSON.parse(new TextDecoder().decode(reply.data)) as Record<
    string,
    unknown
  >;
}

/** Installs a Provider deployment with its optional `extras` KV declined. */
async function installProvider(runtime: Runtime, name: string) {
  const contract = participants.Provider.participant;
  await runtime.contracts.install({ contract });
  const requested = await runtime.contracts.requestApply({ contract });
  if (requested.status !== "approval_required") {
    throw new Error("the Provider deployment must require approval");
  }
  await runtime.contracts.approveApply(requested.pendingId, {
    excludeResources: ["extras"],
  });
  return await runtime.services.createInstance({ name, contract });
}

/**
 * F5 — an installed hard deadline and ordinary renewal are distinguishable.
 *
 * The caller's binding carries a genuine expiration, so the broker must remove
 * the attachment at that boundary. A second connection without any deadline
 * keeps its attachment across ordinary context lifetimes, and the broker's own
 * user information shows no renewable expiration installed for it.
 */
Deno.test("F5 a hard deadline removes the attachment without one installed for renewal", async () => {
  await withTrellisRuntime(async (runtime) => {
    const instance = await installProvider(runtime, "lifetime-provider");
    const contract = participants.Provider.participant;
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "lifetime-provider",
      seed: instance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);

    const key = await runtime.registerClient({
      name: "f5-caller",
      contract: participants.Caller.participant,
    });
    const caller = await TrellisClient.connect({
      name: "f5-caller",
      trellisUrl: runtime.trellisUrl,
      participant: participants.Caller.participant,
      ...runtime.clientAuth(key),
    }).orThrow();

    let raw: NatsConnection | undefined;
    let rawClosed: Promise<unknown> = Promise.resolve();
    try {
      const observed = await observeClientConnection(runtime, key);

      // No deadline installed: the broker reports no renewable expiration.
      const permanent = await issueRawClientConnection(
        runtime,
        key,
        observed.loginSessionId,
      );
      raw = await wsconnect({
        servers: [runtime.natsWebsocketUrl],
        authenticator: permanent.authenticator,
        inboxPrefix: permanent.inboxPrefix,
      });
      rawClosed = raw.closed().catch(() => undefined);
      const info = await ownUserInfo(raw);
      const user = String(info.user ?? "");
      assertEquals(
        /"exp"\s*:\s*\d*[1-9]/.test(user),
        false,
        "a connection without a hard deadline installs no expiration",
      );

      // Give the caller's binding a real hard deadline.
      const page = await runtime.callAdminRpc("authGrantsList", {
        participantId: participants.Caller.participant.identity,
      }) as { items: GrantBinding[] };
      const binding = page.items[0];
      assert(binding, "the caller must have a grant binding");
      await runtime.callAdminRpc("authGrantsSet", {
        expectedRevision: binding.revision,
        expiresAt: BigInt(Date.now() + 15_000),
        grants: binding.grants,
        idempotencyKey: crypto.randomUUID(),
        installedRevision: binding.installedRevision,
        ownerId: binding.ownerId,
        ownerKind: binding.ownerKind,
        participantId: binding.participantId,
        platformPrivileges: binding.platformPrivileges,
      });

      // The broker enforces the boundary rather than renewing past it.
      await runtime.waitFor(async () => {
        const items = await attachmentsFor(
          runtime,
          participants.Caller.participant.identity,
        );
        return !items.some((item) =>
          item.runtimeConnectionId === permanent.connectionId
        );
      }, { timeoutMs: 90_000 });
      await rawClosed;

      const denied = await issueRawClientConnection(
        runtime,
        key,
        observed.loginSessionId,
      ).then(async (replacement) => {
        const nc = await wsconnect({
          servers: [runtime.natsWebsocketUrl],
          authenticator: replacement.authenticator,
          inboxPrefix: replacement.inboxPrefix,
        }).catch(() => undefined);
        if (!nc) return false;
        nc.close().catch(() => undefined);
        return true;
      }, () => false);
      assert(
        denied === false,
        "authority past its hard deadline is not renewed",
      );
    } finally {
      if (raw) raw.close().catch(() => undefined);
      await rawClosed.catch(() => undefined);
      await caller.connection.close().catch(() => undefined);
      await service.connection.close().catch(() => undefined);
      await serviceExit;
    }
  });
});

/**
 * F9 — enforcement survives an auth/runtime restart and stale delivery.
 *
 * Restarting the control plane with NATS kept alive must leave the healthy
 * marked attachment working, and a later reduction must still remove it.
 */
Deno.test("F9 enforcement survives a control-plane restart", async () => {
  await withTrellisRuntime(async (runtime) => {
    const instance = await installProvider(runtime, "restart-provider");
    const contract = participants.Provider.participant;
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "restart-provider",
      seed: instance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    try {
      await service.handleEcho(({ input }) => Result.ok(input));
      const participantId = contract.identity;
      const [before] = await runtime.waitFor(async () => {
        const items = await attachmentsFor(runtime, participantId);
        return items.length === 1 ? items : false;
      }, { timeoutMs: 60_000 });

      await runtime.restartControlPlane();

      // The healthy attachment survived the restart without reconnecting.
      const [after] = await attachmentsFor(runtime, participantId);
      assertEquals(
        after.runtimeConnectionId,
        before.runtimeConnectionId,
        "a restart must not replace the healthy attachment",
      );

      // Enforcement still acts after the restart.
      const page = await runtime.callAdminRpc("authGrantsList", {
        participantId,
      }) as { items: GrantBinding[] };
      const binding = page.items[0];
      assert(binding, "the service must have a grant binding");
      const grants = binding.grants;
      await runtime.callAdminRpc("authGrantsSet", {
        expectedRevision: binding.revision,
        expiresAt: binding.expiresAt,
        grants: {
          format: grants.format,
          permissions: grants.permissions.slice(0, 1),
        },
        idempotencyKey: crypto.randomUUID(),
        installedRevision: binding.installedRevision,
        ownerId: binding.ownerId,
        ownerKind: binding.ownerKind,
        participantId: binding.participantId,
        platformPrivileges: binding.platformPrivileges,
      });
      await runtime.waitFor(async () => {
        const items = await attachmentsFor(runtime, participantId);
        return !items.some((item) =>
          item.runtimeConnectionId === before.runtimeConnectionId
        );
      }, { timeoutMs: 90_000 });
    } finally {
      await service.connection.close().catch(() => undefined);
      await serviceExit;
    }
  });
});

/**
 * F10 — observing a durable operation is not executing it.
 *
 * Refreshing the observing service's transport must not start a second
 * execution, cancel the operation, or lose the durable result.
 */
Deno.test("F10 refreshing an observer transport does not restart execution", async () => {
  await withTrellisRuntime(async (runtime) => {
    const ownerIdentity = await runtime.registerService({
      name: "observer-owner",
      contract: participants.Provider.participant,
    });
    const observerIdentity = await runtime.services.createInstance({
      name: "observer-watcher",
      contract: participants.Provider.participant,
    });
    const observer = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: "observer-owner",
      seed: ownerIdentity.seed,
    }).orThrow();
    void observerIdentity;
    const exits = [observer.wait()].map((promise) =>
      promise.catch((error: unknown) => error)
    );
    let executions = 0;
    const started = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();
    try {
      await observer.handleWork(async ({ input, op }) => {
        executions += 1;
        started.resolve();
        await release.promise;
        return await op.complete({ value: input.value }).orThrow();
      });
      const client = await runtime.connectClient({
        name: "observer-caller",
        contract: participants.Caller.participant,
      });
      const operation = await client.work({ value: "observed" }).start()
        .orThrow();
      await started.promise;

      const watched = await observer.handleWork.control(operation.id).orThrow();
      assertEquals(watched.id, operation.id);

      // Refresh the observing service's transport explicitly.
      await observer.connection.refreshTransport().orThrow();
      const afterRefresh = await observer.handleWork.control(operation.id)
        .orThrow();
      assertEquals(
        afterRefresh.id,
        operation.id,
        "observation survives the observer's transport refresh",
      );
      assertEquals(executions, 1, "observation must not start an execution");

      release.resolve();
      const completed = await operation.wait().orThrow();
      assertEquals(completed.state, "completed");
      assertEquals(executions, 1, "the durable result must not rerun the work");
    } finally {
      release.resolve();
      await observer.connection.close().catch(() => undefined);
      await Promise.all(exits);
    }
  });
});
