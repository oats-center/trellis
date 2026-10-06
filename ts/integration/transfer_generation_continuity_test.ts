/**
 * Live upload and download continuity across automatic transport generations.
 *
 * A real public upload transfer is held open while the service's authority
 * grows. Growth must adopt a new generation under the same logical connection,
 * and the transfer accepted on the original generation must finish exactly once
 * with the exact bytes, then clean up.
 */

import { assert, assertEquals } from "@std/assert";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  Result,
  TransferGrantSchema,
  TrellisClient,
} from "@oatscenter/trellis";
import { Value } from "typebox/value";
import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import type { OperationRefData } from "@oatscenter/trellis";
import { isErr } from "@oatscenter/result";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
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

Deno.test(
  "an in-flight upload transfer finishes with exact bytes across automatic growth",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract = participants.Provider.participant;
      const providerId = providerContract.identity;

      // 1. Deploy the provider with its optional resource declined.
      await runtime.contracts.install({ contract: providerContract });
      const requested = await runtime.contracts.requestApply({
        contract: providerContract,
      });
      if (requested.status === "approval_required") {
        await runtime.contracts.approveApply(requested.pendingId, {
          excludeResources: ["extras"],
        });
      }
      const instance = await runtime.services.createInstance({
        name: "transfer-generation-provider",
        contract: providerContract,
      });
      let service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: providerContract,
        name: "transfer-generation-provider",
        seed: instance.seed,
      }).orThrow();
      let serviceExit = service.wait().catch((error: unknown) => error);

      // The source, not the post-commit handler, holds the active data plane.
      let releaseUpload = false;
      let releaseCancelled = false;
      let sourceHeld = false;
      const progress: number[] = [];
      let uploadStarted = 0;
      await service.handleUpload(async ({ input, op, transfer, signal }) => {
        if (signal.aborted) return;
        uploadStarted += 1;
        assert(transfer, "an active upload must have its accepted transfer");
        const body = await transfer.stream().orThrow();
        const store = await service.store.files.open().orThrow();
        await store.put(input.value, body).orThrow();
        return await op.complete(input).orThrow();
      });

      const caller = await runtime.connectClient({
        name: "transfer-generation-caller",
        contract: participants.Caller.participant,
        timeout: 120_000,
      });

      try {
        const [before] = await attachmentsFor(runtime, providerId);
        assert(before, "the provider must have a physical attachment");
        const logical = before.runtimeConnectionId;
        const physical = before.connectionId;

        // 3. Start a real upload transfer and hold it while the service grows.
        const bytes = Uint8Array.from(
          { length: 8 * 1024 * 1024 },
          (_, index) => index % 251,
        );
        const source = (async function* () {
          for (let offset = 0; offset < bytes.length; offset += 256 * 1024) {
            if (offset === 5 * 1024 * 1024) {
              sourceHeld = true;
              await runtime.waitFor(() => releaseUpload, { timeoutMs: 90_000 });
            }
            yield bytes.subarray(offset, offset + 256 * 1024);
          }
        })();
        const uploading = caller.upload({ value: "held-bytes" })
          .transfer(source)
          .onTransfer(({ transfer }) => {
            progress.push(transfer.transferredBytes);
          })
          .start()
          .orThrow();
        uploading.catch(() => undefined);
        let uploadEnded = false;
        void uploading.finally(() => {
          uploadEnded = true;
        }).catch(() => undefined);
        await runtime.waitFor(() => {
          assert(!uploadEnded, "upload ended before source hold");
          return sourceHeld;
        });
        await runtime.waitFor(() => progress.some((value) => value > 0));
        assertEquals(
          uploadStarted,
          0,
          "handler cannot run before source EOF and commit",
        );

        // Admit cancellation's upload on the same original attachment. Its
        // consumed progress proves storage started before the source is held.
        let cancelledHeld = false;
        let cancelledConsumed = false;
        let acceptedCancelled: OperationRefData | undefined;
        const cancelledSource = (async function* () {
          yield bytes.subarray(0, 1024 * 1024);
          cancelledHeld = true;
          await runtime.waitFor(() => releaseCancelled, { timeoutMs: 90_000 });
          yield bytes.subarray(1024 * 1024);
        })();
        const cancellingUpload = caller.upload({ value: "cancelled-bytes" })
          .transfer(cancelledSource)
          .onAccepted(({ snapshot }) => {
            acceptedCancelled = snapshot;
          })
          .onTransfer(({ transfer }) => {
            if (transfer.transferredBytes > 0) cancelledConsumed = true;
          })
          .start().take();
        let cancellingEnded = false;
        void cancellingUpload.finally(() => {
          cancellingEnded = true;
        });
        await runtime.waitFor(() => {
          assert(!cancellingEnded, "upload ended before cancellation hold");
          return cancelledHeld && cancelledConsumed;
        });

        // 4. Grow authority: the service adopts a wider generation.
        await runtime.contracts.apply({ contract: providerContract });
        const adopted = await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, providerId)).filter(
            (item) => item.runtimeConnectionId === logical,
          );
          return items.length >= 2 ? items : false;
        }, { timeoutMs: 90_000 });
        assert(
          adopted.some((item) => item.connectionId !== physical),
          "authority growth must adopt a new physical generation",
        );

        // 5. Finish the held transfer: exact bytes, delivered exactly once.
        releaseUpload = true;
        const operation = await uploading;
        const terminal = await operation.wait().orThrow();
        assertEquals(terminal.terminal.state, "completed");
        assertEquals(terminal.transferred.size, bytes.length);
        const digest = `SHA-256=${
          btoa(
            String.fromCharCode(
              ...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)),
            ),
          ).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "")
        }`;
        assertEquals(terminal.transferred.digest, digest);
        assertEquals(
          terminal.terminal.transfer?.transferredBytes,
          bytes.length,
        );
        assert(progress.some((value) => value > 0));
        for (let index = 1; index < progress.length; index += 1) {
          assert(
            progress[index] >= progress[index - 1],
            "durable progress must not regress",
          );
        }
        const entry = await service.store.files.waitFor("held-bytes")
          .orThrow();
        assertEquals(await entry.bytes().orThrow(), bytes);
        assertEquals(uploadStarted, 1);
        assertEquals(service.connection.status.phase, "connected");
        assert(
          (await attachmentsFor(runtime, providerId)).some((item) =>
            item.connectionId === physical
          ),
          "the held cancellation upload still owns the draining attachment",
        );

        // Public business cancellation while the upload source has not ended
        // must not run the normal handler or expose an object in its store.
        try {
          assert(acceptedCancelled);
          const cancelledOperation = caller.upload.resume(
            acceptedCancelled,
          );
          await cancelledOperation.cancel().orThrow();
          releaseCancelled = true;
          assert(
            isErr(await cancellingUpload),
            "cancelled DATA cannot report commit",
          );
          assertEquals(
            (await cancelledOperation.wait().orThrow()).state,
            "cancelled",
          );
          assertEquals(
            uploadStarted,
            1,
            "cancelled upload cannot execute business work",
          );
          const cancellationStore = await service.store.files.open().orThrow();
          assert((await cancellationStore.get("cancelled-bytes")).isErr());
          // Observe prompt retirement, not eventual grant/context expiry.
          // Settlement joins the backend before releasing this pinned lease;
          // attachment retirement is the available public lifecycle evidence.
          await runtime.waitFor(
            async () =>
              !(await attachmentsFor(runtime, providerId)).some((item) =>
                item.connectionId === physical
              ),
          );
        } finally {
          releaseCancelled = true;
        }

        // Lose the provider's physical transport before source EOF. Ordinary
        // reconnect plus invocation replay must replace, not append to, staging.
        let interruptedHeld = false;
        let releaseInterrupted = false;
        let interruptedProgress = false;
        let interruptedRef: OperationRefData | undefined;
        let positiveProgressRevision = 0;
        let releaseSmall = false;
        const invocationId = ulid();
        const interruptedSource = (async function* () {
          yield bytes.subarray(0, 2 * 1024 * 1024);
          interruptedHeld = true;
          await runtime.waitFor(() => releaseInterrupted);
          yield bytes.subarray(2 * 1024 * 1024);
        })();
        const interrupted = caller.upload({ value: "recovered-bytes" })
          .transfer(interruptedSource)
          .onAccepted(({ snapshot }) => {
            interruptedRef = snapshot;
          })
          .onTransfer(({ transfer, snapshot }) => {
            if (transfer.transferredBytes > 0) {
              interruptedProgress = true;
              positiveProgressRevision = snapshot.revision;
            }
          })
          .start(undefined, { invocationId }).take();
        try {
          let interruptedEnded = false;
          void interrupted.finally(() => {
            interruptedEnded = true;
          });
          await runtime.waitFor(() => {
            assert(!interruptedEnded, "upload ended before interruption hold");
            return interruptedHeld;
          });
          await runtime.waitFor(() => {
            assert(!interruptedEnded, "upload ended before persisted progress");
            return interruptedProgress;
          });
          assertEquals(uploadStarted, 1);
          await service.connection.close();
          await service.stop();
          await serviceExit;
          releaseInterrupted = true;
          assert(isErr(await interrupted));
          service = await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: providerContract,
            name: "transfer-generation-provider",
            seed: instance.seed,
          }).orThrow();
          serviceExit = service.wait().catch((error: unknown) => error);
          await service.handleUpload(
            async ({ input, op, transfer, signal }) => {
              if (signal.aborted) return;
              uploadStarted += 1;
              assert(transfer);
              const store = await service.store.files.open().orThrow();
              await store.put(input.value, await transfer.stream().orThrow())
                .orThrow();
              if (input.value === "small-bytes") {
                await runtime.waitFor(() => releaseSmall);
              }
              return await op.complete(input).orThrow();
            },
          );
          assert(interruptedRef);
          const interruptedOperation = caller.upload.resume(interruptedRef);
          // The old owner's lease can legitimately remain live after disconnect.
          // Observe fenced recovery resetting transfer progress before replay,
          // rather than assuming reconnect instantly grants a new upload attempt.
          await runtime.waitFor(async () => {
            const snapshot = await interruptedOperation.get().orThrow();
            assert(
              snapshot.state !== "failed" && snapshot.state !== "cancelled" &&
                snapshot.state !== "completed",
              `interrupted upload terminated before lease-governed recovery: ${
                JSON.stringify(snapshot)
              }`,
            );
            return snapshot.revision > positiveProgressRevision &&
              snapshot.transfer === undefined;
          }, {
            timeoutMs: 40_000,
            intervalMs: 250,
          });
          const recovered = await caller.upload({ value: "recovered-bytes" })
            .transfer(bytes).start(undefined, { invocationId }).orThrow();
          const recoveredTerminal = await recovered.wait().orThrow();
          assertEquals(recoveredTerminal.terminal.state, "completed");
          assertEquals(recoveredTerminal.transferred.size, bytes.length);
          assertEquals(recoveredTerminal.transferred.digest, digest);
          assertEquals(
            recoveredTerminal.terminal.transfer?.transferredBytes,
            bytes.length,
          );
          const recoveryStore = await service.store.files.open().orThrow();
          const recoveredEntry = await recoveryStore.get(
            "recovered-bytes",
          ).orThrow();
          assertEquals(await recoveredEntry.bytes().orThrow(), bytes);
          assertEquals(
            uploadStarted,
            2,
            "only committed transfers execute handlers",
          );

          // A sub-threshold upload must still deliver its final Transfer event,
          // including running snapshots without business progress.
          const smallBytes = new TextEncoder().encode("tiny-up");
          let smallTransferred = 0;
          const smallUpload = await caller.upload({ value: "small-bytes" })
            .transfer(smallBytes)
            .onTransfer(({ transfer }) => {
              smallTransferred = transfer.transferredBytes;
              if (smallTransferred === smallBytes.length) releaseSmall = true;
            })
            .start().orThrow();
          await runtime.waitFor(() => smallTransferred === smallBytes.length);
          releaseSmall = true;
          const smallTerminal = await smallUpload.wait().orThrow();
          assertEquals(smallTerminal.terminal.state, "completed");
          assertEquals(smallTerminal.transferred.size, smallBytes.length);
          assertEquals(
            smallTerminal.terminal.transfer?.transferredBytes,
            smallBytes.length,
          );
          const smallEntry = await recoveryStore.get("small-bytes").orThrow();
          assertEquals(
            Array.from(await smallEntry.bytes().orThrow()),
            Array.from(smallBytes),
          );
          assertEquals(smallEntry.info.size, smallBytes.length);
          assertEquals(
            smallTerminal.transferred.digest,
            smallEntry.info.digest?.replace(/=+$/, ""),
          );
        } finally {
          releaseInterrupted = true;
          releaseSmall = true;
        }
      } finally {
        releaseUpload = true;
        releaseCancelled = true;
        await caller.connection.close().catch(() => undefined);
        await service.stop();
        await serviceExit;
      }
    }, {
      authorization: {
        contextLifetimeSeconds: 76,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 46,
      },
    });
  },
);

