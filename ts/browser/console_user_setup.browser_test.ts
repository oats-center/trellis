// An administrator adds a user in the Console and that person signs in.
// Real runtime, real Portal, real Console.

import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
  loginCliAsAdministrator,
  openConsole,
} from "./browser_test_support.ts";
import {
  openConsoleAsRuntimeAdmin,
  waitForConsoleShell,
} from "./console_test_support.ts";
import { startDex } from "./dex_support.ts";

const passwordsOff = "Password sign-in is off";

// A fresh installation needs no switch turned on first. This catches a
// created user who is left without a way in: a setup step that only hands
// out a bare token, or sign-in that refuses the password it set.
test("a user added in the Console on a fresh Trellis signs in with the password they choose", async () => {
  await withTrellisRuntime(async (runtime) => {
    const admin = await launchProfile(runtime);
    // The person uses a browser of their own.
    const browser = await chromium.launch({
      headless: true,
      env: browserTrustEnvironment(),
    });
    const person = await browser.newContext();
    try {
      const page = await admin.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.consoleUrl}/console/admin/users/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const username = `ct-setup-${ulid()}`.toLowerCase();
      await page.getByRole("textbox", { name: /Username/ }).fill(username);
      await page
        .getByRole("button", { name: "Create user", exact: true })
        .click();
      const link = page.getByLabel("Password setup link");
      await link.waitFor({ state: "visible", timeout: 30_000 });
      assert.equal(await page.getByText(passwordsOff).count(), 0);
      const setupUrl = new URL(await link.inputValue());
      assert.equal(setupUrl.origin, new URL(runtime.trellisUrl).origin);

      const personPage = await person.newPage();
      await personPage.goto(setupUrl.href, { waitUntil: "domcontentloaded" });
      const password = `Setup-${ulid()}`;
      await personPage
        .getByLabel("New password", { exact: true })
        .fill(password);
      await personPage
        .getByRole("button", { name: "Set password", exact: true })
        .click();
      await personPage.getByText("Your new password is set").waitFor();
      await openConsole(personPage, runtime, { username, password });
    } finally {
      await browser.close();
      await admin.close();
    }
  }, browserRuntimeOptions());
});

// With password sign-in off, the page says that a user added here can't sign
// in yet and turns it on in place. This catches a page that lets the
// administrator add users nobody can sign in as, without a word.
test("adding a user while password sign-in is off says so and turns it on", async () => {
  await withTrellisRuntime(async (runtime) => {
    const dex = await startDex({
      directory: runtime.workdir,
      redirectUri: `${runtime.trellisUrl}/portal/oidc/callback`,
      emails: ["someone@example.com"],
    });
    const admin = await launchProfile(runtime);
    try {
      await runtime.callAdminRpc("authOIDCProvidersPut", {
        title: "Company",
        issuer: dex.issuer,
        clientId: dex.clientId,
        clientSecret: dex.clientSecret,
        certificateFingerprint: dex.fingerprint,
        scopes: [],
        roleClaim: "",
        claimFreshnessSeconds: 3600,
        expectedRevision: 0n,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      const { run } = await loginCliAsAdministrator(
        runtime,
        await admin.newPage(),
      );
      const page = await admin.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await run("sign-in", "set", "--no-local-users");
      await page.goto(`${runtime.consoleUrl}/console/admin/users/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByText(passwordsOff).waitFor({ timeout: 30_000 });
      await page
        .getByRole("button", { name: "Turn on password sign-in", exact: true })
        .click();
      await page
        .getByText(passwordsOff)
        .waitFor({ state: "detached", timeout: 30_000 });
      assert.equal((await run("sign-in", "show")).settings.localUsers, true);
    } finally {
      await admin.close();
      await dex.stop();
    }
  }, browserRuntimeOptions());
});
