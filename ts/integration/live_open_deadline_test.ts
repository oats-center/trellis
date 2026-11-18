/** A Live opening is never sent after its deadline, first send or resend. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { TransportError } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const WATCH = apis.runtime.API.actions["live:Watch"].subject;

test("a Live opening resend that wakes after the caller's timeout is never sent", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const gate = runtime.nativeTransportGate();
      const openingsSent = () =>
        gate
          .connections()
          .flatMap((connection) => connection.outboundContexts)
          .filter((publication) => publication.subject === WATCH).length;
      const identity = await runtime.registerService({
        name: "live-deadline-provider",
        contract: participants.Provider.participant,
      });
      const client = await runtime.connectClient({
        name: "live-deadline-caller",
        contract: apps.Caller.request,
        timeout: 3_000,
      });
      let stopService: (() => Promise<void>) | undefined;
      try {
        // With no provider the broker answers no-responders and the opening is
        // resent after 100, 200, 400, and 800 ms; its last resend waits
        // 1000 ms and would go out at about 2.5 s. A caller whose event loop
        // is busy from 2 s until after its 3 s timeout wakes with that resend
        // overdue.
        const started = Date.now();
        const late = client.runtimeWatch({});
        await new Promise((resolve) => setTimeout(resolve, 2_000));
        const sentBeforeStall = openingsSent();
        assert(sentBeforeStall >= 2, "the opening was resending");
        while (Date.now() < started + 3_200);
        const outcome = await late;
        assert(outcome.isErr());
        assert(outcome.error instanceof TransportError, String(outcome.error));
        assert.equal(outcome.error.code, "trellis.live.setup_timeout");

        const service = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          seed: identity.seed,
        }).orThrow();
        stopService = async () => {
          await service.stop();
          await service.wait();
        };
        await service.handleWatch(async ({ emit }) => {
          await emit({ value: "ready" }).orThrow();
        });

        // A later opening on the same connection reaches the provider, so
        // anything the late opening had sent passed the transport before it.
        const ready = await client.runtimeWatch({}).orThrow();
        try {
          const event = await ready[Symbol.asyncIterator]().next();
          assert(!event.done);
          assert.deepEqual(event.value, { value: "ready" });
        } finally {
          await ready[Symbol.asyncDispose]();
        }
        assert.equal(
          openingsSent() - sentBeforeStall,
          1,
          "an opening was sent after the caller's timeout",
        );
      } finally {
        await client.connection.close();
        await stopService?.();
      }
    },
    { interruptibleNativeProxy: true },
  );
});
