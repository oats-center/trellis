import assert from "node:assert/strict";
import { test } from "node:test";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("an RPC mount rejects when the broker never confirms its subscription", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "mount-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        name: "mount-provider",
        seed: identity.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      const gate = runtime.nativeTransportGate();
      try {
        // The service is the only proxied participant; its admitted
        // generation is the open connection carrying subscriptions.
        const connection = await runtime.waitFor(
          () =>
            gate
              .connections()
              .filter((candidate) => !candidate.closed && candidate.subs.length)
              .at(-1) ?? false,
          { timeoutMs: 30_000 },
        );
        // Hold the broker's answer to the mount's flush, then lose the
        // connection: the subscription was never confirmed, so the mount
        // must fail rather than report a responder that may not exist.
        const hold = gate.holdNextPong(connection.id);
        const mounted = service
          .handleEcho(({ input }) => Result.ok(input))
          .then(
            () => undefined,
            (error: unknown) => error,
          );
        await hold.held;
        runtime.interruptNativeTransport();
        const outcome = await mounted;
        assert(outcome instanceof Error, `the mount resolved: ${outcome}`);
      } finally {
        runtime.restoreNativeTransport();
        await service.stop();
        await serviceExit;
      }
    },
    { interruptibleNativeProxy: true },
  );
});
