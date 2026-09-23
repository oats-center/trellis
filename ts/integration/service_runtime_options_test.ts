import { TrellisService } from "@oatscenter/trellis/service";
import { assertEquals, assertGreater } from "@std/assert";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type { LoggerLike } from "../packages/trellis/globals.ts";
import {
  connectTrellisServiceWithRuntimeDeps,
  type TrellisServiceConnectArgs,
} from "../packages/trellis/service/runtime/service.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("service connect honors telemetry opt-out and runtime logger", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "runtime-options",
      contract: participants.Provider.participant,
    });
    let telemetryInitialized = false;
    let childLoggerCalls = 0;
    const noop = () => {};
    const logger: LoggerLike = {
      child: () => {
        childLoggerCalls += 1;
        return logger;
      },
      trace: noop,
      debug: noop,
      info: noop,
      warn: noop,
      error: noop,
    };

    const service = (await connectTrellisServiceWithRuntimeDeps({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
      telemetry: false,
      runtime: { log: logger },
    }, {
      initTelemetry: () => {
        telemetryInitialized = true;
      },
    })).orThrow();

    try {
      assertEquals(telemetryInitialized, false);
      assertGreater(childLoggerCalls, 0);
    } finally {
      await service.stop();
    }
  });
});

Deno.test("service connect options keep the flat required shape", () => {
  const participant = participants.Provider.participant;
  const valid: TrellisServiceConnectArgs<typeof participant> = {
    participant,
    trellisUrl: "http://localhost:3000",
    seed: "seed",
    telemetry: false,
    runtime: { log: false, timeout: 1, noResponderRetry: { maxAttempts: 1 } },
  };
  void valid;

  // @ts-expect-error seed is required at the top level.
  const missingSeed: TrellisServiceConnectArgs<typeof participant> = {
    participant,
    trellisUrl: "http://localhost:3000",
  };
  void missingSeed;

  const nestedIdentity: TrellisServiceConnectArgs<typeof participant> = {
    participant,
    trellisUrl: "http://localhost:3000",
    // @ts-expect-error nested identity compatibility is intentionally unsupported.
    identity: { seed: "seed" },
  };
  void nestedIdentity;

  const positionalCompatibilityIsUnsupported = () => {
    // @ts-expect-error positional compatibility overloads are intentionally unsupported.
    return TrellisService.connect(participant, { seed: "seed" });
  };
  void positionalCompatibilityIsUnsupported;
});
