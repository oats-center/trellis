/**
 * Real-broker candidate-staging acceptance for the Rust transport client.
 *
 * Both cases start one `TransportGrowthSubject` service with its optional
 * `Extend` capability approved, so its only initial native attachment is the
 * *wider* generation. Removing that capability through a real administrative
 * grant revision force-removes that single attachment and leaves no already
 * admitted safe survivor, which is the exact condition the SDK's private
 * provisional staging path exists for.
 *
 * Case A proves automatic recovery: the SDK fetches the narrowed authority,
 * opens exactly one private provisional generation, and the *same* physical
 * socket becomes the serving current generation. Real outbound `Advance`,
 * inbound `liveprobe Watch`, and outbound `Progress` are served from it under
 * the new narrowed context, `Extend` is refused, and the broker's own complete
 * inventory shows the wider attachment gone and exactly one narrowed one.
 *
 * Case B withholds a real server reply on the private stage's registry
 * `CONSUMER.INFO`, proves the stage socket is admitted while no application
 * intake is serving, then drops the real logical client through its ordinary
 * owners and keeps the process alive. The exact stage socket must leave the
 * broker, the process must still answer `PING`, and releasing the retained late
 * reply must not resurrect any candidate or serving socket.
 *
 * Physical identity is the broker `server:cid` read from the authenticated
 * `$SYS` CONNZ inventory; the SDK logical connection is `runtimeConnectionId`
 * and must stay stable.
 */

import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  type BrokerConnection,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = TrellisTestRuntime;

/** Qualified API identity the fixture declares for the growth capability. */
const GROWTH_API = "runtime-trellis.transport_growth@v1";

/**
 * One deployment carries one participant assignment, so the subject and its
 * target use separate deployments and nothing relies on the runtime default.
 */
const SUBJECT_DEPLOYMENT = "cs-subject";
const TARGET_DEPLOYMENT = "cs-target";

/** Short lifetimes so a reduction is picked up by a real refresh promptly. */
const runtimeOptions = {
  authorization: {
    contextLifetimeSeconds: 76,
    refreshLeadSeconds: 15,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 46,
  },
  // The private provisional stage is only observable through the native proxy's
  // readiness/reply gate, so the case runs the real intercepting path.
  interruptibleNativeProxy: true,
};

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  contextDigest: string;
};

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

/** Exact permission atom for one declared RPC surface. */
async function rpcAtom(name: string) {
  return {
    action: "call" as const,
    target: await encodePermissionTargetWasm({
      kind: "apiSurface",
      api: GROWTH_API,
      surface: "rpc",
      name,
    }),
  };
}

/** Stable identity of one permission atom, independent of wire encoding. */
function atomKey(atom: { action: string; target: Uint8Array }): string {
  return `${atom.action}:${btoa(String.fromCharCode(...atom.target))}`;
}

/** Reads the grant binding owned by one participant. */
async function grantBinding(
  runtime: Runtime,
  participantId: string,
): Promise<GrantBinding> {
  const page = await runtime.callAdminRpc("authGrantsList", {
    participantId,
  }) as { items: GrantBinding[] };
  const binding = page.items.find((item) =>
    item.participantId === participantId
  );
  if (!binding) throw new Error(`no grant binding for ${participantId}`);
  return binding;
}

/**
 * Replaces one binding's permissions with exactly `permissions`.
 *
 * A reduction of already-authored authority is an ordinary administrative
 * mutation; it is the narrowing half of a grant revision.
 */
async function setPermissions(
  runtime: Runtime,
  binding: GrantBinding,
  permissions: GrantBinding["grants"]["permissions"],
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

/** The subject's admitted attachments from the production admin surface. */
async function attachmentsFor(
  runtime: Runtime,
  participantId: string,
): Promise<Attachment[]> {
  const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
    items: Attachment[];
  };
  return page.items.filter((item) => item.participantId === participantId);
}

/** A bounded command channel to the Rust growth leg. */
type GrowthLeg = {
  waitFor(marker: string, count?: number): Promise<void>;
  waitForAny(markers: readonly string[]): Promise<string>;
  send(command: string): Promise<void>;
  count(marker: string): number;
  latestObservedIndex(): number | undefined;
};

/**
 * Spawns the Rust growth leg, runs `body`, and always reaps the process on a
 * bounded exit.
 */
