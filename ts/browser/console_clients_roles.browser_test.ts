import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { API as authApi } from "../packages/trellis/internal_sdk/generated/apis/auth/mod.js";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";

test("Console creates, reloads, disables and deletes an OAuth client and creates a capability role", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsole(page, runtime, BROWSER_ADMIN);
      const clientId = `console-development-${ulid()}`;
      await page.goto(`${runtime.consoleUrl}/console/admin/clients`);
      await page.getByRole("button", { name: "New app", exact: true }).click();
      await page.getByLabel("App ID").fill(clientId);
      await page.getByLabel("Name", { exact: true }).fill(clientId);
      await page
        .getByRole("textbox", { name: "Return address 1", exact: true })
        .fill("http://localhost:49371/callback");
      await page.getByLabel(/Local development/).check();
      await page.getByRole("button", { name: "Save app", exact: true }).click();
      await page.getByText("App saved.", { exact: true }).waitFor();
      await page.reload();
      const row = page.getByRole("row").filter({ hasText: clientId });
      await row.getByRole("button", { name: "Edit", exact: true }).click();
      assert.equal(
        await page
          .getByRole("textbox", { name: "Return address 1", exact: true })
          .inputValue(),
        "http://localhost:49371/callback",
      );
      page.once("dialog", (dialog) => dialog.accept());
      await page.getByRole("button", { name: "Turn off", exact: true }).click();
      await row.getByText("Disabled", { exact: true }).waitFor();
      await row.getByRole("button", { name: "Edit", exact: true }).click();
      page.once("dialog", (dialog) => dialog.accept());
      await page
        .getByRole("button", { name: "Delete app", exact: true })
        .click();
      await page.getByText("App deleted.", { exact: true }).waitFor();
      assert.equal(await row.count(), 0);

      const title = `Console role ${ulid()}`;
      await page.goto(`${runtime.consoleUrl}/console/admin/roles/new`);
      await page.getByLabel("Title", { exact: true }).fill(title);
      const auth = page.getByRole("group", { name: "Auth", exact: true });
      const account = auth.getByRole("checkbox", { name: /^Your account/ });
      const profile = auth.getByRole("checkbox", { name: /See your profile/ });
      await account.check();
      await auth.getByRole("button", { name: /^More/ }).click();
      assert.equal(await profile.isChecked(), true);
      assert.equal(await profile.isDisabled(), true);
      await account.uncheck();
      await profile.check();
      assert.equal(await account.isChecked(), false);
      await auth.getByText(/1 of \d+ already included/).waitFor();
      await page
        .getByRole("button", { name: "Save role", exact: true })
        .click();
      await page.getByText(title, { exact: true }).waitFor();
      await page.reload();
      await page.getByText(title, { exact: true }).waitFor();
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

// A delete refused because someone changed the role meanwhile rereads it.
// This catches a reread that throws away the operator's unsaved edits, or
// one that quietly moves a typed title onto the newer role so Save overwrites
// the title someone else saved.
test("a refused role delete keeps unsaved edits and shows a title changed meanwhile beside the typed one", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const original = `Readers ${ulid()}`;
    const capabilityIds = ["trellis.auth.v1:users.read"];
    let { role } = await runtime.callAdminRpc(
      "authRolesPut",
      authApi.actions["rpc:Roles.Put"].input.decode({
        expectedRevision: "0",
        title: original,
        description: "",
        capabilityIds,
        assignToNewUsers: false,
        idempotencyKey: codecs.ulid.decode(ulid()),
      }),
    );
    // Another administrator renames the role.
    const rename = async (title: string) => {
      ({ role } = await runtime.callAdminRpc(
        "authRolesPut",
        authApi.actions["rpc:Roles.Put"].input.decode({
          roleId: role.roleId,
          expectedRevision: String(role.revision),
          title,
          description: "",
          capabilityIds,
          assignToNewUsers: false,
          idempotencyKey: codecs.ulid.decode(ulid()),
        }),
      ));
    };
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsole(page, runtime, BROWSER_ADMIN);
      const edit = `${runtime.consoleUrl}/console/admin/roles/edit?roleId=${role.roleId}`;
      await page.goto(edit);
      const title = page.getByLabel("Title", { exact: true });
      const description = page.getByLabel("Description", { exact: true });
      const save = page.getByRole("button", { name: "Save role", exact: true });
      const remove = page.getByRole("button", {
        name: "Delete role",
        exact: true,
      });
      const conflicts = page.getByRole("group", {
        name: "Changed while you were editing",
      });
      page.on("dialog", (dialog) => void dialog.accept());
      await runtime.waitFor(
        async () => (await title.inputValue()) === original || undefined,
      );
      await description.fill("Mine description");
      await rename("Theirs title");

      // The title left alone shows the new name; the typed description stays.
      await remove.click();
      await runtime.waitFor(
        async () => (await title.inputValue()) === "Theirs title" || undefined,
      );
      assert.equal(await description.inputValue(), "Mine description");
      assert.equal(await conflicts.count(), 0);
      assert.equal(await save.isEnabled(), true);

      // A typed title someone renamed meanwhile is shown beside their name.
      await title.fill("Mine title");
      await rename("Theirs again");
      await remove.click();
      const conflict = conflicts.getByRole("listitem", {
        name: "Title changed meanwhile",
      });
      await conflict.waitFor({ timeout: 30_000 });
      assert.equal(
        await conflict.locator('[data-draft="mine"]').innerText(),
        "Mine title",
      );
      assert.equal(
        await conflict.locator('[data-draft="saved"]').innerText(),
        "Theirs again",
      );
      assert.equal(await title.inputValue(), "Mine title");
      assert.equal(await description.inputValue(), "Mine description");
      assert.equal(await save.isDisabled(), true);
      await conflict
        .getByRole("button", { name: "Keep yours for Title" })
        .click();
      await save.click();
      await page.waitForURL(/\/console\/admin\/roles$/);

      // The role was never deleted, and holds what the operator chose.
      await page.goto(edit);
      await runtime.waitFor(
        async () => (await title.inputValue()) === "Mine title" || undefined,
      );
      assert.equal(await description.inputValue(), "Mine description");
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
