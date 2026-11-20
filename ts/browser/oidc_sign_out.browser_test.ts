import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { chromium, type Page } from "playwright";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";

import { apis } from "../packages/trellis-testkit/trellis/index.js";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  approveConsentIfRequired,
  browserRuntimeOptions,
  loginCliAsAdministrator,
  waitForConsoleConnected,
} from "./browser_test_support.ts";
import { startDex } from "./dex_support.ts";

// Signing out of the Console ends the Portal login but not the provider's own
// session. This catches the next OIDC sign-in from that browser going to the
// provider without prompt=login, which signs the person straight back in, and
// a browser that signed in again still being made to re-enter its password on
// every later sign-in, which would take away the provider's single sign-on.
test(
  "after signing out, the next OIDC sign-in makes the provider ask again, and later ones do not",
  { timeout: 300_000 },
  async (t) => {
    await withTrellisRuntime(async (runtime) => {
      const email = "person@example.com";
      const dex = await startDex({
        directory: runtime.workdir,
        redirectUri: `${runtime.trellisUrl}/portal/oidc/callback`,
        emails: [email],
      });
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      let page: Page | undefined;
      try {
        page = await browser.newPage();
        page.setDefaultTimeout(30_000);
        const { run } = await loginCliAsAdministrator(runtime, page);
        const secretFile = join(runtime.workdir, `dex-secret-${ulid()}`);
        await writeFile(secretFile, dex.clientSecret);
        const { provider } = await run(
          "sign-in",
          "providers",
          "add",
          "--name",
          "Company",
          "--issuer",
          dex.issuer,
          "--client-id",
          dex.clientId,
          "--client-secret-file",
          secretFile,
          "--certificate-fingerprint",
          dex.fingerprint,
        );
        const subject = dex.subjects.get(email);
        assert.ok(subject);
        await runtime.callAdminRpc(
          "authUsersCreate",
          apis.auth.API.actions["rpc:Users.Create"].input.decode({
            username: "person",
            displayName: "Person",
            identities: [{ providerId: provider.providerId, subject }],
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        );

        const context = await browser.newContext();
        // Every authorization request this browser sends to Dex, in order, not
        // the steps Dex sends itself through the same path.
        const dexAuthorize = `${new URL(dex.issuer).pathname}/auth`;
        const toDex: URL[] = [];
        const toConsoleAuthorize: string[] = [];
        context.on("request", (request) => {
          const url = new URL(request.url());
          if (
            url.origin === new URL(dex.issuer).origin &&
            url.pathname === dexAuthorize &&
            url.searchParams.has("client_id")
          )
            toDex.push(url);
          if (
            url.searchParams.get("response_type") === "code" &&
            url.searchParams.get("client_id") === "trellis.console"
          )
            toConsoleAuthorize.push(url.href);
        });
        page = await context.newPage();
        page.setDefaultTimeout(30_000);
        const user = page;
        const continueWithDex = async () => {
          await user
            .getByRole("link", { name: "Continue with Company", exact: true })
            .click();
          await user.waitForURL(
            (url) => url.origin === new URL(dex.issuer).origin,
          );
          await runtime.waitFor(async () => toDex.length > 0);
          return toDex.splice(0).map((url) => url.searchParams.get("prompt"));
        };
        const signInAtDex = async () => {
          await user.locator('input[name="login"]').fill(email);
          await user.locator('input[name="password"]').fill("password");
          await user.locator('button[type="submit"]').click();
          await approveConsentIfRequired(user);
          await waitForConsoleConnected(user);
        };

        // A first sign-in lets the provider use its session.
        await user.goto(`${runtime.consoleUrl}/console`, {
          waitUntil: "domcontentloaded",
        });
        assert.deepEqual(await continueWithDex(), [null]);
        await signInAtDex();

        // Signing out of the Console ends the Portal login and returns to
        // the Portal's sign-in page.
        await user.getByLabel("Open user menu", { exact: true }).click();
        await user
          .getByRole("button", { name: "Sign out", exact: true })
          .click();
        await user.getByLabel("Username", { exact: true }).waitFor();
        assert.deepEqual(await continueWithDex(), ["login"]);
        await signInAtDex();

        // The Portal login lapses, without a sign-out. The provider may sign
        // the person in from its session again.
        await context.clearCookies({ name: "__Host-trellis-portal" });
        await context.clearCookies({ name: "__Host-trellis-portal-csrf" });
        const authorize = toConsoleAuthorize.at(-1);
        assert.ok(authorize);
        await user.goto(authorize, { waitUntil: "domcontentloaded" });
        assert.deepEqual(await continueWithDex(), [null]);
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
