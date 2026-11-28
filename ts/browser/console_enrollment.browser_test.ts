import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";

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
      assert.match(text, /^trellis-enroll-E[0-9A-Z]{26}-/);
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

test("an administrator edits and revokes a secret past the first page, and a title edit keeps its exact expiry", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedServiceWithoutResources(runtime, "many-secrets");
    // Auth lists the oldest first, 200 to a page, so the last one issued is
    // on the second page.
    for (let batch = 0; batch < 10; batch += 1) {
      await Promise.all(
        Array.from({ length: 20 }, (_, index) =>
          runtime.callAdminRpc("authEnrollmentSecretsIssue", {
            deploymentId: seeded.deploymentId,
            title: `Earlier ${batch * 20 + index}`,
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        ),
      );
    }
    // Expires three days from now at a time of day other than midnight.
    const expiresAt = new Date(
      Math.floor(Date.now() / 1000) * 1000 + 3 * 86_400_000,
    ).toISOString();
    const { secret } = await runtime.callAdminRpc(
      "authEnrollmentSecretsIssue",
      {
        deploymentId: seeded.deploymentId,
        title: "Last issued",
        expiresAt: codecs.timestamp.decode(expiresAt),
        idempotencyKey: codecs.ulid.decode(ulid()),
      },
    );
    const read = async () => {
      let cursor: string | undefined;
      do {
        const listed = await runtime.callAdminRpc("authEnrollmentSecretsList", {
          deploymentId: seeded.deploymentId,
          page: { limit: 200, ...(cursor ? { cursor } : {}) },
        });
        const found = listed.items.find(
          (item) => item.secretId === secret.secretId,
        );
        if (found) return found;
        cursor = listed.page.nextCursor;
      } while (cursor);
      assert.fail("the secret is listed");
    };

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.consoleUrl}/console/admin/services/${seeded.deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      const section = page.getByRole("region", { name: "Enrollment" });
      const row = section.locator(`tr[data-secret-id="${secret.secretId}"]`);
      await row
        .getByRole("button", { name: "Actions for Last issued" })
        .click();
      await page.getByRole("button", { name: "Edit", exact: true }).click();
      const form = section.getByRole("form", { name: "Save" });
      await form.getByLabel("Title").fill("Renamed");
      await form.getByRole("button", { name: "Save" }).click();
      await row.getByText("Renamed", { exact: true }).waitFor();
      const edited = await read();
      assert.equal(edited.title, "Renamed");
      assert.equal(Date.parse(edited.expiresAt ?? ""), Date.parse(expiresAt));

      await row.getByRole("button", { name: "Actions for Renamed" }).click();
      await page.getByRole("button", { name: "Revoke", exact: true }).click();
      await section
        .getByRole("form", { name: "Revoke secret" })
        .getByRole("button", { name: "Revoke secret" })
        .click();
      await row.getByText("Revoked", { exact: true }).waitFor();
      assert((await read()).revokedAt);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
