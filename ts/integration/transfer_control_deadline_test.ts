import { assert, assertEquals } from "@std/assert";
import { isErr, Result } from "@oatscenter/result";
import { TrellisService } from "@oatscenter/trellis/service";
import { TransferGrantSchema } from "@oatscenter/trellis";
import { Value } from "typebox/value";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

for (const exchange of ["activation", "cancellation"] as const) {
  Deno.test(`a healthy broker cannot extend a stalled transfer ${exchange} past its control budget`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      const identity = await runtime.registerService({
        name: "deadline-provider",
        contract,
      });
      const provider = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        name: "deadline-provider",
        seed: identity.seed,
      }).orThrow();
      const providerExit = provider.wait().catch((cause: unknown) => cause);
      const caller = await runtime.connectClient({
        name: "deadline-caller",
        contract: participants.Caller.participant,
        timeout: 1500,
      });
      let hold:
        | ReturnType<
          ReturnType<typeof runtime.nativeTransportGate>["armResponseHold"]
        >
        | undefined;
      let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
      try {
        await provider.handleEcho(({ input }) => Result.ok(input));
        await provider.handleUpload(async ({ input, op }) =>
          await op.complete(input).orThrow()
        );
        const store = await provider.store.files.open().orThrow();
        await store.put("deadline-bytes", new Uint8Array(8 * 1024 * 1024))
          .orThrow();
        await provider.handleDownload(async ({ input, context }) => {
          assert(context.caller.type === "verified");
          const transfer = await provider.createTransfer({
            direction: "receive",
            store: "files",
            key: "deadline-bytes",
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
        const gate = runtime.nativeTransportGate();
        let outcome: Promise<unknown>;
        let failed = false;
        let cancellationCause: unknown;
        if (exchange === "activation") {
          // Upload activation has no DATA before acceptance. Hold only the
          // provider's signed activation reply, not the connection or PONGs.
          hold = gate.armResponseHold(
            "transfer.v2.control.",
            undefined,
            (body) =>
              JSON.parse(new TextDecoder().decode(body)).type === "activated",
          );
          outcome = caller.upload({ value: "deadline" }).transfer(
            new Uint8Array([7]),
          )
            .start().take().then((result) => {
              failed = isErr(result);
            });
        } else {
          const response = await caller.download({ value: "deadline" })
            .orThrow();
          const grant = Value.Parse(TransferGrantSchema, response.transfer);
          assert(grant.direction === "receive");
          reader = (await caller.transfer(grant).stream().orThrow())
            .getReader();
          assert(!(await reader.read()).done);
          hold = gate.armResponseHold(
            grant.controlSubject,
            undefined,
            (body) =>
              JSON.parse(new TextDecoder().decode(body)).type === "cancelled",
          );
          outcome = reader.cancel().catch((cause) => {
            failed = true;
            cancellationCause = cause;
          });
        }
        let settled = false;
        void outcome.finally(() => {
          settled = true;
        }).catch(() => {});
        let held = false;
        void hold.held.then(() => {
          held = true;
        });
        await runtime.waitFor(() => held);
        // Unrelated real traffic on the same caller/provider sockets remains
        // usable while the particular control response is withheld.
        assertEquals(
          (await caller.echo({ value: "healthy" }).orThrow()).value,
          "healthy",
        );
        await runtime.waitFor(() => settled, { timeoutMs: 5000 });
        await outcome;
        if (exchange === "cancellation") {
          assert(
            String(cancellationCause).includes("remote cleanup is unconfirmed"),
          );
        }
        assert(
          failed,
          "the withheld control response must report a bounded failure",
        );
        assertEquals(
          (await caller.echo({ value: "still-healthy" }).orThrow()).value,
          "still-healthy",
        );
      } finally {
        await hold?.release();
        reader?.releaseLock();
        await caller.connection.close();
        await provider.stop();
        await providerExit;
      }
    }, { interruptibleNativeProxy: true });
  });
}
