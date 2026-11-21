import assert from "node:assert/strict";
import { test } from "node:test";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
} from "./browser_test_support.ts";
import {
  openConsoleAsRuntimeAdmin,
  seedServiceWithoutResources,
} from "./console_test_support.ts";

test("an administrator issues an enrollment secret, sees it once, and revokes it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedServiceWithoutResources(runtime, "enrolling");
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.consoleUrl}/console/admin/services/${seeded.deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      const section = page.getByRole("region", { name: "Enrollment" });
      await section.getByRole("button", { name: "Issue a secret" }).click();
      const form = section.getByRole("form", { name: "Issue secret" });
      await form.getByLabel("Title").fill("Production pods");
      await form.getByLabel("Instances at once").fill("3");
      await form.getByRole("button", { name: "Issue secret" }).click();

      const shown = section.locator("[data-enrollment-secret]");
      await shown.waitFor();
      const text = await shown.innerText();
      assert.match(text, /^trellis-enroll-[0-9A-Z]{26}-/);
      const { items } = await runtime.callAdminRpc(
        "authEnrollmentSecretsList",
        { deploymentId: seeded.deploymentId },
      );
      const [issued] = items;
      assert(issued);
      assert.equal(issued.title, "Production pods");
      assert.equal(issued.maxInstances, 3);
      assert(text.includes(issued.secretId));

      // Once dismissed, the secret is never shown again.
      await section.getByRole("button", { name: "Done" }).click();
      await shown.waitFor({ state: "detached" });
      const row = section.locator(`tr[data-secret-id="${issued.secretId}"]`);
      await row.getByText("Active", { exact: true }).waitFor();
      assert(!(await page.content()).includes(text));

      await row
        .getByRole("button", { name: "Actions for Production pods" })
        .click();
      await page.getByRole("button", { name: "Revoke", exact: true }).click();
      await section
        .getByRole("form", { name: "Revoke secret" })
        .getByRole("button", { name: "Revoke secret" })
        .click();
      await row.getByText("Revoked", { exact: true }).waitFor();
      const after = await runtime.callAdminRpc("authEnrollmentSecretsList", {
        deploymentId: seeded.deploymentId,
      });
      assert(after.items[0]?.revokedAt);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
