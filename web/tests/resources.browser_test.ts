import { deepStrictEqual, ok, strictEqual } from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { test } from "node:test";
import { chromium, type Page } from "playwright";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as removedParticipants } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { loginNativeTestClient } from "../../ts/packages/trellis-testkit/src/admin/native_auth.ts";
import { generateSessionSeed } from "../../ts/packages/trellis-testkit/src/auth/random.ts";
import {
  apps as adminApps,
  types as adminTypes,
} from "../../ts/packages/trellis-testkit/trellis/index.js";
import { connectNativeClient } from "../../ts/packages/trellis/native.ts";
import { createAuth } from "../../ts/packages/trellis/portable.ts";
import { TrellisService } from "../../ts/packages/trellis/service/mod.ts";

async function findResource(page: Page, resourceId: string): Promise<void> {
  const row = page
    .getByRole("row")
    .filter({ has: page.getByText(resourceId, { exact: true }) });
  for (let index = 0; index < 40; index++) {
    await page.waitForFunction(
      () => {
        const button = [...document.querySelectorAll("button")].find(
          (item) => item.textContent?.trim() === "Refresh",
        );
        return button && !button.disabled;
      },
      undefined,
      { timeout: 10_000 },
    );
    if (await row.count()) {
      await row.waitFor();
      return;
    }
    const next = page.getByRole("button", { name: "Next", exact: true });
    ok(
      (await next.count()) && (await next.isEnabled()),
      `Exact resource ${resourceId} absent from the retained catalog`,
    );
    const previousRows = await page.getByRole("row").allTextContents();
    await next.click();
    await page.waitForFunction(
      (previous) => {
        const rows = [...document.querySelectorAll("tr")].map(
          (item) => item.textContent,
        );
        return JSON.stringify(rows) !== previous;
      },
      JSON.stringify(previousRows),
      { timeout: 10_000 },
    );
  }
  throw new Error(`Exact resource ${resourceId} not found within 40 pages`);
}

