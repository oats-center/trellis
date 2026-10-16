/**
 * Real-broker retained-receipt takeover after the owning generation is disposed.
 *
 * One service (`TransportGrowthSubject`) serves two live routes. Its optional
 * `extend` capability is initially revoked and granted later, so authority
 * growth opens a wider physical generation (G2) under the same logical
 * connection. The caller's Live observation is then opened ON G2 (the old
 * generic intake is retired, so G2 owns the session). Losing only G2's physical
 * attachment disposes G2's per-generation live
 * provider through `onRetire`: the accepted session becomes a retained receipt.
 *
 * The same-route provider installed on the surviving original generation (G1)
 * has never served a session yet must answer the caller's bounded signed CLOSE
 * from its receipt; an unrelated-route provider registered first must not be
 * the responder. This is the real production manager/provider path over a real
 * broker, no captured NATS.
 */

import { isErr } from "@oatscenter/result";
import { TransportError } from "@oatscenter/trellis/errors";
import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
  readRuntimeServiceAttachments as attachmentsFor,
} from "./_support/broker_inventory.ts";
import { repoTrellisSource, withTrellisRuntime } from "./_support/runtime.ts";

/** Optional capability id declined at consent and grown later. */
const CAPABILITY_B = "runtime-trellis.transport_growth@v1::extend";
const SUBJECT_DEPLOYMENT = "receipt-takeover-subject";
const TARGET_DEPLOYMENT = "receipt-takeover-target";

/** Canonical short lifetimes; no timing widening for the receipt window. */
const runtimeOptions = {
  interruptibleNativeProxy: true,
  authorization: {
    contextLifetimeSeconds: 80,
    refreshLeadSeconds: 15,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 46,
  },
  trellis: { source: repoTrellisSource() },
};