async function withTransportGrowthLeg(
  runtime: Runtime,
  seed: string,
  deadlineMs: number,
  body: (leg: GrowthLeg) => Promise<void>,
  nativeOwnerScope = false,
): Promise<void> {
  const child = new Deno.Command("setsid", {
    args: rustFixtureArgv("transport_growth_subject"),
    env: {
      TRELLIS_URL: runtime.trellisUrl,
      TRELLIS_IDENTITY_SEED: seed,
      TRELLIS_NATIVE_OWNER_SCOPE: nativeOwnerScope ? "1" : "0",
    },
    stdin: "piped",
    stdout: "piped",
    stderr: "inherit",
  }).spawn();
  const lines: string[] = [];
  let output = "";
  let exited = false;
  const status = child.status.then((result) => {
    exited = true;
    return result;
  });
  const stdin = child.stdin.getWriter();
  const drain = (async () => {
    const reader = child.stdout.pipeThrough(new TextDecoderStream())
      .getReader();
    let buffer = "";
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      output += chunk.value;
      buffer += chunk.value;
      const parts = buffer.split("\n");
      buffer = parts.pop() ?? "";
      for (const part of parts) {
        const line = part.trim();
        if (line.length > 0) lines.push(line);
      }
    }
    const tail = buffer.trim();
    if (tail.length > 0) lines.push(tail);
  })();
  const drainSettled = drain.catch(() => {});
  const deadline = Date.now() + deadlineMs;
  const remainingMs = (): number => Math.max(0, deadline - Date.now());
  const failureMarker = (): string | undefined =>
    lines.find((line) =>
      line.startsWith("TRANSPORT_GROWTH_ERROR ") ||
      line.startsWith("TRANSPORT_GROWTH_ADVANCE_ERROR ")
    );

  const leg: GrowthLeg = {
    async waitFor(marker: string, count = 1): Promise<void> {
      await runtime.waitFor(
        () => {
          if (lines.filter((line) => line.startsWith(marker)).length >= count) {
            return true;
          }
          const error = failureMarker();
          if (error !== undefined) {
            throw new Error(`Rust transport growth leg failed: ${error}`);
          }
          return undefined;
        },
        { timeoutMs: remainingMs(), intervalMs: 50 },
      );
    },
    async waitForAny(markers: readonly string[]): Promise<string> {
      return await runtime.waitFor(
        () => {
          const seen = lines.find((line) =>
            markers.some((marker) => line.startsWith(marker))
          );
          if (seen !== undefined) return seen;
          const error = failureMarker();
          if (error !== undefined) {
            throw new Error(`Rust transport growth leg failed: ${error}`);
          }
          return undefined;
        },
        { timeoutMs: remainingMs(), intervalMs: 50 },
      );
    },
    async send(command: string): Promise<void> {
      await stdin.write(new TextEncoder().encode(`${command}\n`));
    },
    count(marker: string): number {
      return lines.filter((line) => line.startsWith(marker)).length;
    },
    latestObservedIndex(): number | undefined {
      let latest: number | undefined;
      for (const line of lines) {
        if (line.startsWith("TRANSPORT_GROWTH_OBSERVED ")) {
          const value = Number(line.slice("TRANSPORT_GROWTH_OBSERVED ".length));
          if (Number.isFinite(value)) latest = value;
        }
      }
      return latest;
    },
  };

  try {
    try {
      await body(leg);
    } catch (cause) {
      const markers = lines.filter((line) =>
        !line.startsWith("TRANSPORT_GROWTH_OBSERVED ")
      );
      throw new Error(
        `${
          cause instanceof Error ? cause.message : String(cause)
        }\n--- growth leg markers ---\n${markers.join("\n")}`,
        { cause },
      );
    }
    const finished = lines.includes("TRANSPORT_GROWTH_DONE") ||
      lines.includes("TRANSPORT_GROWTH_CLOSED");
    if (!finished && !exited) {
      await leg.send("EXIT");
      await leg.waitFor("TRANSPORT_GROWTH_DONE");
    }
    await runtime.waitFor(() => (exited ? true : undefined), {
      timeoutMs: remainingMs(),
      intervalMs: 50,
    });
    const finalStatus = await status;
    assert(finalStatus.success, `Rust transport growth leg failed: ${output}`);
  } finally {
    try {
      await stdin.close();
    } catch {
      // stdin may already be closed once the process exited.
    }
    if (!exited) {
      try {
        Deno.kill(-child.pid, "SIGKILL");
      } catch {
        // the process may have exited between the check and the signal.
      }
    }
    await Promise.allSettled([status, drainSettled]);
  }
}

/** The one expected non-internal application route suffix shape. */
type Narrowed = { digest: string; sockets: BrokerConnection[] };

/**
 * Reads the subject's fresh (non-wide) admitted digest and its exact broker
 * socket, or `undefined` while the transition is still settling.
 */
async function narrowedState(
  runtime: Runtime,
  participantId: string,
  wideDigest: string,
  brokerServerId: string,
): Promise<Narrowed | undefined> {
  const digests = new Set(
    (await attachmentsFor(runtime, participantId)).map((item) =>
      item.contextDigest
    ),
  );
  const fresh = [...digests].filter((digest) => digest !== wideDigest);
  if (fresh.length !== 1) return undefined;
  const inventory = await readRuntimeBrokerInventory(runtime, {
    requiredServerIds: [brokerServerId],
  });
  const sockets = admittedConnections(inventory, new Set(fresh));
  if (sockets.length !== 1) return undefined;
  return { digest: fresh[0], sockets };
}