// Allocate through public deployment/provision/apply APIs unless the runner supplies
// exact expendable fixtures. Never pick the first allocation or seed private SQL.
test(
  "Console retains detached allocations until the operator confirms one exact deletion",
  { timeout: 90_000 },
  async (context) => {
    const application = process.env.TRELLIS_TEST_CONSOLE_URL;
    const username = process.env.TRELLIS_TEST_ADMIN_USERNAME;
    const password = process.env.TRELLIS_TEST_ADMIN_PASSWORD;
    let resourceId = process.env.TRELLIS_TEST_DETACHED_RESOURCE_ID;
    let resourceName = process.env.TRELLIS_TEST_DETACHED_RESOURCE_NAME;
    let readyId = process.env.TRELLIS_TEST_READY_RESOURCE_ID;
    if (!application || !username || !password)
      throw new Error(
        "Live resource runner must supply Console URL and administrator credentials",
      );
    const close: Array<() => Promise<unknown>> = [];
    context.after(async () => {
      for (const dispose of close.reverse()) await dispose();
    });
    if (!resourceId || !resourceName || !readyId) {
      const issuer = process.env.TRELLIS_NATIVE_TEST_ISSUER;
      ok(issuer, "public fixture setup requires TRELLIS_NATIVE_TEST_ISSUER");
      const directory = await mkdtemp("/tmp/opencode/console-resources-");
      close.push(() => rm(directory, { recursive: true, force: true }));
      const login = await loginNativeTestClient({
        issuer,
        username,
        password,
        directory,
        clientId: "trellis.cli",
        request: adminApps.cli.request,
        platformPrivileges: { required: ["principals.manage"] },
        signal: context.signal,
      });
      const admin = await connectNativeClient({
        ...login,
        request: adminApps.cli.request,
        signal: context.signal,
      }).orThrow();
      close.push(() => admin.connection.close());
      const provider = participants.ResourceProvider.participant;
      const omitted = removedParticipants.ResourceProvider.participant;
      const allocations = [];
      for (const label of ["detached", "ready"]) {
        const { deployment } = await admin
          .deploymentsCreate({
            title: `console-${label}-${ulid()}`,
            kind: provider.kind,
            participantId: provider.identity,
            packageEvidence: adminTypes.AuthPackageEvidenceCodec.decode(
              provider.packageEvidence,
            ),
            idempotencyKey: ulid(),
          })
          .orThrow();
        const seed = generateSessionSeed();
        const identity = await createAuth({ sessionKeySeed: seed });
        await admin
          .serviceInstancesProvision({
            deploymentId: deployment.deploymentId,
            identityPublicKey: identity.sessionKey,
            title: `console ${label} resources`,
            idempotencyKey: ulid(),
          })
          .orThrow();
        const service = await TrellisService.connect({
          trellisUrl: issuer,
          participant: provider,
          seed,
          signal: context.signal,
        }).orThrow();
        close.push(() => service.stop());
        const value = { value: `retained-${ulid()}` };
        await service.kv.records.put("retained", value).orThrow();
        deepStrictEqual(
          await service.kv.records.get("retained").orThrow(),
          value,
        );
        await service.stop();
        if (label === "detached") {
          const current = await admin
            .deploymentsGet({ deploymentId: deployment.deploymentId })
            .orThrow();
          await admin
            .deploymentsApply({
              deploymentId: deployment.deploymentId,
              expectedRevision: current.deployment.revision,
              packageEvidence: adminTypes.AuthPackageEvidenceCodec.decode(
                omitted.packageEvidence,
              ),
              idempotencyKey: ulid(),
            })
            .orThrow();
        }
        const { items } = await admin
          .resourcesList({
            ownerKind: "deployment",
            ownerId: deployment.deploymentId,
          })
          .orThrow();
        const matches = items.filter(
          (item) =>
            item.ownerId === deployment.deploymentId &&
            item.participantId === provider.identity &&
            item.kind === "kv" &&
            item.localName === "records",
        );
        strictEqual(
          matches.length,
          1,
          "exact deployment records allocation must be retained",
        );
        const allocation = matches[0]!;
        strictEqual(allocation.state, label);
        allocations.push(allocation);
      }
      resourceId = allocations[0]!.resourceId;
      resourceName = allocations[0]!.localName;
      readyId = allocations[1]!.resourceId;
    }
    const origin = new URL(application).origin;
    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();
    try {
      page.setDefaultTimeout(10_000);
      await page.goto(new URL("/console/admin/resources", origin).href);
      await page.getByLabel("Username", { exact: true }).fill(username);
      await page.getByLabel("Password", { exact: true }).fill(password);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
      await page
        .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
        .waitFor();
      for (const choice of await page.getByRole("checkbox").all())
        if (await choice.isEnabled()) await choice.check();
      const privileges = page
        .getByRole("group", { name: "Administrative privileges", exact: true })
        .getByRole("checkbox");
      for (const choice of await privileges.all()) await choice.uncheck();
      await page.getByRole("checkbox", { name: /Manage identities/ }).check();
      await page.getByRole("button", { name: "Authorize application" }).click();
      await page
        .getByRole("heading", { name: "Resources", exact: true })
        .waitFor();
      const row = page
        .getByRole("row")
        .filter({ has: page.getByText(resourceId, { exact: true }) });
      await findResource(page, resourceId);
      await page.reload();
      await page
        .getByRole("heading", { name: "Resources", exact: true })
        .waitFor();
      await findResource(page, resourceId);
      // Seeing the allocation after a real read, not a declaration inventory, is
      // what establishes retention. Changing filters must include ready resources.
      await page
        .getByRole("button", { name: "All retained states", exact: true })
        .click();
      const ready = page
        .getByRole("row")
        .filter({ has: page.getByText(readyId, { exact: true }) });
      await findResource(page, readyId);
      strictEqual(
        await ready.getByRole("button", { name: /^Delete / }).count(),
        0,
      );
      await page.getByRole("button", { name: "Detached", exact: true }).click();
      await findResource(page, resourceId);
      await row.getByRole("button", { name: /^Delete / }).click();
      const dialog = page.getByRole("dialog");
      await dialog
        .getByRole("button", { name: "Cancel", exact: true })
        .first()
        .click();
      await page.getByRole("button", { name: "Refresh", exact: true }).click();
      await findResource(page, resourceId);
      await row.getByRole("button", { name: /^Delete / }).waitFor();
      await row.getByRole("button", { name: /^Delete / }).click();
      await dialog.getByRole("textbox").fill(resourceName);
      await dialog
        .getByRole("button", { name: "Permanently delete", exact: true })
        .click();
      await row.waitFor({ state: "hidden" });
      await page
        .getByRole("button", { name: "All retained states", exact: true })
        .click();
      await findResource(page, readyId);
    } catch (error) {
      await page.screenshot({
        path: "/tmp/opencode/resources-failure.png",
        fullPage: true,
      });
      await writeFile(
        "/tmp/opencode/resources-failure.html",
        await page.content(),
      );
      context.diagnostic(await page.locator("body").innerText());
      throw error;
    } finally {
      await browser.close();
    }
  },
);
