import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { test } from "node:test";
import nacl from "tweetnacl";
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

test("replicas titled alike stay distinguishable, and one turned off is turned back on from its row", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedServiceWithoutResources(runtime, "replicas");
    // The CLI titles every instance it provisions after the deployment.
    const provisioned = [];
    for (let index = 0; index < 2; index++) {
      const { instance } = await runtime.callAdminRpc(
        "authServiceInstancesProvision",
        {
          deploymentId: seeded.deploymentId,
          title: seeded.deploymentId,
          identityPublicKey: Buffer.from(
            nacl.sign.keyPair.fromSeed(randomBytes(32)).publicKey,
          ).toString("base64url"),
          idempotencyKey: codecs.ulid.decode(ulid()),
        },
      );
      provisioned.push(instance);
    }
    const [, off] = provisioned;
    assert(off);
    await runtime.callAdminRpc("authServiceInstancesDisable", {
      instanceId: off.instanceId,
      expectedRevision: off.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.consoleUrl}/console/admin/services/${seeded.deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      const rows = page
        .getByRole("region", { name: "Instances" })
        .locator("tbody tr");
      await runtime.waitFor(async () => (await rows.count()) === 2);
      const names = await rows
        .locator("td.trellis-primary > span:first-child")
        .allInnerTexts();
      assert.equal(new Set(names).size, 2, `rows read alike: ${names}`);
      assert(
        names.every((name) => !name.includes(seeded.deploymentId)),
        `a row is named after its deployment: ${names}`,
      );

      const offRow = rows.filter({ hasText: off.instanceId });
      await offRow.getByText("Off", { exact: true }).waitFor();
      await offRow.getByRole("button", { name: /^Actions for / }).click();
      await page.getByRole("button", { name: "Turn on", exact: true }).click();
      await offRow.getByText("On", { exact: true }).waitFor();
      const { items } = await runtime.callAdminRpc("authServiceInstancesList", {
        deploymentId: seeded.deploymentId,
      });
      assert.equal(
        items.find((item) => item.instanceId === off.instanceId)?.state,
        "active",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
