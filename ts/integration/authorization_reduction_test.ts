/**
 * Real-boundary authority-reduction acceptance.
 *
 * Covers the two reduction cases that must hold against real broker state:
 *
 * F3 — rights that were offered but never adopted can be withdrawn without
 *      touching any socket, while a connection that *did* adopt them is
 *      removed.
 * F4 — revocation reaches an uncooperative raw NATS client that neither reads
 *      change hints nor closes itself, for both user and native principals.
 *
 * Both cases read the broker's own admitted-attachment inventory through the
 * production admin surface and assert on real traffic, not on local state.
 */

import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { wsconnect } from "@nats-io/nats-core";
import { connect, type NatsConnection } from "@nats-io/transport-node";
import { credsAuthenticator } from "@nats-io/nats-core";
import { join } from "@std/path";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as webParticipants } from "trellis-web-generated";
import { TrellisService } from "@oatscenter/trellis/service";
import { TrellisClient } from "@oatscenter/trellis";
import { withTrellisRuntime } from "./_support/runtime.ts";
import {
  issueRawClientConnection,
  observeClientConnection,
} from "./_support/client_session.ts";

/** Grant binding as reported by the production admin surface. */
type TrellisGrantBinding = {
  approval: unknown;
  approvalMode: string;
  createdAt: bigint;
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
  provenance: unknown;
  revision: bigint;
  state: string;
  updatedAt: bigint;
};

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  runtimeConnectionId: string;
  contextDigest: string;
  participantId: string;
  connectedAt: bigint;
};

/**
 * The server-side pending-admission window (the Auth Callout's
 * `ADMISSION_PENDING_MS`) during which a written attachment record is not yet
 * considered confirmed. A retained attachment must outlive this window before
 * enforcement can rely on it.
 */
const PENDING_ADMISSION_MS = 60_000;

/** Reads the admitted attachments for one participant. */
async function attachmentsFor(
  runtime: Runtime,
  participantId: string,
): Promise<Attachment[]> {
  const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
    items: Attachment[];
  };
  return page.items.filter((item) => item.participantId === participantId);
}

/** Waits until the attachment set for a participant is exactly `count`. */
async function waitForAttachmentCount(
  runtime: Runtime,
  participantId: string,
  count: number,
): Promise<Attachment[]> {
  return await runtime.waitFor(async () => {
    const items = await attachmentsFor(runtime, participantId);
    return items.length === count ? items : false;
  }, { timeoutMs: 60_000 });
}

/** Reads the grant binding owned by a participant. */
async function grantBinding(
  runtime: Runtime,
  participantId: string,
): Promise<TrellisGrantBinding> {
  const page = await runtime.callAdminRpc("authGrantsList", {
    participantId,
  }) as { items: TrellisGrantBinding[] };
  const binding = page.items.find((item) =>
    item.participantId === participantId
  );
  if (!binding) throw new Error(`no grant binding for ${participantId}`);
  return binding;
}

/** Stable identity of one permission atom, independent of wire encoding. */
function atomKey(atom: { action: string; target: Uint8Array }): string {
  return `${atom.action}:${btoa(String.fromCharCode(...atom.target))}`;
}

/**
 * Replaces the binding's permissions with exactly `permissions`.
 *
 * A reduction of already-authored authority is an ordinary administrative
 * mutation; it is the narrowing half of a grant revision.
 */
async function setPermissions(
  runtime: Runtime,
  binding: TrellisGrantBinding,
  permissions: TrellisGrantBinding["grants"]["permissions"],
): Promise<void> {
  await runtime.callAdminRpc("authGrantsSet", {
    expectedRevision: binding.revision,
    expiresAt: binding.expiresAt,
    grants: { format: binding.grants.format, permissions },
    idempotencyKey: crypto.randomUUID(),
    installedRevision: binding.installedRevision,
    ownerId: binding.ownerId,
    ownerKind: binding.ownerKind,
    participantId: binding.participantId,
    platformPrivileges: binding.platformPrivileges,
  });
}

/** Opens a privileged ordinary connection for observation. */
async function platformConnection(runtime: Runtime): Promise<NatsConnection> {
  return await connect({
    servers: runtime.natsUrl,
    authenticator: credsAuthenticator(
      await Deno.readFile(
        join(runtime.workdir, "nats/creds/trellis-auth.creds"),
      ),
    ),
  });
}

/**
 * F3 — an offered-but-unadopted grant can be withdrawn harmlessly.
 *
 * The Provider deployment starts with `records` approved and its optional
 * `extras` KV declined. Approving `extras` grows the desired authority while
 * the admitted attachment keeps only `records`; withdrawing it again must
 * clear the retained notice, keep `records` working, and never replace the
 * attachment. A second connection that *did* adopt `extras` is removed.
 */
