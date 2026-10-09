/**
 * Real-boundary authority-reduction acceptance.
 *
 * Covers the two reduction cases that must hold against real broker state:
 *
 * F3 — withdrawing grown rights closes the wider generation while preserving
 *      the original safe attachment and its accepted work.
 * F4 — revocation reaches an uncooperative raw NATS client that neither reads
 *      change hints nor closes itself, for both user and native principals.
 *
 * Both cases read the broker's own admitted-attachment inventory through the
 * production admin surface and assert on real traffic, not on local state.
 */

import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { type NatsConnection, wsconnect } from "@nats-io/nats-core";

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
  /** Per-physical-attachment identity, distinct across generations. */
  connectionId: string;
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

/**
 * F3 — a grown grant can be withdrawn, closing only the wider generation.
 *
 * The Provider deployment starts with `records` approved and its optional
 * `extras` KV declined. Approving `extras` is adopted automatically on a wider
 * generation under the same logical connection; withdrawing it again must
 * close that generation immediately, keep `records` working on the original
 * attachment, and keep the logical connection identity stable.
 */
Deno.test("F3 a reduction closes the wider generation and keeps the original", async () => {
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
    let closeCaller: (() => Promise<unknown>) | undefined;
    const retainedAbort = new AbortController();
    try {
      // A genuinely covered RPC accepted on the original generation and held
      // across the reduction: real accepted work whose lease keeps the healthy
      // original alive, so survivor reuse is proven by work, not by a pin.
      let releaseHeld: (() => void) | undefined;
      const heldGate = new Promise<void>((resolve) => {
        releaseHeld = resolve;
      });
      let heldEntered = false;
      await service.handleEcho(({ input }) => {
        if (input.value === "held") {
          heldEntered = true;
          return heldGate.then(() => Result.ok(input));
        }
        return Result.ok(input);
      });
      let feeds = 0;
      await service.handleWatch(async ({ emit, signal }) => {
        const feed = ++feeds;
        let frame = 0;
        while (!signal.aborted) {
          await emit({ value: `feed-${feed}-${++frame}` }).orThrow();
          await new Promise((resolve) => setTimeout(resolve, 25));
        }
      });
      const caller = await runtime.connectClient({
        name: "reduction-caller",
        contract: participants.Caller.participant,
        timeout: 120_000,
      });
      closeCaller = () => caller.connection.close();
      const records = service.kv.records;
      assert(records, "the required KV must be bound");
      await records.put("before", { value: "before" });
      // Generic RPC intake on the original generation before any growth.
      assertEquals(await caller.echo({ value: "before" }).orThrow(), {
        value: "before",
      });
      // A live observation accepted on the original generation, kept across the
      // growth and the reduction to prove retained continuity.
      const retainedFeed = await caller.watch({}, {
        signal: retainedAbort.signal,
      }).orThrow();
      const retained = retainedFeed[Symbol.asyncIterator]();
      assert((await retained.next()).value?.value?.startsWith("feed-"));

      const participantId = contract.identity;
      const [before] = await waitForAttachmentCount(runtime, participantId, 1);
      const adoptedKeys = new Set(
        (await grantBinding(runtime, participantId)).grants.permissions.map(
          atomKey,
        ),
      );

      // Hold one RPC accepted on the original generation across the reduction.
      const held = caller.echo({ value: "held" });
      await runtime.waitFor(() => heldEntered, { timeoutMs: 30_000 });

      // Grow `extras`: it is adopted automatically on a wider generation under
      // the same logical connection.
      await runtime.contracts.apply({ contract });
      const extras = await runtime.waitFor(() => service.kv.extras ?? false, {
        timeoutMs: 60_000,
      });
      assertEquals(
        await records.get("before").orThrow(),
        { value: "before" },
        "the adopted resource keeps working across the growth",
      );
      await extras.put("grown", { value: "grown" });
      assertEquals(await extras.get("grown").orThrow(), { value: "grown" });
      const grownAttachments = await attachmentsFor(runtime, participantId);
      assert(
        grownAttachments.length >= 2 &&
          grownAttachments.every((item) =>
            item.runtimeConnectionId === before.runtimeConnectionId
          ),
        "growth must adopt a wider generation on the same logical connection",
      );
      assert(
        new Set(grownAttachments.map((item) => item.connectionId)).size >= 2,
        "the wider generation is a distinct physical attachment",
      );
      const grownPhysical = grownAttachments.find((item) =>
        item.connectionId !== before.connectionId
      );
      assert(grownPhysical, "the wider generation has its own attachment");

      // Withdraw the grown grant by narrowing the binding back to the
      // permissions the original attachment adopted.
      const binding = await grantBinding(runtime, participantId);
      const recordsOnly = binding.grants.permissions.filter((atom) =>
        adoptedKeys.has(atomKey(atom))
      );
      assert(
        recordsOnly.length > 0,
        "the reduction must keep the adopted authority",
      );
      await setPermissions(runtime, binding, recordsOnly);

      // The wider generation closes immediately; the *exact original physical
      // attachment* survives under the same logical connection. `runtimeConnectionId`
      // is the logical identity and cannot prove this, so assert the per-physical
      // `connectionId` from the authoritative broker inventory.
      const survived = await runtime.waitFor(async () => {
        const items = await attachmentsFor(runtime, participantId);
        return items.length === 1 ? items : undefined;
      }, { timeoutMs: 60_000 });
      assertEquals(
        survived[0].connectionId,
        before.connectionId,
        "the original physical attachment must survive the reduction",
      );
      assert(
        !survived.some((item) =>
          item.connectionId === grownPhysical.connectionId
        ),
        "the wider physical attachment must be gone from broker inventory",
      );
      assertEquals(
        await records.get("before").orThrow(),
        { value: "before" },
        "adopted authority survives the withdrawal",
      );
      // The covered RPC accepted on the original generation completes on that
      // same healthy original after the wider one is forced away.
      releaseHeld?.();
      assertEquals(
        await held.orThrow(),
        { value: "held" },
        "covered accepted work must complete on the reused original",
      );
      // The original generation is reactivated after the reduction; its generic
      // RPC intake must have been reinstalled before publication, so a fresh
      // call is served rather than timing out into a drained generation.
      const afterReduction = await runtime.waitFor(async () => {
        const r = await caller.echo({ value: "after-reduction" });
        return r.isOk() ? r.orThrow() : undefined;
      }, { timeoutMs: 60_000, intervalMs: 1_000 });
      assertEquals(afterReduction, { value: "after-reduction" });
      // Fresh live intake works on the reactivated survivor: its generic
      // live-open subscription was reinstalled on the same provider. The
      // reduction re-issues the provider's authorization context, so bound the
      // wait for intake to serve under the converged context.
      const freshAbort = new AbortController();
      const freshFeed = await runtime.waitFor(async () => {
        try {
          return await caller.watch({}, { signal: freshAbort.signal })
            .orThrow();
        } catch {
          return undefined;
        }
      }, { timeoutMs: 90_000, intervalMs: 1_000 });
      const fresh = freshFeed[Symbol.asyncIterator]();
      const freshFrame = (await fresh.next()).value?.value;
      assert(
        typeof freshFrame === "string" && freshFrame.startsWith("feed-"),
        "a fresh live observation must open on the reactivated survivor",
      );
      assertEquals(feeds, 2, "the fresh live observation opens a new feed");
      // A reduction revokes the *original* authorization context the retained
      // observation was accepted under, so that accepted session must end with a
      // bounded authorization error rather than keep streaming under withdrawn
      // authority. This is the distinct live failure a reduction has (a growth
      // leaves the accepted context intact).
      let retainedEnded = false;
      try {
        const next = await retained.next();
        retainedEnded = next.done === true || next.value === undefined;
      } catch {
        retainedEnded = true;
      }
      assert(
        retainedEnded,
        "a retained observation must end when the reduction revokes its context",
      );
      freshAbort.abort();
    } finally {
      retainedAbort.abort();
      await closeCaller?.().catch(() => undefined);
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
