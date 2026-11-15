import assert from "node:assert/strict";
import { test } from "node:test";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { codecs } from "../packages/trellis/generated.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
} from "./browser_test_support.ts";

const participant = participants.ConfiguredService.participant;

test("an administrator sets a waiting service's settings from the link it printed", async () => {
  const services: Promise<{ stop(): Promise<void> }>[] = [];
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const key = await runtime.services.createInstance({
        name: "configured",
        deployment: "configured",
        contract: participant,
      });
      // The service has no endpoint or key yet, so it waits and prints a link.
      const connecting = TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: key.seed,
      }).orThrow();
      services.push(connecting);
      const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
      await runtime.waitFor(async () => {
        const view = await runtime.callAdminRpc("authConfigGet", { target });
        return view.instances[0]?.connectedAt;
      });

      const admin = await launchProfile(runtime);
      try {
        const page = await admin.newPage();
        await page.goto(
          `${runtime.trellisUrl}/console/admin/services/${key.deploymentId}#settings`,
          { waitUntil: "domcontentloaded" },
        );
        await completeConsoleEntry(page, BROWSER_ADMIN);
        const settings = page.getByRole("region", { name: "Settings" });
        await settings
          .getByText(
            "Its instances wait until these are set: API key, Endpoint.",
          )
          .waitFor();
        await settings.getByLabel("Endpoint").fill("https://one.example");
        await settings.getByLabel("API key").fill("k1");
        // A nullable secret takes plain text, not JSON.
        await settings.getByLabel("Signing key").fill("s1");
        await settings
          .getByRole("button", { name: "Save settings", exact: true })
          .click();
        await settings.getByText("Settings saved.").waitFor();
        // The secret is never shown back; its field only says it is set.
        const apiKey = settings.getByLabel("API key");
        assert.equal(await apiKey.inputValue(), "");
        assert.equal(
          await apiKey.getAttribute("placeholder"),
          "Set — type to replace",
        );
      } finally {
        await admin.close();
      }

      const service = await connecting;
      assert.equal(service.config.current.endpoint, "https://one.example");
      assert.equal(service.config.current.apiKey, "k1");
      assert.equal(service.config.current.signingKey, "s1");
    }, browserRuntimeOptions());
  } finally {
    // A failed step leaves the connecting service running. With the runtime
    // gone its wait ends, so stop it here or the file never exits.
    for (const connecting of services)
      await connecting.then(
        (service) => service.stop(),
        () => {},
      );
  }
});
