import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium, type Page } from "playwright";

import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  completeConsoleEntry,
  createOrdinaryUserWithPassword,
  waitForConsoleConnected,
} from "./browser_test_support.ts";
import { openConsoleAsRuntimeAdmin } from "./console_test_support.ts";
import { startDex } from "./dex_support.ts";

// The real trellis-server trusts only the system's certificates, and this Dex
// is served from a private CA, so it can sign in through Dex only because an
// administrator pinned that CA. The first part catches Auth trusting the CA
// without a pin, or storing a pin whose fingerprint was never checked. The
// second catches the Account page hiding a second account linked through the
// same provider, or disconnecting the wrong one (issue #49).
test(
  "an administrator pins a provider's private CA, and a person disconnects one of two accounts linked through it",
  { timeout: 300_000 },
  async (t) => {
    await withTrellisRuntime(async (runtime) => {
      const dex = await startDex({
        directory: runtime.workdir,
        redirectUri: `${runtime.trellisUrl}/portal/oidc/callback`,
        emails: ["first@example.com", "second@example.com"],
      });
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      let page: Page | undefined;
      try {
        page = await (await browser.newContext()).newPage();
        page.setDefaultTimeout(30_000);
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.consoleUrl}/console/admin/sign-in`);
        await waitForConsoleConnected(page);
        await page.getByRole("button", { name: "Add provider" }).click();
        const form = page.locator("form").filter({
          has: page.getByRole("button", { name: "Save provider" }),
        });
        await form.getByLabel("Name").fill("Company");
        await form.getByLabel("Address").fill(dex.issuer);
        await form.getByLabel("Client ID").fill(dex.clientId);
        await form.getByLabel("Client secret").fill(dex.clientSecret);
        const fingerprint = form.getByLabel("Certificate fingerprint");
        const save = () =>
          form.getByRole("button", { name: "Save provider" }).click();
        const listed = async () =>
          (await runtime.callAdminRpc("authOIDCProvidersList", {})).items;

        // Without a pin, Auth refuses an issuer nothing vouches for and names
        // the fingerprint to confirm.
        await save();
        await page
          .getByText(`its issuer's SHA-256 fingerprint is ${dex.fingerprint}`, {
            exact: false,
          })
          .waitFor();
        assert.deepEqual(await listed(), []);

        const wrong = Array.from({ length: 32 }, () => "AB").join(":");
        await fingerprint.fill(wrong);
        await save();
        await page
          .getByText(
            `presents issuer fingerprint ${dex.fingerprint}, not ${wrong}`,
            {
              exact: false,
            },
          )
          .waitFor();
        assert.deepEqual(await listed(), []);

        // However it is written, the right fingerprint pins Dex's CA.
        await fingerprint.fill(
          dex.fingerprint.replaceAll(":", "").toLowerCase(),
        );
        await save();
        await page.getByText("Provider saved.").waitFor();
        const [provider] = await listed();
        assert.equal(provider?.certificateFingerprint, dex.fingerprint);
        await page
          .getByRole("button", { name: "Actions for Company", exact: true })
          .click();
        await page.getByRole("button", { name: "Edit", exact: true }).click();
        assert.equal(await fingerprint.inputValue(), dex.fingerprint);

        const credentials = {
          username: "two-accounts",
          password: "two-accounts-password",
        };
        const principalId = await createOrdinaryUserWithPassword(
          runtime,
          credentials,
        );
        // The sign-in page arrives with its providers: "Continue with
        // Company" is there the moment the form is, so the form never moves
        // down when they load.
        const signIn = await browser.newContext();
        await signIn.addInitScript(() => {
          const seen = new MutationObserver(() => {
            if (!document.getElementById("username")) return;
            seen.disconnect();
            Object.assign(window, {
              providersWithForm: document.querySelector(
                'a[href*="/portal/oidc/"]',
              )?.textContent,
            });
          });
          seen.observe(document, { childList: true, subtree: true });
        });
        page = await signIn.newPage();
        page.setDefaultTimeout(30_000);
        await page.goto(`${runtime.consoleUrl}/console`, {
          waitUntil: "domcontentloaded",
        });
        await page.getByLabel("Username", { exact: true }).waitFor();
        assert.equal(
          (
            await page.evaluate(() =>
              "providersWithForm" in window
                ? String(window.providersWithForm)
                : "",
            )
          ).trim(),
          "Continue with Company",
        );
        await completeConsoleEntry(page, credentials);
        const account = `${runtime.consoleUrl}/console/profile`;
        const user = page;
        const link = async (email: string, connect: string) => {
          await user.goto(account);
          await waitForConsoleConnected(user);
          await user
            .getByRole("button", { name: connect, exact: true })
            .click();
          await user
            .getByRole("heading", { name: "Link OIDC sign-in" })
            .waitFor();
          await user
            .getByRole("button", { name: "Continue", exact: true })
            .click();
          await user
            .getByRole("link", { name: "Continue", exact: true })
            .click();
          await user.waitForURL(
            (url) => url.origin === new URL(dex.issuer).origin,
          );
          await user.locator('input[name="login"]').fill(email);
          await user.locator('input[name="password"]').fill("password");
          await user.locator('button[type="submit"]').click();
          await user
            .getByRole("heading", { name: "OIDC sign-in linked" })
            .waitFor();
        };
        await link("first@example.com", "Connect");
        await link("second@example.com", "Connect another");

        const identities = async () =>
          (
            (
              await runtime.callAdminRpc("authUsersFind", {
                userId: principalId,
              })
            ).found?.identities ?? []
          ).map((identity) => identity.subject);
        // Identity IDs are ULIDs, so the list is in the order they were linked.
        const [first, second, ...rest] = await identities();
        assert.ok(first && second, "both Dex accounts are linked");
        assert.deepEqual(rest, []);
        assert.notEqual(first, second);

        await user.goto(account);
        await waitForConsoleConnected(user);
        await user.getByText(first, { exact: true }).waitFor();
        await user.getByText(second, { exact: true }).waitFor();
        await user
          .locator("div")
          .filter({ hasText: second, hasNotText: first })
          .filter({ has: user.getByRole("button", { name: "Disconnect" }) })
          .last()
          .getByRole("button", { name: "Disconnect" })
          .click();
        await user
          .getByRole("dialog")
          .getByRole("button", { name: "Disconnect" })
          .click();
        await user
          .getByText(second, { exact: true })
          .waitFor({ state: "detached" });
        await user.getByText(first, { exact: true }).waitFor();
        assert.deepEqual(await identities(), [first]);
      } catch (error) {
        if (page)
          t.diagnostic(
            `${page.url()}\n${await page.locator("body").innerText()}`,
          );
        t.diagnostic(await dex.log());
        throw error;
      } finally {
        await browser.close();
        await dex.stop();
      }
    }, browserRuntimeOptions());
  },
);