/** Whether one exact broker connection key is still present in the inventory. */
async function brokerHasKey(
  runtime: Runtime,
  brokerServerId: string,
  key: string,
): Promise<boolean> {
  const inventory = await readRuntimeBrokerInventory(runtime, {
    requiredServerIds: [brokerServerId],
  });
  return inventory.some((item) => brokerConnectionKey(item) === key);
}

/** The context digest field of one admitted broker connection. */
function digestOf(connection: BrokerConnection): string {
  return connection.authorizedUser.slice("trellis.auth.v1:".length).split(
    ":",
    1,
  )[0];
}

/** A diagnostic snapshot of every callout socket and the subject's sockets. */
async function subjectSocketReport(
  runtime: Runtime,
  participantId: string,
  brokerServerId: string,
): Promise<string> {
  const inventory = await readRuntimeBrokerInventory(runtime, {
    requiredServerIds: [brokerServerId],
  });
  const digests = new Set(
    (await attachmentsFor(runtime, participantId)).map((item) =>
      item.contextDigest
    ),
  );
  return JSON.stringify({
    subjectDigests: [...digests],
    subject: admittedConnections(inventory, digests).map((item) => ({
      key: brokerConnectionKey(item),
      digest: digestOf(item),
    })),
    callouts: inventory
      .filter((item) => item.authorizedUser.startsWith("trellis.auth.v1:"))
      .map((item) => ({
        key: brokerConnectionKey(item),
        digest: digestOf(item),
      })),
  });
}

/** The one-shot server-reply barrier type returned by the native gate. */ type HoldBarrier =
  ReturnType<
    ReturnType<Runtime["nativeTransportGate"]>["armResponseHold"]
  >;

/**
 * Awaits a held server reply, failing loudly on a timeout or a hold that matched
 * an unrelated request.
 */
async function heldOrFail(
  hold: HoldBarrier,
  label: string,
  expectedPrefix = "$JS.API.CONSUMER.INFO.",
): Promise<{ connectionId: number; requestSubject: string }> {
  const held = await Promise.race([
    hold.held,
    new Promise<never>((_, reject) =>
      setTimeout(
        () =>
          reject(
            new Error(`${label} never withheld a ${expectedPrefix} reply`),
          ),
        90_000,
      )
    ),
  ]);
  assert(
    held.requestSubject.startsWith(expectedPrefix),
    `${label} must target ${expectedPrefix}: ${held.requestSubject}`,
  );
  return held;
}

/** One prepared subject/target/observer setup shared by both cases. */
async function prepareSubjects(runtime: Runtime) {
  const subjectContract = participants.TransportGrowthSubject.participant;
  const targetContract = participants.TransportGrowthTarget.participant;

  // The target serves every capability through its generated handlers.
  await runtime.contracts.apply({
    deployment: TARGET_DEPLOYMENT,
    contract: targetContract,
  });
  const targetInstance = await runtime.services.createInstance({
    deployment: TARGET_DEPLOYMENT,
    name: "cs-target",
    contract: targetContract,
  });
  const target = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: targetContract,
    name: "cs-target",
    seed: targetInstance.seed,
  }).orThrow();
  const targetExit = target.wait().catch((error: unknown) => error);

  // The verified caller of every accepted `Advance`, in handler-entry order, so
  // the exact authorization context a real RPC carried is observable.
  const advanceCallers: { type: string; contextDigest: string | null }[] = [];
  let extendCalls = 0;
  const progress = { running: true, index: 0 };
  await target.handleAdvance(({ context }) => {
    const caller = context.caller;
    advanceCallers.push({
      type: caller.type,
      contextDigest: caller.type === "verified" ? caller.contextDigest : null,
    });
    return Result.ok({});
  });
  await target.handleExtend(() => {
    extendCalls += 1;
    return Result.ok({});
  });
  // A plain periodic Live source the subject observes across the forced
  // reduction, proving real outbound observation on the recovered attachment.
  await target.handleProgress(async ({ emit, signal }) => {
    while (!signal.aborted && progress.running) {
      progress.index += 1;
      await emit({ value: `${progress.index}` });
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
  });

  // The subject starts approved with `Extend` included, so its only initial
  // native attachment is the wider generation.
  await runtime.contracts.install({ contract: subjectContract });
  const requested = await runtime.contracts.requestApply({
    deployment: SUBJECT_DEPLOYMENT,
    contract: subjectContract,
  });
  if (requested.status !== "approval_required") {
    throw new Error(
      `expected an approval-required apply, got ${requested.status}`,
    );
  }
  await runtime.contracts.approveApply(requested.pendingId);
  const subjectInstance = await runtime.services.createInstance({
    deployment: SUBJECT_DEPLOYMENT,
    name: "cs-subject",
    contract: subjectContract,
  });
  const subjectId = subjectContract.identity;

  // The subject's inbound Live consumer for its served `liveprobe Watch` source.
  const observer = await runtime.connectClient({
    name: "cs-subject-live",
    contract: participants.LiveProbeCaller.participant,
  });

  return {
    subjectInstance,
    subjectId,
    target,
    targetExit,
    observer,
    advanceCallers,
    extendCalls: () => extendCalls,
    progress,
  };
}

