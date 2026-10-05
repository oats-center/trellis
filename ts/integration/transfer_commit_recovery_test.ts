import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { jetstream } from "@nats-io/jetstream";
import { Objm } from "@nats-io/obj";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  type OperationRefData,
  TransferGrantSchema,
} from "@oatscenter/trellis";
import { Value } from "typebox/value";
import { isErr } from "@oatscenter/result";
import { TransferSession } from "../packages/trellis/transfer/session.ts";
import { decodeResourceValue } from "../packages/trellis/kv.ts";
import { DurableOperationRecordSchema } from "../packages/trellis/session.ts";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

for (const cancelDuringCommit of [false, true]) {
  Deno.test(
    cancelDuringCommit
      ? "wire cancellation during a held final CAS never acknowledges cancellation of committed bytes"
      : "a lost final Operation CAS response recovers committed bytes while the provider stays alive",
    async () => {
      await withTrellisRuntime(async (runtime) => {
        const contract = participants.Provider.participant;
        const identity = await runtime.registerService({
          name: "commit-recovery",
          contract,
        });
        const provider = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: contract,
          name: "commit-recovery",
          seed: identity.seed,
        }).orThrow();
        const providerExit = provider.wait().catch((cause: unknown) => cause);
        const caller = await runtime.connectClient({
          name: "commit-recovery-caller",
          contract: participants.Caller.participant,
        });
        const privileged = await connect({
          servers: runtime.natsUrl,
          authenticator: credsAuthenticator(
            await Deno.readFile(join(
              runtime.workdir,
              "config/trellis/nats/creds/trellis-auth.creds",
            )),
          ),
        });
        const jsm = await jetstream(privileged).jetstreamManager();
        const gate = runtime.nativeTransportGate();
        let hold: ReturnType<typeof gate.armResponseHold> | undefined;
        let accepted: OperationRefData | undefined;
        let wire: TransferSession | undefined;
        const signals: string[] = [];
        let entered = 0;
        const bytes = new Uint8Array([1, 2, 3, 4, 5, 6, 7]);
        try {
          await provider.handleUpload(async ({ input, transfer, op }) => {
            entered++;
            assert(transfer);
            const store = await provider.store.files.open().orThrow();
            await store.put(input.value, await transfer.stream().orThrow())
              .orThrow();
            return await op.complete(input).orThrow();
          });
          const connection = await runtime.waitFor(() =>
            gate.connections().find((item) =>
              !item.closed &&
              item.subs.some((sub) => sub.subject.endsWith(".Upload"))
            ) ?? false
          );
          hold = gate.armResponseHold("$KV.", connection.id);
          const uploading = caller.upload({ value: "committed-recovery" })
            .transfer(bytes).onAccepted(({ snapshot }) => {
              accepted = snapshot;
            })
            .start().take();
          let uploadEnded = false;
          void uploading.then(() => {
            uploadEnded = true;
          });
          // Each held frame is a real broker ACK. Skip admission/progress ACKs
          // only after independently reading their actual persisted records.
          let durable: { transferGrant?: unknown; uploadStorageKey?: string };
          for (;;) {
            let observed: Awaited<typeof hold.held> | undefined;
            void hold.held.then((value) => {
              observed = value;
            });
            await runtime.waitFor(() => {
              assert(
                !uploadEnded,
                "upload settled before its final write was held",
              );
              return observed !== undefined;
            });
            assert(observed);
            const stream = await jsm.streams.find(observed.requestSubject);
            const stored = await jsm.streams.getMessage(stream, {
              last_by_subj: observed.requestSubject,
            });
            assert(stored);
            durable = await decodeResourceValue<
              { transferGrant?: unknown; uploadStorageKey?: string }
            >(
              {
                version: 1,
                migrations: {},
                codec: {
                  encode: (value: unknown) => value,
                  decode: (value) =>
                    Value.Parse(DurableOperationRecordSchema, value),
                },
              },
              {},
              stored.data,
            );
            if (
              durable.transferGrant &&
              typeof durable.transferGrant === "object" &&
              "committed" in durable.transferGrant
            ) break;
            const releasing = hold.release();
            hold = gate.armResponseHold("$KV.", connection.id);
            await releasing;
          }
          assert(accepted);
          assert(durable.uploadStorageKey);
          assertEquals(
            entered,
            0,
            "handler cannot enter before the final CAS response",
          );
          if (cancelDuringCommit) {
            const grant = Value.Parse(
              TransferGrantSchema,
              Value.Clean(
                TransferGrantSchema,
                structuredClone(durable.transferGrant),
              ),
            );
            assert(grant.direction === "send");
            const handle = caller.transfer(grant);
            const lease = await handle.transport.acquireFor({
              publish: [grant.controlSubject],
              subscribe: [grant.signalSubject],
            }, { deadlineMs: Date.now() + 5000 });
            wire = new TransferSession(grant, lease, handle.auth, false);
            await wire.retainLocal();
            wire.subscriptions.push(lease.nc.subscribe(grant.signalSubject, {
              callback: (error, message) => {
                assert(!error);
                signals.push(
                  JSON.parse(new TextDecoder().decode(message.data)).type,
                );
              },
            }));
            const payload = new TextEncoder().encode(JSON.stringify({
              format: "trellis.transfer.v2",
              type: "control",
              action: "cancel",
              transferId: grant.transferId,
              controlSeq: "2",
              receivedSeq: "0",
              consumedSeq: "0",
              consumedBytes: "0",
            }));
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
          assert(durable.uploadStorageKey);
          const objects = new Objm(privileged);
          let retained = false;
          for await (const bucket of objects.list()) {
            const value = await (await objects.open(bucket.bucket)).getBlob(
              durable.uploadStorageKey,
            );
            if (value === null) continue;
            assertEquals(Array.from(value), Array.from(bytes));
            retained = true;
            break;
          }
          assert(
            retained,
            "broker-staged bytes must survive lost commit confirmation",
          );
          await hold?.release();
          const recovered = await caller.upload.resume(accepted).wait({
            observationSignal: AbortSignal.timeout(45_000),
          }).orThrow();
          assertEquals(recovered.state, "completed");
          assertEquals(entered, 1);
          const store = await provider.store.files.open().orThrow();
          assertEquals(
            await (await store.get("committed-recovery").orThrow()).bytes()
              .orThrow(),
            bytes,
          );
        } finally {
          await hold?.release();
          wire?.finish();
          await wire?.join();
          await provider.stop();
          await providerExit;
          await privileged.close();
        }
      }, { interruptibleNativeProxy: true });
    },
  );
}