Deno.test("download endAck retires the last caller and provider generation leases", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerContract = participants.Provider.participant;
    const targetContract = participants.TransportGrowthTarget.participant;
    const callerContract = participants.DownloadGrowthCaller.participant;
    await runtime.contracts.install({ contract: providerContract });
    const requested = await runtime.contracts.requestApply({
      contract: providerContract,
    });
    assertEquals(requested.status, "approval_required");
    if (requested.status !== "approval_required") {
      throw new Error("missing provider approval");
    }
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeResources: ["extras"],
    });
    const providerInstance = await runtime.services.createInstance({
      name: "download-ack-provider",
      contract: providerContract,
    });
    const provider = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: providerContract,
      name: "download-ack-provider",
      seed: providerInstance.seed,
    }).orThrow();
    const providerExit = provider.wait().catch((error: unknown) => error);
    const deployment = "download-ack-growth-target";
    try {
      await runtime.contracts.apply({ deployment, contract: targetContract });
      const targetInstance = await runtime.services.createInstance({
        deployment,
        name: "download-ack-growth-target",
        contract: targetContract,
      });
      const growthTarget = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: targetContract,
        name: "download-ack-growth-target",
        seed: targetInstance.seed,
      }).orThrow();
      const targetExit = growthTarget.wait().catch((error: unknown) => error);
      try {
        await growthTarget.handleExtend(() => Result.ok({}));
        const callerKey = await runtime.registerClient({
          name: "download-ack-caller",
          contract: callerContract,
        });
        await runtime.ensurePortalConsentPolicy(callerContract.identity, []);
        const auth = runtime.clientAuth(callerKey);
        const caller = await TrellisClient.connect({
          trellisUrl: runtime.trellisUrl,
          participant: callerContract,
          name: "download-ack-caller",
          auth: auth.auth,
          onAuthRequired: auth.onAuthRequired,
        }).orThrow();
        let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
        try {
          const bytes = Uint8Array.from(
            { length: 8 * 1024 * 1024 },
            (_, index) => index % 251,
          );
          const store = await provider.store.files.open().orThrow();
          await store.put("download-ack", bytes).orThrow();
          await provider.handleDownload(async ({ input, context }) => {
            assert(context.caller.type === "verified");
            const transfer = await provider.createTransfer({
              direction: "receive",
              store: "files",
              key: "download-ack",
              sessionKey: context.caller.sessionKey,
              connectionId: context.caller.connectionId,
              contextDigest: context.caller.contextDigest,
              permission: context.permission,
              requiredCapabilities: context.requiredCapabilities,
              inboxPrefix: context.inboxPrefix,
              expiresInMs: 60_000,
            }).orThrow();
            return Result.ok({ ...input, transfer });
          });
          const [oldCaller] = await attachmentsFor(
            runtime,
            callerContract.identity,
          );
          const [oldProvider] = await attachmentsFor(
            runtime,
            providerContract.identity,
          );
          assert(oldCaller && oldProvider);
          const download = await caller.download({ value: "download-ack" })
            .orThrow();
          const grant = Value.Parse(TransferGrantSchema, download.transfer);
          assert(grant.direction === "receive");
          reader = (await caller.transfer(grant).stream().orThrow())
            .getReader();
          const first = await reader.read();
          assert(!first.done && first.value.length > 0);
          const chunks = [first.value];
          const grants = await runtime.callAdminRpc("authGrantsList", {
            participantId: callerContract.identity,
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
          const initial = grants.items.find((item) =>
            item.participantId === callerContract.identity
          );
          assert(initial);
          const extendAtom = {
            action: "call",
            target: await encodePermissionTargetWasm({
              kind: "apiSurface",
              api: "runtime-trellis.transport_growth@v1",
              surface: "rpc",
              name: "Extend",
            }),
          };
          await runtime.callAdminRpc("authGrantsSet", {
            expectedRevision: initial.revision,
            expiresAt: initial.expiresAt,
            grants: {
              format: initial.grants.format,
              permissions: [...initial.grants.permissions, extendAtom],
            },
            idempotencyKey: crypto.randomUUID(),
            installedRevision: initial.installedRevision,
            ownerId: initial.ownerId,
            ownerKind: initial.ownerKind,
            participantId: initial.participantId,
            platformPrivileges: initial.platformPrivileges,
          });
          await runtime.contracts.apply({ contract: providerContract });
          for (
            const [participantId, old] of [
              [callerContract.identity, oldCaller],
              [providerContract.identity, oldProvider],
            ] as const
          ) {
            await runtime.waitFor(async () => {
              const items = (await attachmentsFor(runtime, participantId))
                .filter((item) =>
                  item.runtimeConnectionId === old.runtimeConnectionId
                );
              return items.some((item) =>
                item.connectionId === old.connectionId
              ) &&
                items.some((item) => item.connectionId !== old.connectionId);
            });
          }
          // No other requests or observation sessions own either old generation.
          // Stop consumption across growth; resume only after both are draining.
          for (;;) {
            const chunk = await reader.read();
            if (chunk.done) break;
            chunks.push(chunk.value);
          }
          const received = new Uint8Array(
            chunks.reduce((size, chunk) => size + chunk.length, 0),
          );
          let offset = 0;
          for (const chunk of chunks) {
            received.set(chunk, offset);
            offset += chunk.length;
          }
          assertEquals(received, bytes);
          const digest = `SHA-256=${
            btoa(
              String.fromCharCode(
                ...new Uint8Array(
                  await crypto.subtle.digest("SHA-256", received),
                ),
              ),
            )
              .replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "")
          }`;
          assertEquals(grant.info.digest, digest);
          assert(
            Date.parse(grant.expiresAt) > Date.now() + 10_000,
            "retirement must be observed before grant expiry",
          );
          await runtime.waitFor(async () => {
            const callerItems = await attachmentsFor(
              runtime,
              callerContract.identity,
            );
            const providerItems = await attachmentsFor(
              runtime,
              providerContract.identity,
            );
            return !callerItems.some((item) =>
              item.connectionId === oldCaller.connectionId
            ) &&
              !providerItems.some((item) =>
                item.connectionId === oldProvider.connectionId
              );
          });
        } finally {
          await reader?.cancel().catch(() => undefined);
          reader?.releaseLock();
          await caller.connection.close();
        }
      } finally {
        await growthTarget.stop();
        await targetExit;
      }
    } finally {
      await provider.stop();
      await providerExit;
    }
  }, {
    authorization: {
      contextLifetimeSeconds: 76,
      refreshLeadSeconds: 15,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 46,
    },
  });
});