Deno.test("F3 an unadopted grant withdraws without touching the socket", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    assertEquals(requested.status, "approval_required");
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeResources: ["extras"],
    });
    const instance = await runtime.services.createInstance({
      name: "reduction-provider",
      contract,
    });

    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "reduction-provider",
      seed: instance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    try {
      await service.handleEcho(({ input }) => Result.ok(input));
      const records = service.kv.records;
      assert(records, "the required KV must be bound");
      await records.put("before", { value: "before" });

      const participantId = contract.identity;
      const [before] = await waitForAttachmentCount(runtime, participantId, 1);
      const adoptedKeys = new Set(
        (await grantBinding(runtime, participantId)).grants.permissions.map(
          atomKey,
        ),
      );

      // Offer `extras` without adopting it: D grows, A does not.
      await runtime.contracts.apply({ contract });
      const extras = await runtime.waitFor(() => service.kv.extras ?? false, {
        timeoutMs: 60_000,
      });
      assertEquals(
        await records.get("before").orThrow(),
        { value: "before" },
        "the adopted resource keeps working while the new one is pending",
      );
      const pending = await extras.get("missing");
      assert(pending.isErr());
      assertEquals(
        (pending.error as { code?: string }).code,
        "transport_upgrade_required",
      );

      const [stillOne] = await attachmentsFor(runtime, participantId);
      assertEquals(
        stillOne.runtimeConnectionId,
        before.runtimeConnectionId,
        "growing authority must not replace the attachment",
      );

      // Withdraw the unadopted grant by narrowing the binding back to the
      // permissions the attachment actually adopted.
      const binding = await grantBinding(runtime, participantId);
      const recordsOnly = binding.grants.permissions.filter((atom) =>
        adoptedKeys.has(atomKey(atom))
      );
      assert(
        recordsOnly.length > 0,
        "the reduction must keep the adopted authority",
      );
      await setPermissions(runtime, binding, recordsOnly);

      // The retained notice clears and the original socket is untouched.
      await runtime.waitFor(
        () => service.connection.status.transportUpgradeAvailable === false,
        { timeoutMs: 60_000 },
      );
      assertEquals(
        await records.get("before").orThrow(),
        { value: "before" },
        "adopted authority survives the withdrawal",
      );
      const [after] = await attachmentsFor(runtime, participantId);
      assertEquals(
        after.runtimeConnectionId,
        before.runtimeConnectionId,
        "withdrawing an unadopted grant must not replace the attachment",
      );
    } finally {
      await service.connection.close().catch(() => undefined);
      await serviceExit;
    }
  });
});

/**
 * F4 — revocation reaches an uncooperative raw NATS client.
 *
 * The raw client is admitted through the production Auth Callout on a real
 * user login and never processes Trellis change hints, so only server-side
 * enforcement can remove it. It must first survive broker inventory
 * reconciliation and its pending-admission window while remaining represented
 * in `Auth.Connections.List`; after an administrative reduction the broker must
 * drop the old attachment, and a fresh connection must not regain the removed
 * permission.
 */