test("a retained receipt is answered by the surviving same-route generation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const subjectContract = participants.TransportGrowthSubject.participant;
    const targetContract = participants.TransportGrowthTarget.participant;

    // The subject's `transport_growth` dependency deployment must resolve
    // before the subject may bootstrap its own authority.
    await runtime.contracts.apply({
      deployment: TARGET_DEPLOYMENT,
      contract: targetContract,
    });
    const targetInstance = await runtime.services.createInstance({
      deployment: TARGET_DEPLOYMENT,
      name: "receipt-takeover-target",
      contract: targetContract,
    });
    const target = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: targetContract,
      name: "receipt-takeover-target",
      seed: targetInstance.seed,
    }).orThrow();
    const targetExit = target.wait().catch((error: unknown) => error);
    await target.handleAdvance(() => Result.ok({}));
    await target.handleExtend(() => Result.ok({}));

    await runtime.contracts.install({ contract: subjectContract });
    await runtime.contracts.apply({
      deployment: SUBJECT_DEPLOYMENT,
      contract: subjectContract,
    });
    const instance = await runtime.services.createInstance({
      deployment: SUBJECT_DEPLOYMENT,
      name: "receipt-takeover-subject",
      contract: subjectContract,
    });
    const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
      capabilityId: CAPABILITY_B,
    });
    const grant = grants.items[0];
    assert(grant);
    await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
      grantId: grant.grantId,
      expectedRevision: grant.revision,
      idempotencyKey: ulid(),
    });
    const subject = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: subjectContract,
      name: "receipt-takeover-subject",
      seed: instance.seed,
    }).orThrow();
    const subjectExit = subject.wait().catch((error: unknown) => error);
    const subjectId = subjectContract.identity;

    // An accepted RPC keeps G1 alive across growth without offering Watch on
    // that generation. An idle superseded generation must otherwise reap.
    let inspectionAccepted = 0;
    let inspectionRelease = Promise.withResolvers<void>();
    await subject.handleInspect(async () => {
      inspectionAccepted += 1;
      await inspectionRelease.promise;
      return Result.ok({ starts: 0n, active: 0n, cleanups: 0n, emitted: 0n });
    });

    const emitters = new Set<() => void>();

    // The unrelated route (`probe_aux.Pulse`) is installed FIRST, so it
    // registers as an unrelated-route live provider before the Watch
    // route's provider on the same connection.
    await subject.handlePulse(async ({ emit, signal }) => {
      let index = 1n;
      while (!signal.aborted) {
        await emit({ value: String(index) }).orThrow();
        index += 1n;
        await new Promise<void>((resolve) => {
          const timer = setTimeout(resolve, 250);
          emitters.add(() => {
            clearTimeout(timer);
            resolve();
          });
        });
      }
    });
    // The Watch route under test: installed at readiness, never offered a
    // session on the original generation.
    await subject.handleWatch(async ({ emit, signal }) => {
      let index = 1n;
      while (!signal.aborted) {
        await emit({
          runId: "receipt-run",
          streamId: "watch",
          sourceGeneration: 1n,
          index,
          payload: new Uint8Array(),
          padding: "",
        }).orThrow();
        index += 1n;
        await new Promise<void>((resolve) => {
          const timer = setTimeout(resolve, 250);
          emitters.add(() => {
            clearTimeout(timer);
            resolve();
          });
        });
      }
    });

    const caller = await runtime.connectClient({
      name: "receipt-takeover-caller",
      contract: apps.LiveProbeCaller.request,
    });
    const gate = runtime.nativeTransportGate();
    const inspectionAbort = new AbortController();
    let heldInspection: Promise<unknown> | undefined;

    try {
      // Baseline: the original attachment and its narrow atom set.
      const [original] = await attachmentsFor(runtime, subjectId);
      assert(original, "the subject must have an attachment");
      const logical = original.runtimeId;
      assert((await subject.extend({})).isErr());

      const inspection = caller
        .inspect(
          {
            runId: ulid(),
            streamId: "hold-baseline",
          },
          { timeout: 30_000, signal: inspectionAbort.signal },
        )
        .take();
      heldInspection = inspection;
      await runtime.waitFor(() => inspectionAccepted === 1);
      // Cancelling the caller's wait does not undo accepted provider work. The
      // handler retains G1 until explicitly released, independently of renewal.
      inspectionAbort.abort();
      const cancelled = await inspection;
      assert(isErr(cancelled));
      assert(cancelled.error instanceof TransportError);
      assert.equal(cancelled.error.code, "trellis.request.cancelled");
      inspectionRelease.resolve();
      inspectionRelease = Promise.withResolvers<void>();
      const deadline = caller
        .inspect(
          { runId: ulid(), streamId: "deadline-baseline" },
          { timeout: 1_000 },
        )
        .take();
      await runtime.waitFor(() => inspectionAccepted === 2);
      const timedOut = await deadline;
      assert(isErr(timedOut));
      assert(timedOut.error instanceof TransportError);
      assert.equal(timedOut.error.code, "trellis.request.timeout");

      // Grow the declined optional capability: a wider generation is adopted
      // under the same logical connection, automatically.
      await runtime.callAdminRpc("authCapabilityGrantsGrant", {
        principalId: grant.principalId,
        capabilityId: CAPABILITY_B,
        expectedRevision: 0n,
        idempotencyKey: ulid(),
      });
      const overlap = await runtime.waitFor(
        async () => {
          const items = (await attachmentsFor(runtime, subjectId)).filter(
            (item) => item.runtimeId === logical,
          );
          return items.some(
            (item) => item.attachmentId === original.attachmentId,
          ) && items.some((item) => item.attachmentId !== original.attachmentId)
            ? items
            : false;
        },
        { timeoutMs: 90_000 },
      );
      const wider = overlap.find(
        (item) => item.attachmentId !== original.attachmentId,
      );
      assert(wider, "growth must create a distinct physical generation");
      const overlapInventory = await readRuntimeBrokerInventory(runtime);
      const [g1Socket] = admittedConnections(
        overlapInventory,
        new Set([original.attachmentId]),
      );
      const [g2Socket] = admittedConnections(
        overlapInventory,
        new Set([wider.attachmentId]),
      );
      assert(g1Socket, "the held baseline RPC must keep G1 broker-admitted");
      assert(g2Socket, "the wider generation must be broker-admitted");
      assert(brokerConnectionKey(g1Socket) !== brokerConnectionKey(g2Socket));
      assert.deepEqual(await subject.extend({}).orThrow(), {});

      // Only now open the observation, so the wider generation owns it; the
      // original generation's generic intake has already been retired.
      const feed = await caller
        .watch({
          runId: ulid(),
          streamId: "watch",
        })
        .orThrow();
      const frames: bigint[] = [];
      const feedTask = (async () => {
        for await (const frame of feed) frames.push(frame.index);
      })().catch(() => undefined);
      await runtime.waitFor(() => frames.length > 0, { timeoutMs: 30_000 });

      // Lose only the serving attachment, not its logical authorization session.
      // Keep replacement intake unready so it cannot answer the retained receipt.
      gate.arm(".Inspect");
      const readiness = gate.barrierHeld();
      let heldGeneration: number | undefined;
      void readiness.then((id) => {
        heldGeneration = id;
      });
      await runtime.callAdminRpc("authConnectionsKick", {
        attachmentId: wider.attachmentId,
        idempotencyKey: ulid(),
      });
      await runtime.waitFor(
        async () => {
          const items = await attachmentsFor(runtime, subjectId);
          const inventory = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [g1Socket.server, g2Socket.server],
          });
          const physicalKeys = new Set(inventory.map(brokerConnectionKey));
          return items.some(
            (item) =>
              item.runtimeId === logical &&
              item.attachmentId === original.attachmentId,
          ) &&
            physicalKeys.has(brokerConnectionKey(g1Socket)) &&
            !physicalKeys.has(brokerConnectionKey(g2Socket))
            ? items
            : false;
        },
        { timeoutMs: 60_000 },
      );
      await runtime.waitFor(() => heldGeneration, { timeoutMs: 30_000 });

      // The accepted session is now a retained receipt on the shared manager;
      // the surviving same-route provider has never offered a session. A
      // bounded signed CLOSE from the original caller must be confirmed by a
      // real exchange, not a no-op.
      const receipt = await feed.close().orThrow();
      assert.deepEqual(
        receipt.remote,
        "confirmed",
        "the surviving same-route generation must confirm the close receipt",
      );
      await feedTask;
      await gate.release();
      inspectionRelease.resolve();
    } finally {
      await gate.release();
      inspectionRelease.resolve();
      await heldInspection?.catch(() => undefined);
      for (const wake of emitters) wake();
      await caller.connection.close().catch(() => undefined);
      await subject.connection.close().catch(() => undefined);
      await target.connection.close().catch(() => undefined);
      await Promise.all([subjectExit, targetExit]);
    }
  }, runtimeOptions);
});
