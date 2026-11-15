import assert from "node:assert/strict";
import { test } from "node:test";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { codecs } from "../packages/trellis/generated.ts";
import { API as authApi } from "../packages/trellis/internal_sdk/generated/apis/auth/mod.js";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
} from "./browser_test_support.ts";
import { wireNatsGate } from "./console_test_support.ts";

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
        await settings
          .getByLabel("Endpoint", { exact: true })
          .fill("https://one.example");
        await settings.getByLabel("API key", { exact: true }).fill("k1");
        // A nullable secret takes plain text, not JSON.
        await settings.getByLabel("Signing key", { exact: true }).fill("s1");
        await settings
          .getByRole("button", { name: "Save settings", exact: true })
          .click();
        await settings.getByText("Settings saved.").waitFor();
        // The secret is never shown back; its field only says it is set.
        const apiKey = settings.getByLabel("API key", { exact: true });
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

test("an administrator overrides the only instance's settings, null included, without changing the service's", async () => {
  const services: { stop(): Promise<void> }[] = [];
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const key = await runtime.services.createInstance({
        name: "single",
        deployment: "single",
        contract: participant,
        config: {
          values: { endpoint: "https://one.example", region: "east" },
          secrets: { apiKey: "k1", signingKey: "s1" },
        },
      });
      const deploymentId = codecs.ulid.decode(key.deploymentId);
      const before = await runtime.callAdminRpc("authConfigGet", {
        target: { deploymentId },
      });
      assert.equal(before.instances.length, 1);
      const instanceId = before.instances[0]!.instanceId;
      const valueOf = (view: typeof before, name: string) => {
        const field = view.fields.find((item) => item.name === name);
        return field && { value: field.value, source: field.source };
      };

      const admin = await launchProfile(runtime);
      try {
        const page = await admin.newPage();
        const wired = await wireNatsGate(page);
        try {
          await page.goto(
            `${runtime.trellisUrl}/console/admin/services/${key.deploymentId}#settings`,
            { waitUntil: "domcontentloaded" },
          );
          await completeConsoleEntry(page, BROWSER_ADMIN);
          const settings = page.getByRole("region", { name: "Settings" });
          const region = settings.getByLabel("Region", { exact: true });
          await runtime.waitFor(
            async () => (await region.inputValue()) === "east",
          );
          const save = settings.getByRole("button", {
            name: "Save settings",
            exact: true,
          });

          // A draft typed for the whole service, then the only instance is
          // chosen while its settings are still on the way.
          await region.fill("everywhere");
          const held = wired.gate.armNextReply(
            authApi.actions["rpc:Config.Get"].subject,
          );
          await settings
            .getByRole("combobox")
            .selectOption({ value: instanceId });
          await held;
          // Saving now would write the service's draft for every instance,
          // so nothing can be edited or saved until the instance loads.
          assert.equal(
            await save.isDisabled(),
            true,
            "Save was offered before the instance's settings loaded",
          );
          assert.equal(await region.isDisabled(), true);
          await save.click({ timeout: 1_000 }).catch(() => {});
          wired.releaseToPage();
          await settings.getByText("From the service").first().waitFor();
          await runtime.waitFor(() => region.isEnabled());
          assert.equal(
            await region.inputValue(),
            "east",
            "the service's draft was saved or carried over to the instance",
          );
          assert.equal(await save.isDisabled(), true);

          // Null is stored for the instance, unlike Clear, which inherits.
          await settings
            .getByRole("button", { name: "Set Region to null", exact: true })
            .click();
          await settings.getByText("Region set to null.").waitFor();
          await settings
            .getByRole("button", {
              name: "Set Signing key to null",
              exact: true,
            })
            .click();
          await settings.getByText("Signing key set to null.").waitFor();
          await settings
            .getByLabel("Endpoint", { exact: true })
            .fill("https://two.example");
          await save.click();
          await settings.getByText("Settings saved.").waitFor();
          assert.equal(await region.inputValue(), "");
          assert.equal(await region.getAttribute("placeholder"), "null");
        } finally {
          wired.gate.dispose();
        }
      } finally {
        await admin.close();
      }

      const after = await runtime.callAdminRpc("authConfigGet", {
        target: { deploymentId },
      });
      assert.equal(after.revision, before.revision);
      assert.deepEqual(valueOf(after, "region"), {
        value: "east",
        source: "deployment",
      });
      assert.deepEqual(valueOf(after, "endpoint"), {
        value: "https://one.example",
        source: "deployment",
      });
      const instance = await runtime.callAdminRpc("authConfigGet", {
        target: { deploymentId, instanceId },
      });
      assert.deepEqual(valueOf(instance, "region"), {
        value: null,
        source: "instance",
      });
      assert.deepEqual(valueOf(instance, "endpoint"), {
        value: "https://two.example",
        source: "instance",
      });

      // The instance itself reads its own overrides, the null secret too.
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: key.seed,
      }).orThrow();
      services.push(service);
      assert.equal(service.config.current.endpoint, "https://two.example");
      assert.equal(service.config.current.region, null);
      assert.equal(service.config.current.signingKey, null);
      assert.equal(service.config.current.apiKey, "k1");
    }, browserRuntimeOptions());
  } finally {
    for (const service of services) await service.stop();
  }
});