/** Close every owning handle of one prepared setup. */
async function closeSubjects(
  ctx: Awaited<ReturnType<typeof prepareSubjects>>,
): Promise<void> {
  ctx.progress.running = false;
  await ctx.observer.connection.close().catch(() => undefined);
  await ctx.target.connection.close().catch(() => undefined);
  await ctx.targetExit;
}

/** Removes exactly the `Extend` atom from the subject's grant binding. */
async function reduceExtend(
  runtime: Runtime,
  subjectId: string,
): Promise<void> {
  const binding = await grantBinding(runtime, subjectId);
  const extendKey = atomKey(await rpcAtom("Extend"));
  assert(
    binding.grants.permissions.some((atom) => atomKey(atom) === extendKey),
    "the initial wider authority must include the Extend atom",
  );
  const kept = binding.grants.permissions.filter((atom) =>
    atomKey(atom) !== extendKey
  );
  assert(
    kept.length > 0 && kept.length < binding.grants.permissions.length,
    "the reduction must remove exactly Extend and keep the adopted authority",
  );
  await setPermissions(runtime, binding, kept);
}

Deno.test(
  "a no-survivor reduction recovers on one staged socket that becomes current",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const ctx = await prepareSubjects(runtime);
      const gate = runtime.nativeTransportGate();
      try {
        await withTransportGrowthLeg(
          runtime,
          ctx.subjectInstance.seed,
          180_000,
          async (leg) => {
            await leg.waitFor("TRANSPORT_GROWTH_ADVANCE_OK");
            const before = await attachmentsFor(runtime, ctx.subjectId);
            assertEquals(
              before.length,
              1,
              "a fresh subject has exactly one admitted attachment",
            );
            const logical = before[0].runtimeConnectionId;
            const wideDigest = before[0].contextDigest;

            // The initial attachment really serves the optional capability, so
            // it is the wider generation.
            await leg.send("AVAILABLE");
            await leg.waitFor("TRANSPORT_GROWTH_EXTEND_AVAILABLE");
            await leg.send("EXTEND");
            await leg.waitFor("TRANSPORT_GROWTH_EXTEND_OK");
            assertEquals(
              ctx.extendCalls(),
              1,
              "the wide attachment served Extend",
            );

            const wideSockets = admittedConnections(
              await readRuntimeBrokerInventory(runtime),
              new Set([wideDigest]),
            );
            assertEquals(
              wideSockets.length,
              1,
              "the wide generation must own exactly one physical socket",
            );
            const wideKey = brokerConnectionKey(wideSockets[0]);
            const brokerServerId = wideSockets[0].server;

            // The wide serving connection's real application-route
            // subscriptions, so the private stage can be proven not to serve
            // them before it is promoted.
            const wideConn = gate.connections().find((connection) =>
              connection.outboundContexts.some((out) =>
                out.context === wideDigest
              )
            );
            assert(
              wideConn !== undefined,
              "the wide generation must have carried a real signed request",
            );
            const appSubs = new Set(
              wideConn!.subs
                .filter((sub) =>
                  !sub.subject.startsWith("_INBOX.") &&
                  !sub.subject.startsWith("$")
                )
                .map((sub) => sub.subject),
            );
            assert(
              appSubs.size > 0,
              "the wide generation must serve application routes",
            );

            // Pause the private provisional stage's registry warm so its exact
            // socket can be observed before it serves; release it to finish the
            // automatic recovery.
            const hold = gate.armResponseHold("$JS.API.CONSUMER.INFO.");

            // Removing Extend force-removes the only admitted (wider)
            // attachment, leaving no already-admitted safe survivor.
            await reduceExtend(runtime, ctx.subjectId);

            const held = await heldOrFail(hold, "the private stage warm");
            assert(
              held.connectionId > 0,
              "the private stage must be a real proxied connection",
            );
            const stageConn = gate.connection(held.connectionId);
            assert(
              stageConn !== undefined && !stageConn.closed,
              "the private stage connection must be open while its warm is held",
            );
            const stageSubs = new Set(
              stageConn!.subs.map((sub) => sub.subject),
            );
            assertEquals(
              [...appSubs].filter((subject) => stageSubs.has(subject)),
              [],
              "the private stage must not serve application routes while held",
            );

            // The exact staged socket is admitted under the narrowed authority
            // and the wider socket is gone, on the same logical connection.
            const staged = await runtime.waitFor(
              async () =>
                await narrowedState(
                  runtime,
                  ctx.subjectId,
                  wideDigest,
                  brokerServerId,
                ),
              { timeoutMs: 60_000, intervalMs: 300 },
            );
            const stageKey = brokerConnectionKey(staged.sockets[0]);
            const stagedAttachments = await attachmentsFor(
              runtime,
              ctx.subjectId,
            );
            assertEquals(
              stagedAttachments.map((item) => item.contextDigest),
              [staged.digest],
              "only the narrowed attachment may remain",
            );
            assertEquals(
              stagedAttachments[0].runtimeConnectionId,
              logical,
              "the staged socket must stay on the same logical connection",
            );

            // Finish the recovery: the same staged physical connection must
            // become the serving current generation.
            await hold.release();
            const served = await runtime.waitFor(() => {
              const connection = gate.connection(held.connectionId);
              if (connection === undefined || connection.closed) {
                return undefined;
              }
              return connection.subs.some((sub) => appSubs.has(sub.subject))
                ? connection.id
                : undefined;
            }, { timeoutMs: 60_000, intervalMs: 100 });
            assertEquals(
              served,
              held.connectionId,
              "the staged socket itself must become the serving attachment",
            );
            const otherServing = gate.connections().filter((connection) =>
              !connection.closed && connection.id !== held.connectionId &&
              connection.subs.some((sub) => appSubs.has(sub.subject))
            );
            assertEquals(
              otherServing.map((connection) => connection.id),
              [],
              "no second connection may serve the application routes",
            );
            const settled = await narrowedState(
              runtime,
              ctx.subjectId,
              wideDigest,
              brokerServerId,
            );
            assertEquals(
              settled !== undefined
                ? brokerConnectionKey(settled.sockets[0])
                : undefined,
              stageKey,
              "the staged socket must remain the exact serving attachment",
            );
            assert(
              !(await brokerHasKey(runtime, brokerServerId, wideKey)),
              "the exact wider socket must be gone from the broker",
            );

            // Real inbound `liveprobe Watch`: served by the narrowed attachment
            // through a real LiveProbeCaller and closed with a confirmed remote
            // cleanup.
            const feed = await ctx.observer.watch({
              runId: crypto.randomUUID(),
              streamId: "stage",
            }).orThrow();
            const frames: bigint[] = [];
            const feedTask = (async () => {
              for await (const frame of feed) frames.push(frame.index);
            })().catch(() => undefined);
            await runtime.waitFor(
              () => frames.length > 0 ? true : undefined,
              { timeoutMs: 30_000, intervalMs: 50 },
            );
            const receipt = await feed.close().orThrow();
            assertEquals(
              receipt.remote,
              "confirmed",
              "the recovered attachment must confirm the Live close remotely",
            );
            assertEquals(
              receipt.cleanup,
              "complete",
              "the recovered Live close must report a complete remote cleanup",
            );
            await feedTask;

            // Real outbound `Progress` observation on the narrowed attachment.
            const observedBefore = leg.latestObservedIndex() ?? -1;
            await leg.send("OBSERVE");
            await runtime.waitFor(
              () =>
                (leg.latestObservedIndex() ?? -1) > observedBefore
                  ? true
                  : undefined,
              { timeoutMs: 30_000, intervalMs: 50 },
            );
            await leg.send("OBSERVE_CLOSE");
            await leg.waitFor("TRANSPORT_GROWTH_OBSERVED_CLOSED");

            // A fresh generated outbound `Advance` is accepted and the target
            // sees the newly installed narrowed context.
            const callsBefore = ctx.advanceCallers.length;
            await leg.send("ADVANCE");
            await runtime.waitFor(
              () => ctx.advanceCallers.length > callsBefore ? true : undefined,
              { timeoutMs: 30_000, intervalMs: 50 },
            );
            const seen = ctx.advanceCallers[ctx.advanceCallers.length - 1];
            assertEquals(seen.type, "verified");
            assertEquals(
              seen.contextDigest,
              staged.digest,
              "a real Advance must carry the narrowed installed context",
            );

            // `Extend` is refused by the narrowed authority and never reaches
            // the target.
            await leg.send("TRY_EXTEND");
            await leg.waitFor("TRANSPORT_GROWTH_EXTEND_REFUSED");
            assertEquals(
              ctx.extendCalls(),
              1,
              "the refused Extend must never reach the target",
            );

            // The recovered attachment is the only one, the same logical
            // connection, and no hidden candidate/renewal socket exists.
            const finalInventory = await readRuntimeBrokerInventory(runtime, {
              requiredServerIds: [brokerServerId],
            });
            assertEquals(
              admittedConnections(finalInventory, new Set([staged.digest]))
                .map(brokerConnectionKey),
              [stageKey],
              "no hidden candidate or renewal socket may exist",
            );
            assert(
              !finalInventory.some((item) =>
                brokerConnectionKey(item) === wideKey
              ),
              "the wider socket must stay reaped",
            );
            const finalAttachments = await attachmentsFor(
              runtime,
              ctx.subjectId,
            );
            assertEquals(
              finalAttachments.length,
              1,
              "the recovered attachment must be the only one",
            );
            assertEquals(
              finalAttachments[0].runtimeConnectionId,
              logical,
              "the logical connection identity must stay stable",
            );
            assertEquals(finalAttachments[0].contextDigest, staged.digest);

            await leg.send("EXIT");
            await leg.waitFor("TRANSPORT_GROWTH_DONE");
          },
        );
      } finally {
        await closeSubjects(ctx);
      }
    }, runtimeOptions);
  },
);

