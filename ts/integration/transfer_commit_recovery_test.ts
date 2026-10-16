import assert from "node:assert/strict";
import { test } from "node:test";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  type OperationRefData,
  decodeTransferGrant,
  type SendTransferGrant,
} from "@oatscenter/trellis";
import { isErr } from "@oatscenter/result";
import { TransferSession } from "../packages/trellis/transfer/session.ts";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

for (const cancelDuringCommit of [false, true]) {
  test(
    cancelDuringCommit
      ? "wire cancellation during a held final CAS never acknowledges cancellation of committed bytes"
      : "a lost final Operation CAS response recovers committed bytes while the provider stays alive",
    async () => {
      await withTrellisRuntime(
        async (runtime) => {
          const contract = participants.Provider.participant;
          const identity = await runtime.registerService({
            name: "commit-recovery",
            contract,
          });
          const provider = await TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
            participant: contract,
            name: "commit-recovery",
            seed: identity.seed,
          }).orThrow();
          const providerExit = provider.wait().catch((cause: unknown) => cause);
          const caller = await runtime.connectClient({
            name: "commit-recovery-caller",
            contract: apps.Caller.request,
          });
          const gate = runtime.nativeTransportGate();
          const writePrefix = "$KV.";
          let hold: ReturnType<typeof gate.armResponseHold> | undefined;
          let accepted: OperationRefData | undefined;
          let grant: SendTransferGrant | undefined;
          let wire: TransferSession | undefined;
          const signals: string[] = [];
          let entered = 0;
          const bytes = new Uint8Array([1, 2, 3, 4, 5, 6, 7]);
          try {
            await provider.handleUpload(async ({ input, transfer, op }) => {
              entered++;
              assert(transfer);
              const store = await provider.store.files.open().orThrow();
              await store
                .put(input.value, await transfer.stream().orThrow())
                .orThrow();
              return await op.complete(input).orThrow();
            });
            const connection = await runtime.waitFor(
              () =>
                gate
                  .connections()
                  .find(
                    (item) =>
                      !item.closed &&
                      item.subs.some((sub) => sub.subject.endsWith(".Upload")),
                  ) ?? false,
            );
            const openingSubject = connection.subs.find((sub) =>
              sub.subject.endsWith(".Upload"),
            )?.subject;
            assert(openingSubject);
            // Capture the grant sent to this real caller, not the private
            // Operation record (which also contains storage bookkeeping).
            hold = gate.armResponseHold(openingSubject, undefined, (body) => {
              const envelope: unknown = JSON.parse(
                new TextDecoder().decode(body),
              );
              if (
                !envelope ||
                typeof envelope !== "object" ||
                !Reflect.has(envelope, "transfer")
              )
                return false;
              const decoded = decodeTransferGrant(
                Reflect.get(envelope, "transfer"),
              ).orThrow();
              assert(decoded.direction === "send");
              grant = decoded;
              return true;
            });
            const uploading = caller
              .upload({ value: "committed-recovery" })
              .transfer(bytes)
              .onAccepted(({ snapshot }) => {
                accepted = { ...snapshot, deploymentId: identity.deploymentId };
              })
              .start()
              .take();
            let uploadEnded = false;
            void uploading.then(() => {
              uploadEnded = true;
            });
            // Get reads the durable public snapshot. The final upload CAS
            // makes it runnable before its held ACK lets the handler enter.
            {
              await Promise.race([
                hold.held,
                uploading.then((result) => {
                  throw isErr(result)
                    ? result
                    : new Error("upload settled before its held reply");
                }),
              ]);
              for (;;) {
                const releasing = hold.release();
                hold = gate.armResponseHold(writePrefix, connection.id);
                await releasing;
                await Promise.race([
                  hold.held,
                  uploading.then((result) => {
                    throw isErr(result)
                      ? result
                      : new Error("upload settled before its held reply");
                  }),
                ]);
                assert(
                  !uploadEnded,
                  "upload settled before its final write was held",
                );
                if (
                  accepted &&
                  (await caller.upload.resume(accepted).get().orThrow())
                    .state === "running"
                )
                  break;
              }
            }
            assert(accepted);
            assert(grant);
            assert.deepEqual(
              entered,
              0,
              "handler cannot enter before the final CAS response",
            );
            if (cancelDuringCommit) {
              assert(grant.direction === "send");
              const handle = caller.transfer(grant);
              const lease = await handle.transport.acquireFor(
                {
                  publish: [grant.controlSubject],
                  subscribe: [grant.signalSubject],
                },
                { deadlineMs: Date.now() + 5000 },
              );
              wire = new TransferSession(grant, lease, handle.auth, false);
              await wire.retainLocal();
              wire.subscriptions.push(
                lease.nc.subscribe(grant.signalSubject, {
                  callback: (error, message) => {
                    assert(!error);
                    signals.push(
                      JSON.parse(new TextDecoder().decode(message.data)).type,
                    );
                  },
                }),
              );
              const payload = new TextEncoder().encode(
                JSON.stringify({
                  format: "trellis.transfer.v2",
                  type: "control",
                  action: "cancel",
                  transferId: grant.transferId,
                  controlSeq: "2",
                  receivedSeq: "0",
                  consumedSeq: "0",
                  consumedBytes: "0",
                }),
              );
              lease.nc.publish(grant.controlSubject, payload, {
                headers: await wire.requestHeaders(
                  grant.controlSubject,
                  payload,
                  2n,
                  "control",
                ),
                reply: grant.signalSubject,
              });
              await lease.nc.flush();
            }
            // Keep the broker healthy and withhold just the final ACK until the
            // ordinary KV exchange budget expires. No fake persistence result.
            await runtime.waitFor(
              () => uploadEnded || signals.includes("cancelled"),
              { timeoutMs: 15_000 },
            );
            assert(
              !signals.includes("cancelled"),
              "durably committed upload cannot acknowledge cancellation",
            );
            assert(
              isErr(await uploading),
              "lost commitment confirmation must not report success",
            );

            await hold?.release();
            const recovered = await caller.upload
              .resume(accepted)
              .wait({
                observationSignal: AbortSignal.timeout(45_000),
              })
              .orThrow();
            assert.deepEqual(recovered.state, "completed");
            assert.deepEqual(entered, 1);
            const store = await provider.store.files.open().orThrow();
            assert.deepEqual(
              await (
                await store.get("committed-recovery").orThrow()
              )
                .bytes()
                .orThrow(),
              bytes,
            );
          } finally {
            await hold?.release();
            wire?.finish();
            await wire?.join();
            await provider.stop();
            await providerExit;
          }
        },
        { interruptibleNativeProxy: true },
      );
    },
  );
}
