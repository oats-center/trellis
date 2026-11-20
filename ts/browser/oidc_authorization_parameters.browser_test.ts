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

// An Auth0 application that requires an organization refuses every sign-in
// whose authorization request lacks `organization`. This catches a provider's
// custom authorization parameters not reaching the provider, parameters
// cleared from the provider still being sent, and a parameter that would
// override one Trellis sets itself being accepted.
test(
  "a provider's custom authorization parameters reach the provider until cleared",
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
        const parameters = [
          { name: "organization", value: "org_123" },
          { name: "audience", value: "https://api.example.com/" },
        ];
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
          ...parameters.flatMap(({ name, value }) => [
            "--authorization-param",
            `${name}=${value}`,
          ]),
        );
        assert.deepEqual(provider.authorizationParameters, parameters);

        // `prompt` has its own setting; letting a custom parameter set it
        // would bypass the prompt sent after a sign-out.
        await assert.rejects(
          run(
            "sign-in",
            "providers",
            "edit",
            provider.providerId,
            "--authorization-param",
            "prompt=none",
          ),
          (error: { stderr?: string }) =>
            /authorization parameter prompt is set by Trellis itself/.test(
              error.stderr ?? "",
            ),
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
        const dexAuthorize = `${new URL(dex.issuer).pathname}/auth`;
        const toDex: URL[] = [];
        context.on("request", (request) => {
          const url = new URL(request.url());
          if (
            url.origin === new URL(dex.issuer).origin &&
            url.pathname === dexAuthorize &&
            url.searchParams.has("client_id")
          )
            toDex.push(url);
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
          return toDex
            .splice(0)
            .map((url) => [
              url.searchParams.getAll("organization"),
              url.searchParams.getAll("audience"),
            ]);
        };

        await user.goto(`${runtime.consoleUrl}/console`, {
          waitUntil: "domcontentloaded",
        });
        assert.deepEqual(await continueWithDex(), [
          [["org_123"], ["https://api.example.com/"]],
        ]);
        await user.locator('input[name="login"]').fill(email);
        await user.locator('input[name="password"]').fill("password");
        await user.locator('button[type="submit"]').click();
        await approveConsentIfRequired(user);
        await waitForConsoleConnected(user);

        const cleared = await run(
          "sign-in",
          "providers",
          "edit",
          provider.providerId,
          "--clear-authorization-params",
        );
        assert.deepEqual(cleared.provider.authorizationParameters, []);
        await user.getByLabel("Open user menu", { exact: true }).click();
        await user
          .getByRole("button", { name: "Sign out", exact: true })
          .click();
        await user.getByLabel("Username", { exact: true }).waitFor();
        assert.deepEqual(await continueWithDex(), [[[], []]]);
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
