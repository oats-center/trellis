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
  startedServices,
} from "./browser_test_support.ts";

const participant = participants.ConfiguredService.participant;

test("one save of plain and secret settings is one revision, and an instance that rejects it says why", async () => {
  const services = startedServices();
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const key = await runtime.services.createInstance({
        name: "checked",
        deployment: "checked",
        contract: participant,
        config: {
          values: { endpoint: "https://one.example" },
          secrets: { apiKey: "k1" },
        },
      });
      const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
      const service = await services.add(
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant,
          seed: key.seed,
          signal: services.signal,
        }).orThrow(),
      );
      await service.config.check((candidate, { reject }) => {
        if (!candidate.endpoint.startsWith("https://"))
          reject(["endpoint"], "must be an https URL");
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
        const instances = settings.getByRole("list", { name: "Instances" });
        await instances.getByText("Uses the latest settings").waitFor();

        const endpoint = settings.getByLabel("Endpoint", { exact: true });
        await endpoint.fill("http://plain.example");
        await settings.getByLabel("API key", { exact: true }).fill("k2");
        await settings.getByLabel("Signing key", { exact: true }).fill("s2");
        await settings
          .getByRole("button", { name: "Save settings", exact: true })
          .click();
        await settings.getByText("Settings saved.").waitFor();
        const history = await runtime.callAdminRpc("authConfigRevisionsList", {
          target,
        });
        assert.deepEqual(
          history.items.map(({ revision, changed, changedSecrets }) => ({
            revision,
            changed,
            changedSecrets,
          })),
          [
            {
              revision: 2,
              changed: ["endpoint"],
              changedSecrets: ["apiKey", "signingKey"],
            },
            { revision: 1, changed: ["endpoint"], changedSecrets: ["apiKey"] },
          ],
        );

        // The instance refuses the revision as a whole, and the page says
        // which field and why while it keeps rereading the service.
        await settings
          .getByText(
            "rejected the latest settings for Endpoint: must be an https URL.",
            { exact: false },
          )
          .waitFor();
        await instances.getByText("Rejected the latest settings").waitFor();
        assert.equal(service.config.current.endpoint, "https://one.example");
        assert.equal(service.config.current.apiKey, "k1");

        await endpoint.fill("https://two.example");
        await settings
          .getByRole("button", { name: "Save settings", exact: true })
          .click();
        await settings.getByText("Settings saved.").waitFor();
        await instances.getByText("Uses the latest settings").waitFor();
        assert.equal(
          await settings
            .getByText("rejected the latest settings", { exact: false })
            .count(),
          0,
        );
        assert.equal(service.config.current.endpoint, "https://two.example");
        assert.equal(service.config.current.apiKey, "k2");
      } finally {
        await admin.close();
      }
    }, browserRuntimeOptions());
  } finally {
    await services.stop();
  }
});
