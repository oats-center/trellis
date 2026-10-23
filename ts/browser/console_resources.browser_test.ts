import { deepStrictEqual, ok, strictEqual } from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium, type Page } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as removedParticipants } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import { generateSessionSeed } from "../packages/trellis-testkit/src/auth/random.ts";
import {
  apps as adminApps,
  types as adminTypes,
} from "../packages/trellis-testkit/trellis/index.js";
import { connectNativeClient } from "../packages/trellis/native.ts";
import { createAuth } from "../packages/trellis/portable.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";

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

// Allocate through public deployment/provision/apply APIs. Never pick the first
// allocation or seed private SQL.
test(
  "Console retains detached allocations until the operator confirms one exact deletion",
  { timeout: 90_000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      let resourceId = "";
      let resourceName = "";
      let readyId = "";
      const application = runtime.trellisUrl;
      const issuer = runtime.oauthIssuer;
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;
      const close: Array<() => Promise<unknown>> = [];
      context.after(async () => {
        for (const dispose of close.reverse()) await dispose();
      });
      {
        const login = await loginNativeTestClient({
          issuer,
          username,
          password,
          clientId: "trellis.cli",
          request: adminApps.cli.request,
          administrativeCapabilities: ["trellis.auth.v1:users.manage"],
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
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      const page = await browser.newPage();
      try {
        page.setDefaultTimeout(10_000);
        await page.goto(new URL("/console/admin/resources", origin).href);
        await page.getByLabel("Username", { exact: true }).fill(username);
        await page.getByLabel("Password", { exact: true }).fill(password);
        await page
          .getByRole("button", { name: "Sign in", exact: true })
          .click();
        await page
          .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
          .waitFor();
        for (const choice of await page.getByRole("checkbox").all())
          if (await choice.isEnabled()) await choice.check();
        const administration = page
          .getByRole("group", {
            name: "Manage this Trellis",
            exact: true,
          })
          .getByRole("checkbox");
        for (const choice of await administration.all()) await choice.uncheck();
        await page
          .getByRole("checkbox", { name: /Manage users and services/ })
          .check();
        await page
          .getByRole("button", { name: "Authorize application" })
          .click();
        await page
          .getByRole("checkbox", { name: /I understand this application/ })
          .check();
        await page
          .getByRole("button", { name: "Allow management of Trellis" })
          .click();
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
        await page
          .getByRole("button", { name: "Detached", exact: true })
          .click();
        await findResource(page, resourceId);
        await row.getByRole("button", { name: /^Delete / }).click();
        const dialog = page.getByRole("dialog");
        await dialog
          .getByRole("button", { name: "Cancel", exact: true })
          .first()
          .click();
        await page
          .getByRole("button", { name: "Refresh", exact: true })
          .click();
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
        const results = fileURLToPath(
          new URL("./test-results/", import.meta.url),
        );
        await mkdir(results, { recursive: true });
        await page.screenshot({
          path: `${results}resources-failure.png`,
          fullPage: true,
        });
        await writeFile(
          `${results}resources-failure.html`,
          await page.content(),
        );
        context.diagnostic(await page.locator("body").innerText());
        throw error;
      } finally {
        await browser.close();
      }
    }, browserRuntimeOptions());
  },
);
