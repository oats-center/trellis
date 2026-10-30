import assert from "node:assert/strict";
import { test } from "node:test";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("services registered without a deployment each connect, and a second instance joins its own", async () => {
  await withTrellisRuntime(async (runtime) => {
    const provider = await runtime.registerService({
      name: "provider",
      contract: participants.Provider.participant,
    });
    // Echo serves the same API as Provider: Auth refuses that second
    // deployment until the administrator acknowledges the shared API.
    await assert.rejects(
      runtime.registerService({
        name: "echo",
        contract: participants.EchoProvider.participant,
      }),
      /requests would be split/,
    );
    const echo = await runtime.registerService({
      name: "echo",
      contract: participants.EchoProvider.participant,
      acknowledgeSharedApis: true,
    });
    assert.notEqual(echo.deploymentId, provider.deploymentId);
    const replica = await runtime.registerService({
      name: "provider replica",
      contract: participants.Provider.participant,
    });
    assert.equal(replica.deploymentId, provider.deploymentId);
    for (const [identity, participant, name] of [
      [provider, participants.Provider.participant, "provider"],
      [echo, participants.EchoProvider.participant, "echo"],
      [replica, participants.Provider.participant, "provider replica"],
    ] as const) {
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        name,
        seed: identity.seed,
      }).orThrow();
      await service.stop();
    }
  });
});
