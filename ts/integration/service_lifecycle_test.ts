/**
 * Service lifecycle across an already-closed transport.
 *
 * Stopping a service whose logical transport is already closed must still run
 * its logical cleanup — terminate operation intake, release queued generation
 * pins, and clear recovery scans — rather than early-returning on the physical
 * socket state. The stop must settle, not hang on a dead connection.
 */

import { test } from "node:test";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "a service stops cleanly after its transport already closed",
  { timeout: 60_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      await runtime.contracts.apply({ contract });
      const instance = await runtime.services.createInstance({
        name: "lifecycle-provider",
        contract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        name: "lifecycle-provider",
        seed: instance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);

      // Register operation intake so stop has logical work to clean up.
      await service.handleEcho(({ input }) =>
        Promise.resolve(Result.ok(input)),
      );
      await service.handleWork(() =>
        Promise.resolve(Result.ok({ value: "ok" })),
      );

      // Close the logical transport first: stop must not early-return on the
      // already-closed socket before doing its logical cleanup.
      await service.connection.close();

      await service.stop();
      await service.stop();
      await serviceExit;
    });
  },
);