Deno.test(
  "ending runtime native ownership releases its broker socket despite retained client references",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const ctx = await prepareSubjects(runtime);
      try {
        await withTransportGrowthLeg(
          runtime,
          ctx.subjectInstance.seed,
          90_000,
          async (leg) => {
            await leg.waitFor("TRANSPORT_GROWTH_READY");
            const attachments = await attachmentsFor(runtime, ctx.subjectId);
            assertEquals(attachments.length, 1);
            const sockets = admittedConnections(
              await readRuntimeBrokerInventory(runtime),
              new Set([attachments[0].contextDigest]),
            );
            assertEquals(sockets.length, 1);
            const key = brokerConnectionKey(sockets[0]);
            const server = sockets[0].server;

            // The runtime owner closes logical scope while both it and the
            // fixture retain their actual client references.
            await leg.send("SHUTDOWN_NATIVE");
            await leg.waitFor("TRANSPORT_GROWTH_CLOSED_AND_WAITING");
            await leg.send("REFRESH_AFTER_SHUTDOWN");
            await leg.waitFor("TRANSPORT_GROWTH_REFRESH_REFUSED");
            await leg.send("PING");
            await leg.waitFor("TRANSPORT_GROWTH_PONG");
            await runtime.waitFor(
              async () =>
                await brokerHasKey(runtime, server, key) ? undefined : true,
              { timeoutMs: 5_000, intervalMs: 100 },
            );

            // Keep the process and retained client alive through real broker
            // reads, beyond a registry request timeout. Scope end must not
            // permit authorization renewal to resurrect a physical attachment.
            const until = Date.now() + 10_000;
            while (Date.now() < until) {
              const allAttachments = await attachmentsFor(
                runtime,
                ctx.subjectId,
              );
              const inventory = await readRuntimeBrokerInventory(runtime, {
                requiredServerIds: [server],
              });
              assertEquals(
                admittedConnections(
                  inventory,
                  new Set(allAttachments.map((item) => item.contextDigest)),
                ).map(brokerConnectionKey),
                [],
              );
              assert(
                !inventory.some((item) => brokerConnectionKey(item) === key),
              );
              await new Promise((resolve) => setTimeout(resolve, 250));
            }
            await leg.send("PING");
            await leg.waitFor("TRANSPORT_GROWTH_PONG", 2);
            await leg.send("EXIT");
            await leg.waitFor("TRANSPORT_GROWTH_DONE");
          },
          true,
        );
      } finally {
        await closeSubjects(ctx);
      }
    }, runtimeOptions);
  },
);

