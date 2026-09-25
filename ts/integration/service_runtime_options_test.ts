import { assertEquals, assertGreater } from "@std/assert";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type { LoggerLike } from "../packages/trellis/globals.ts";
import { connectTrellisServiceWithRuntimeDeps } from "../packages/trellis/service/runtime/service.ts";
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