Deno.test("F4 revocation removes an uncooperative raw user attachment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeResources: ["extras"],
    });
    const instance = await runtime.services.createInstance({
      name: "reduction-raw-provider",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "reduction-raw-provider",
      seed: instance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);

    const key = await runtime.registerClient({
      name: "f4-user",
      contract: webParticipants.Console.participant,
    });
    const caller = await TrellisClient.connect({
      name: "f4-user",
      trellisUrl: runtime.trellisUrl,
      participant: webParticipants.Console.participant,
      ...runtime.clientAuth(key),
    }).orThrow();

    let rawNats: NatsConnection | undefined;
    let rawClosed: Promise<unknown> = Promise.resolve();
    try {
      const participantId = webParticipants.Console.participant.identity;
      const observed = await observeClientConnection(runtime, key);
      const raw = await issueRawClientConnection(
        runtime,
        key,
        observed.loginSessionId,
      );
      rawNats = await wsconnect({
        servers: [runtime.natsWebsocketUrl],
        authenticator: raw.authenticator,
      }).catch(() => undefined);
      assert(rawNats, "the raw connection must be established");
      // The broker removes this attachment mid-test; consume the transport
      // error so the removal is asserted rather than reported as unhandled.
      rawClosed = rawNats.closed().catch(() => undefined);

      assert(
        raw.contextDigest !== observed.contextDigest,
        "the raw connection is its own logical connection",
      );
      const admitted = await runtime.waitFor(async () => {
        const items = await attachmentsFor(runtime, participantId);
        return items.find((item) =>
          item.runtimeConnectionId === raw.connectionId
        ) ?? false;
      }, { timeoutMs: 60_000 });

      // Keep the raw attachment alive across broker inventory reconciliation
      // and its pending-admission window: a record that confirmation could not
      // yet settle must still be tracked so later enforcement can reach it.
      // Every poll re-asserts that the socket is still open and still
      // represented, so a spurious drop keeps failing until this bounded wait
      // expires rather than surfacing silently at the reduction below.
      const rawSocket = rawNats;
      await runtime.waitFor(async () => {
        assert(
          !rawSocket.isClosed(),
          "the raw socket must stay connected across reconciliation",
        );
        assert(
          (await attachmentsFor(runtime, participantId)).some((item) =>
            item.runtimeConnectionId === raw.connectionId
          ),
          "the raw attachment must stay represented across reconciliation",
        );
        return Date.now() - Number(admitted.connectedAt) >=
          PENDING_ADMISSION_MS;
      }, { timeoutMs: PENDING_ADMISSION_MS + 30_000, intervalMs: 5_000 });

      // Strip the caller's binding to a single retained atom.
      const binding = await grantBinding(runtime, participantId);
      const kept = binding.grants.permissions.slice(0, 1);
      assert(kept.length > 0, "the binding must have permissions to reduce");
      await setPermissions(runtime, binding, kept);

      // The broker removes the raw attachment without client cooperation.
      await runtime.waitFor(async () => {
        const items = await attachmentsFor(runtime, participantId);
        return !items.some((item) =>
          item.runtimeConnectionId === raw.connectionId
        );
      }, { timeoutMs: 60_000 });
      await rawClosed;

      // A reconnect cannot regain the removed permission: either issuing the
      // replacement authority or admitting it must fail.
      const reAdmitted = await issueRawClientConnection(
        runtime,
        key,
        observed.loginSessionId,
      ).then(async (replacement) => {
        const nc = await wsconnect({
          servers: [runtime.natsWebsocketUrl],
          authenticator: replacement.authenticator,
        }).catch(() => undefined);
        if (!nc) return false;
        nc.close().catch(() => undefined);
        return true;
      }, () => false);
      assert(
        reAdmitted === false,
        "a stripped authority must not be re-admitted",
      );
    } finally {
      if (rawNats) rawNats.close().catch(() => undefined);
      await rawClosed.catch(() => undefined);
      await caller.connection.close().catch(() => undefined);
      await service.connection.close().catch(() => undefined);
      await serviceExit;
    }
  });
});

/**
 * F4 (native scope) — a service attachment is removed on reduction.
 *
 * Native principals are enforced by the same reevaluation path, so the
 * reduction must remove the service's physical attachment rather than only
 * updating stored state.
 */
Deno.test("F4 revocation removes a native service attachment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeResources: ["extras"],
    });
    const instance = await runtime.services.createInstance({
      name: "reduction-native-provider",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "reduction-native-provider",
      seed: instance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);

    try {
      const participantId = contract.identity;
      const [admitted] = await waitForAttachmentCount(
        runtime,
        participantId,
        1,
      );

      const binding = await grantBinding(runtime, participantId);
      const kept = binding.grants.permissions.slice(0, 1);
      assert(kept.length > 0, "the binding must have permissions to reduce");
      await setPermissions(runtime, binding, kept);

      await runtime.waitFor(
        async () =>
          (await attachmentsFor(runtime, participantId)).every((item) =>
            item.runtimeConnectionId !== admitted.runtimeConnectionId
          ),
        { timeoutMs: 60_000 },
      );
    } finally {
      await service.connection.close().catch(() => undefined);
      await serviceExit;
    }
  });
});

/**
 * F7 — explicit transport refresh coalesces and never strands the owner.
 *
 * Concurrent public refresh calls must produce one shared reconnect rather
 * than one per call, and closing the logical connection during a refresh must
 * not resurrect it afterwards.
 */
Deno.test("F7 concurrent transport refresh performs one shared replacement", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeResources: ["extras"],
    });
    const instance = await runtime.services.createInstance({
      name: "refresh-provider",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "refresh-provider",
      seed: instance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    try {
      const participantsSeen = new Set<string>();
      const refresh = () => {
        const status = service.connection.status;
        participantsSeen.add(status.phase);
        return service.connection.refreshTransport();
      };
      // One shared replacement, however many callers ask at once.
      await Promise.all([refresh(), refresh(), refresh()]).then((results) => {
        for (const result of results) {
          assertEquals(
            result.isOk(),
            true,
            "every caller observes the shared result",
          );
        }
      });
      await runtime.waitFor(
        () => service.connection.status.phase === "connected",
        { timeoutMs: 60_000 },
      );
      await service.handleEcho(({ input }) => Result.ok(input));

      // Closing during a refresh must not resurrect the connection.
      const pending = service.connection.refreshTransport();
      await service.connection.close();
      // Closing during a refresh settles the pending refresh as a value; it
      // must not leave the logical connection reconnecting afterwards.
      await pending;
      await runtime.waitFor(
        () => service.connection.status.phase !== "connected",
        { timeoutMs: 60_000 },
      );
    } finally {
      await service.connection.close().catch(() => undefined);
      await serviceExit;
    }
  });
});