Deno.test(
  "dropping the logical client during a withheld stage warm releases only the provisional socket",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const ctx = await prepareSubjects(runtime);
      const gate = runtime.nativeTransportGate();
      try {
        await withTransportGrowthLeg(
          runtime,
          ctx.subjectInstance.seed,
          180_000,
          async (leg) => {
            await leg.waitFor("TRANSPORT_GROWTH_ADVANCE_OK");
            const before = await attachmentsFor(runtime, ctx.subjectId);
            assertEquals(before.length, 1);
            const wideDigest = before[0].contextDigest;

            // Prove the initial attachment is the wider generation.
            await leg.send("AVAILABLE");
            await leg.waitFor("TRANSPORT_GROWTH_EXTEND_AVAILABLE");

            const wideSockets = admittedConnections(
              await readRuntimeBrokerInventory(runtime),
              new Set([wideDigest]),
            );
            assertEquals(wideSockets.length, 1);
            const wideKey = brokerConnectionKey(wideSockets[0]);
            const brokerServerId = wideSockets[0].server;

            const wideConn = gate.connections().find((connection) =>
              connection.outboundContexts.some((out) =>
                out.context === wideDigest
              )
            );
            assert(wideConn !== undefined);
            const appSubs = new Set(
              wideConn!.subs
                .filter((sub) =>
                  !sub.subject.startsWith("_INBOX.") &&
                  !sub.subject.startsWith("$")
                )
                .map((sub) => sub.subject),
            );
            assert(appSubs.size > 0);

            // Withhold the private provisional stage's real registry warm reply.
            const hold = gate.armResponseHold("$JS.API.CONSUMER.INFO.");
            await reduceExtend(runtime, ctx.subjectId);
            const held = await heldOrFail(hold, "the private stage warm");
            const stageConn = gate.connection(held.connectionId);
            assert(
              stageConn !== undefined && !stageConn.closed,
              "the private stage connection must be open while held",
            );

            // The exact stage socket is admitted while no application intake is
            // serving on it, and the wider socket is gone.
            const staged = await runtime.waitFor(
              async () =>
                await narrowedState(
                  runtime,
                  ctx.subjectId,
                  wideDigest,
                  brokerServerId,
                ),
              { timeoutMs: 60_000, intervalMs: 300 },
            );
            const stageKey = brokerConnectionKey(staged.sockets[0]);
            const stageSubs = new Set(
              stageConn!.subs.map((sub) => sub.subject),
            );
            assertEquals(
              [...appSubs].filter((subject) => stageSubs.has(subject)),
              [],
              "the private stage must not serve application routes while held",
            );
            assert(
              !(await brokerHasKey(runtime, brokerServerId, wideKey)),
              "the exact wider socket must be gone from the broker",
            );

            // Drop the real logical client through its ordinary owners while the
            // reply is still withheld, and keep the fixture process alive.
            await leg.send("CLOSE_AND_WAIT");
            await leg.waitFor("TRANSPORT_GROWTH_CLOSED_AND_WAITING");

            // The process is alive before any broker absence is established.
            await leg.send("PING");
            await leg.waitFor("TRANSPORT_GROWTH_PONG");

            // Cancellation must release the exact provisional socket from the
            // broker, not merely close the process or the proxy. This is bounded
            // well below the withheld registry request's own timeout, so the
            // release can only come from the client drop and never from a warm
            // timeout that happens to fence the stage.
            const absenceUntil = Date.now() + 5_000;
            let stageGone = false;
            while (Date.now() < absenceUntil) {
              if (!(await brokerHasKey(runtime, brokerServerId, stageKey))) {
                stageGone = true;
                break;
              }
              await new Promise((resolve) => setTimeout(resolve, 250));
            }
            assert(
              stageGone,
              `the provisional socket ${stageKey} must be released by the client drop, not the warm timeout: ${await subjectSocketReport(
                runtime,
                ctx.subjectId,
                brokerServerId,
              )}`,
            );

            // The process is still alive after the client is gone.
            await leg.send("PING");
            await leg.waitFor("TRANSPORT_GROWTH_PONG");

            // A released late reply must not resurrect any candidate, serving
            // socket, or application work.
            await hold.release().catch(() => undefined);
            const settleUntil = Date.now() + 10_000;
            while (Date.now() < settleUntil) {
              const report = await subjectSocketReport(
                runtime,
                ctx.subjectId,
                brokerServerId,
              );
              const inventory = await readRuntimeBrokerInventory(runtime, {
                requiredServerIds: [brokerServerId],
              });
              const subjectDigests = new Set(
                (await attachmentsFor(runtime, ctx.subjectId)).map((item) =>
                  item.contextDigest
                ),
              );
              assertEquals(
                admittedConnections(inventory, subjectDigests)
                  .map(brokerConnectionKey),
                [],
                `no subject socket may exist after the client is released: ${report}`,
              );
              await new Promise((resolve) => setTimeout(resolve, 500));
            }
            await leg.send("PING");
            await leg.waitFor("TRANSPORT_GROWTH_PONG");
            await leg.send("EXIT");
            await leg.waitFor("TRANSPORT_GROWTH_DONE");
          },
        );
      } finally {
        await closeSubjects(ctx);
      }
    }, runtimeOptions);
  },
);

