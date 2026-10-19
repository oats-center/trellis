/**
 * Live upload and download continuity across automatic transport generations.
 *
 * A real public upload transfer is held open while the service's authority
 * grows. Growth must adopt a new generation under the same logical connection,
 * and the transfer accepted on the original generation must finish exactly once
 * with the exact bytes, then clean up.
 */

import { connectNativeClient } from "@oatscenter/trellis/native";
import assert from "node:assert/strict";
import { test } from "node:test";
import { TrellisService } from "@oatscenter/trellis/service";
import { Result, decodeTransferGrant } from "@oatscenter/trellis";
import type { OperationRefData } from "@oatscenter/trellis";
import { isErr } from "@oatscenter/result";
import { ulid } from "ulid";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";
import { readRuntimeServiceAttachments } from "./_support/broker_inventory.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

async function attachmentsFor(runtime: Runtime, participantId: string) {
  return await readRuntimeServiceAttachments(runtime, participantId);
}

test("an in-flight upload transfer finishes with exact bytes across automatic growth", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const providerContract = participants.TransferGrowthProvider.participant;
      const providerId = providerContract.identity;

      // Deploy the transfer provider with its optional API capability withheld.
      await runtime.contracts.install({ contract: providerContract });
      await runtime.contracts.apply({
        contract: providerContract,
      });
      const instance = await runtime.services.createInstance({
        name: "transfer-generation-provider",
        contract: providerContract,
      });
      const capabilityId = "runtime-trellis.transport_growth@v1::extend";
      const initialGrants = await runtime.callAdminRpc(
        "authCapabilityGrantsList",
        { capabilityId },
      );
      const providerGrant = initialGrants.items[0];
      assert(providerGrant);
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: providerGrant.grantId,
        expectedRevision: providerGrant.revision,
        idempotencyKey: ulid(),
      });
      let service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
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
        contract: apps.Caller.request,
        timeout: 120_000,
      });

      try {
        const [before] = await attachmentsFor(runtime, providerId);
        assert(before, "the provider must have a physical attachment");
        const logical = before.runtimeId;
        const physical = before.attachmentId;
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
        const uploading = caller
          .upload({ value: "held-bytes" })
          .transfer(source)
          .onTransfer(({ transfer }) => {
            progress.push(transfer.transferredBytes);
          })
          .start()
          .orThrow();
        uploading.catch(() => undefined);
        let uploadEnded = false;
        void uploading
          .finally(() => {
            uploadEnded = true;
          })
          .catch(() => undefined);
        await runtime.waitFor(() => {
          assert(!uploadEnded, "upload ended before source hold");
          return sourceHeld;
        });
        await runtime.waitFor(() => progress.some((value) => value > 0));
        assert.deepEqual(
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
        const cancellingUpload = caller
          .upload({ value: "cancelled-bytes" })
          .transfer(cancelledSource)
          .onAccepted(({ snapshot }) => {
            acceptedCancelled = {
              id: snapshot.id,
              service: snapshot.service,
              operation: snapshot.operation,
              deploymentId: instance.deploymentId,
            };
          })
          .onTransfer(({ transfer }) => {
            if (transfer.transferredBytes > 0) cancelledConsumed = true;
          })
          .start()
          .take();
        let cancellingEnded = false;
        void cancellingUpload.finally(() => {
          cancellingEnded = true;
        });
        await runtime.waitFor(() => {
          assert(!cancellingEnded, "upload ended before cancellation hold");
          return cancelledHeld && cancelledConsumed;
        });

        // 4. Grow authority: the service adopts a wider generation.
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: providerGrant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        const adopted = await runtime.waitFor(
          async () => {
            const items = (await attachmentsFor(runtime, providerId)).filter(
              (item) => item.runtimeId === logical,
            );
            return items.length >= 2 ? items : false;
          },
          { timeoutMs: 90_000 },
        );
        assert(
          adopted.some((item) => item.attachmentId !== physical),
          "authority growth must adopt a new physical generation",
        );

        // 5. Finish the held transfer: exact bytes, delivered exactly once.
        releaseUpload = true;
        const operation = await uploading;
        const terminal = await operation.wait().orThrow();
        assert.deepEqual(terminal.terminal.state, "completed");
        assert.deepEqual(terminal.transferred.size, bytes.length);
        const digest = `SHA-256=${btoa(
          String.fromCharCode(
            ...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)),
          ),
        )
          .replaceAll("+", "-")
          .replaceAll("/", "_")
          .replace(/=+$/, "")}`;
        assert.deepEqual(terminal.transferred.digest, digest);
        assert.deepEqual(
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
        const entry = await service.store.files.waitFor("held-bytes").orThrow();
        assert.deepEqual(await entry.bytes().orThrow(), bytes);
        assert.deepEqual(uploadStarted, 1);
        assert.deepEqual(service.connection.status.phase, "connected");
        assert(
          (await attachmentsFor(runtime, providerId)).some(
            (item) => item.attachmentId === physical,
          ),
          "the held cancellation upload still owns the draining attachment",
        );

        // Public business cancellation while the upload source has not ended
        // must not run the normal handler or expose an object in its store.
        try {
          assert(acceptedCancelled);
          const cancelledOperation = caller.upload.resume(acceptedCancelled);
          await cancelledOperation.cancel().orThrow();
          releaseCancelled = true;
          assert(
            isErr(await cancellingUpload),
            "cancelled DATA cannot report commit",
          );
          assert.deepEqual(
            (await cancelledOperation.wait().orThrow()).state,
            "cancelled",
          );
          assert.deepEqual(
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
              !(await attachmentsFor(runtime, providerId)).some(
                (item) => item.attachmentId === physical,
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
        const interrupted = caller
          .upload({ value: "recovered-bytes" })
          .transfer(interruptedSource)
          .onAccepted(({ snapshot }) => {
            interruptedRef = {
              id: snapshot.id,
              service: snapshot.service,
              operation: snapshot.operation,
              deploymentId: instance.deploymentId,
            };
          })
          .onTransfer(({ transfer, snapshot }) => {
            if (transfer.transferredBytes > 0) {
              interruptedProgress = true;
              positiveProgressRevision = snapshot.revision;
            }
          })
          .start(undefined, { invocationId })
          .take();
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
          assert.deepEqual(uploadStarted, 1);
          await service.connection.close();
          await service.stop();
          await serviceExit;
          releaseInterrupted = true;
          assert(isErr(await interrupted));
          service = await TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
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
              await store
                .put(input.value, await transfer.stream().orThrow())
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
          await runtime.waitFor(
            async () => {
              const snapshot = await interruptedOperation.get().orThrow();
              assert(
                snapshot.state !== "failed" &&
                  snapshot.state !== "cancelled" &&
                  snapshot.state !== "completed",
                `interrupted upload terminated before lease-governed recovery: ${JSON.stringify(
                  snapshot,
                )}`,
              );
              return (
                snapshot.revision > positiveProgressRevision &&
                snapshot.transfer === undefined
              );
            },
            {
              timeoutMs: 40_000,
              intervalMs: 250,
            },
          );
          const recovered = await caller
            .upload({ value: "recovered-bytes" })
            .transfer(bytes)
            .start(undefined, { invocationId })
            .orThrow();
          const recoveredTerminal = await recovered.wait().orThrow();
          assert.deepEqual(recoveredTerminal.terminal.state, "completed");
          assert.deepEqual(recoveredTerminal.transferred.size, bytes.length);
          assert.deepEqual(recoveredTerminal.transferred.digest, digest);
          assert.deepEqual(
            recoveredTerminal.terminal.transfer?.transferredBytes,
            bytes.length,
          );
          const recoveryStore = await service.store.files.open().orThrow();
          const recoveredEntry = await recoveryStore
            .get("recovered-bytes")
            .orThrow();
          assert.deepEqual(await recoveredEntry.bytes().orThrow(), bytes);
          assert.deepEqual(
            uploadStarted,
            2,
            "only committed transfers execute handlers",
          );

          // A sub-threshold upload must still deliver its final Transfer event,
          // including running snapshots without business progress.
          const smallBytes = new TextEncoder().encode("tiny-up");
          let smallTransferred = 0;
          const smallUpload = await caller
            .upload({ value: "small-bytes" })
            .transfer(smallBytes)
            .onTransfer(({ transfer }) => {
              smallTransferred = transfer.transferredBytes;
              if (smallTransferred === smallBytes.length) releaseSmall = true;
            })
            .start()
            .orThrow();
          await runtime.waitFor(() => smallTransferred === smallBytes.length);
          releaseSmall = true;
          const smallTerminal = await smallUpload.wait().orThrow();
          assert.deepEqual(smallTerminal.terminal.state, "completed");
          assert.deepEqual(smallTerminal.transferred.size, smallBytes.length);
          assert.deepEqual(
            smallTerminal.terminal.transfer?.transferredBytes,
            smallBytes.length,
          );
          const smallEntry = await recoveryStore.get("small-bytes").orThrow();
          assert.deepEqual(
            Array.from(await smallEntry.bytes().orThrow()),
            Array.from(smallBytes),
          );
          assert.deepEqual(smallEntry.info.size, smallBytes.length);
          assert.deepEqual(
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
    },
    {
      authorization: {
        contextLifetimeSeconds: 80,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 46,
      },
    },
  );
});

test("download endAck retires the last caller and provider generation leases", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const providerContract = participants.TransferGrowthProvider.participant;
      const targetContract = participants.TransportGrowthTarget.participant;
      const callerContract = apps.DownloadGrowthCaller.request;
      await runtime.contracts.install({ contract: providerContract });
      await runtime.contracts.apply({
        contract: providerContract,
      });
      const providerInstance = await runtime.services.createInstance({
        name: "download-ack-provider",
        contract: providerContract,
      });
      const capabilityId = "runtime-trellis.transport_growth@v1::extend";
      const providerGrants = await runtime.callAdminRpc(
        "authCapabilityGrantsList",
        { capabilityId },
      );
      const providerGrant = providerGrants.items[0];
      assert(providerGrant);
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: providerGrant.grantId,
        expectedRevision: providerGrant.revision,
        idempotencyKey: ulid(),
      });
      const provider = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
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
          trellisUrl: runtime.oauthIssuer,
          participant: targetContract,
          name: "download-ack-growth-target",
          seed: targetInstance.seed,
        }).orThrow();
        const targetExit = growthTarget.wait().catch((error: unknown) => error);
        try {
          await growthTarget.handleExtend(() => Result.ok({}));
          const login = await runtime.registerClient({
            name: "download-ack-caller",
            contract: callerContract,
          });
          const ownUser = await runtime.callAdminRpc("authUsersResolve", {});
          const callerGrants = await runtime.callAdminRpc(
            "authCapabilityGrantsList",
            { principalId: ownUser.user.principalId, capabilityId },
          );
          const callerGrant = callerGrants.items[0];
          assert(callerGrant);
          await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
            grantId: callerGrant.grantId,
            expectedRevision: callerGrant.revision,
            idempotencyKey: ulid(),
          });
          // Renew the original consented credential after setup reduction, before
          // physical admission: revocation retires an already admitted session.
          const session = (await login.session.refresh()).orThrow();
          const caller = await connectNativeClient({
            ...login,
            session,
            request: callerContract,
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
              const transfer = await provider
                .createTransfer({
                  direction: "receive",
                  store: "files",
                  key: "download-ack",
                  sessionKey: context.caller.sessionPublicKey,
                  connectionId: context.caller.runtimeId,
                  contextDigest: context.caller.authorityDigest,
                  permission: context.permission,
                  requiredCapabilities: context.requiredCapabilities,
                  inboxPrefix: context.inboxPrefix,
                  expiresInMs: 60_000,
                })
                .orThrow();
              return Result.ok({ ...input, transfer });
            });
            const [oldProvider] = await attachmentsFor(
              runtime,
              providerContract.identity,
            );
            assert(oldProvider);
            const download = await caller
              .download({ value: "download-ack" })
              .orThrow();
            const grant = decodeTransferGrant(download.transfer).orThrow();
            assert(grant.direction === "receive");
            const callerAttachments = async () => {
              const page = await runtime.callAdminRpc("authConnectionsList", {
                state: "admitted",
                principalId: callerGrant.principalId,
                page: { limit: 100 },
              });
              return page.items.filter(
                (item) => item.sessionPublicKey === grant.consumer.sessionKey,
              );
            };
            const [oldCaller] = await callerAttachments();
            assert(oldCaller);
            reader = (
              await caller.transfer(grant).stream().orThrow()
            ).getReader();
            const first = await reader.read();
            assert(!first.done && first.value.length > 0);
            const chunks = [first.value];

            await runtime.callAdminRpc("authCapabilityGrantsGrant", {
              principalId: callerGrant.principalId,
              capabilityId,
              expectedRevision: 0n,
              idempotencyKey: ulid(),
            });
            await runtime.callAdminRpc("authCapabilityGrantsGrant", {
              principalId: providerGrant.principalId,
              capabilityId,
              expectedRevision: 0n,
              idempotencyKey: ulid(),
            });
            // Observe both promotions within this grant's actual validity window.
            // Reserve the final ten seconds for endAck-driven retirement, not expiry.
            const promotionDeadline = Date.parse(grant.expiresAt) - 10_000;
            await Promise.all(
              (
                [
                  [callerAttachments, oldCaller],
                  [
                    () => attachmentsFor(runtime, providerContract.identity),
                    oldProvider,
                  ],
                ] as const
              ).map(async ([attachments, old]) => {
                await runtime
                  .waitFor(
                    async () => {
                      const items = await attachments();
                      return (
                        items.some(
                          (item) => item.attachmentId === old.attachmentId,
                        ) &&
                        items.some(
                          (item) => item.attachmentId !== old.attachmentId,
                        )
                      );
                    },
                    { timeoutMs: Math.max(1, promotionDeadline - Date.now()) },
                  )
                  .catch(async (cause: unknown) => {
                    const observed = await attachments();
                    throw new Error(
                      `Expected overlap for ${old.attachmentId}; observed ${observed.map((item) => `${item.attachmentId}:${item.state}`).join(", ")}`,
                      { cause },
                    );
                  });
              }),
            );
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
            assert.deepEqual(received, bytes);
            const digest = `SHA-256=${btoa(
              String.fromCharCode(
                ...new Uint8Array(
                  await crypto.subtle.digest("SHA-256", received),
                ),
              ),
            )
              .replaceAll("+", "-")
              .replaceAll("/", "_")
              .replace(/=+$/, "")}`;
            assert.deepEqual(grant.info.digest, digest);
            assert(
              Date.parse(grant.expiresAt) > Date.now() + 10_000,
              "retirement must be observed before grant expiry",
            );
            await runtime.waitFor(async () => {
              const callerItems = await callerAttachments();
              const providerItems = await attachmentsFor(
                runtime,
                providerContract.identity,
              );
              return (
                !callerItems.some(
                  (item) => item.attachmentId === oldCaller.attachmentId,
                ) &&
                !providerItems.some(
                  (item) => item.attachmentId === oldProvider.attachmentId,
                )
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
    },
    {
      authorization: {
        contextLifetimeSeconds: 80,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 46,
      },
    },
  );
});