Deno.test(
  "stopping the logical client preempts provider intake still awaiting stage readiness",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const ctx = await prepareSubjects(runtime);
      const gate = runtime.nativeTransportGate();
      try {
        await withTransportGrowthLeg(
          runtime,
          ctx.subjectInstance.seed,
          180_000,
          async (leg) => {
            await leg.waitFor("TRANSPORT_GROWTH_ADVANCE_OK");
            const before = await attachmentsFor(runtime, ctx.subjectId);
            assertEquals(before.length, 1);
            const wideDigest = before[0].contextDigest;

            // Prove the initial attachment is the wider generation.
            await leg.send("AVAILABLE");
            await leg.waitFor("TRANSPORT_GROWTH_EXTEND_AVAILABLE");

            const wideSockets = admittedConnections(
              await readRuntimeBrokerInventory(runtime),
              new Set([wideDigest]),
            );
            assertEquals(wideSockets.length, 1);
            const wideKey = brokerConnectionKey(wideSockets[0]);
            const brokerServerId = wideSockets[0].server;

            const wideConn = gate.connections().find((connection) =>
              connection.outboundContexts.some((out) =>
                out.context === wideDigest
              )
            );
            assert(wideConn !== undefined);
            const appSubs = new Set(
              wideConn!.subs
                .filter((sub) =>
                  !sub.subject.startsWith("_INBOX.") &&
                  !sub.subject.startsWith("$")
                )
                .map((sub) => sub.subject),
            );
            assert(appSubs.size > 0);

            // Withhold the private provisional stage's real registry warm so its
            // exact socket can be observed before it serves.
            const warmHold = gate.armResponseHold("$JS.API.CONSUMER.INFO.");
            await reduceExtend(runtime, ctx.subjectId);
            const held = await heldOrFail(warmHold, "the private stage warm");
            const stageConn = gate.connection(held.connectionId);
            assert(
              stageConn !== undefined && !stageConn.closed,
              "the private stage connection must be open while held",
            );

            // The exact stage socket is admitted under the narrowed authority
            // and the wider socket is gone, on the same logical connection.
            const staged = await runtime.waitFor(
              async () =>
                await narrowedState(
                  runtime,
                  ctx.subjectId,
                  wideDigest,
                  brokerServerId,
                ),
              { timeoutMs: 60_000, intervalMs: 300 },
            );
            const stageKey = brokerConnectionKey(staged.sockets[0]);
            assert(
              !(await brokerHasKey(runtime, brokerServerId, wideKey)),
              "the exact wider socket must be gone from the broker",
            );

            let readinessHold: HoldBarrier | undefined;
            try {
              // Releasing the warm synchronously disarms its token before the
              // queued reply bytes reach the child, so the next
              // `$SYS.REQ.USER.INFO` on this exact connection is the
              // provider-readiness probe `ProviderIngress::start` awaits after
              // subscribing its application routes. Arming the hold before
              // awaiting the release therefore withholds the readiness reply,
              // leaving intake preparation in flight.
              const releasing = warmHold.release();
              readinessHold = gate.armResponseHold(
                "$SYS.REQ.USER.INFO",
                held.connectionId,
              );
              await releasing;
              await heldOrFail(
                readinessHold,
                "the stage provider readiness",
                "$SYS.REQ.USER.INFO",
              );

              // Intake is prepared but not published: the stage's exact socket
              // has its application routes subscribed and broker-live while the
              // readiness reply that would complete the barrier is withheld.
              const intakeConn = gate.connection(held.connectionId);
              assert(
                intakeConn !== undefined && !intakeConn.closed,
                "the stage socket must be open while readiness is withheld",
              );
              const intakeSubs = new Set(
                intakeConn!.subs.map((sub) => sub.subject),
              );
              assertEquals(
                [...appSubs].filter((subject) => !intakeSubs.has(subject)),
                [],
                "the stage must have prepared its application intake",
              );

              // Drop the real logical client through its ordinary owners while
              // intake preparation still awaits readiness, and keep the process
              // alive.
              await leg.send("CLOSE_AND_WAIT");
              await leg.waitFor("TRANSPORT_GROWTH_CLOSED_AND_WAITING");
              await leg.send("PING");
              await leg.waitFor("TRANSPORT_GROWTH_PONG");

              // Cancellation must preempt the in-flight preparation and release
              // the exact provisional socket from the broker well below the
              // readiness probe's own bound, so the release can only come from
              // the client drop and never from a readiness timeout.
              const absenceUntil = Date.now() + 5_000;
              let stageGone = false;
              while (Date.now() < absenceUntil) {
                if (!(await brokerHasKey(runtime, brokerServerId, stageKey))) {
                  stageGone = true;
                  break;
                }
                await new Promise((resolve) => setTimeout(resolve, 250));
              }
              assert(
                stageGone,
                `the provisional socket ${stageKey} must be released by the client drop, not the readiness timeout: ${await subjectSocketReport(
                  runtime,
                  ctx.subjectId,
                  brokerServerId,
                )}`,
              );

              // The process is still alive after the client is gone.
              await leg.send("PING");
              await leg.waitFor("TRANSPORT_GROWTH_PONG");
              await leg.send("EXIT");
              await leg.waitFor("TRANSPORT_GROWTH_DONE");
            } finally {
              // Release the barriers only after the cancellation assertion so a
              // withheld reply can never be mistaken for the client drop.
              await readinessHold?.release().catch(() => undefined);
              await warmHold.release().catch(() => undefined);
            }
          },
        );
      } finally {
        await closeSubjects(ctx);
      }
    }, runtimeOptions);
  },
);
