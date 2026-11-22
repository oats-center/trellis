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
// It also catches the Console's "Sign in again" recovery skipping the prompt
// when the Portal login had already ended, the provider's configured prompt
// not being sent, a provider that refuses the prompt being sent it on every
// retry, so the person could never sign in, and that refusal also dropping
// another provider's prompt, which would let its session sign the person
// straight back in.
test(
  "after signing out, the next OIDC sign-in sends the provider's prompt, and later ones do not",
  { timeout: 300_000 },
  async (t) => {
    await withTrellisRuntime(async (runtime) => {
      const email = "person@example.com";
      const dex = await startDex({
        directory: runtime.workdir,
        redirectUri: `${runtime.trellisUrl}/portal/oidc/callback`,
        emails: [email],
      });
      const partnerDex = await startDex({
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
        await writeFile(secretFile, partnerDex.clientSecret);
        await run(
          "sign-in",
          "providers",
          "add",
          "--name",
          "Partner",
          "--issuer",
          partnerDex.issuer,
          "--client-id",
          partnerDex.clientId,
          "--client-secret-file",
          secretFile,
          "--certificate-fingerprint",
          partnerDex.fingerprint,
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
        const toPartnerDex: URL[] = [];
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
            url.origin === new URL(partnerDex.issuer).origin &&
            url.pathname === dexAuthorize &&
            url.searchParams.has("client_id")
          )
            toPartnerDex.push(url);
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
        const submitAtDex = async () => {
          await user.locator('input[name="login"]').fill(email);
          await user.locator('input[name="password"]').fill("password");
          await user.locator('button[type="submit"]').click();
          await approveConsentIfRequired(user);
        };
        const signInAtDex = async () => {
          await submitAtDex();
          await waitForConsoleConnected(user);
        };
        const signOut = async () => {
          await user.getByLabel("Open user menu", { exact: true }).click();
          await user
            .getByRole("button", { name: "Sign out", exact: true })
            .click();
          await user.getByLabel("Username", { exact: true }).waitFor();
        };

        // A first sign-in lets the provider use its session.
        await user.goto(`${runtime.consoleUrl}/console`, {
          waitUntil: "domcontentloaded",
        });
        assert.deepEqual(await continueWithDex(), [null]);
        await signInAtDex();

        // Signing out of the Console ends the Portal login and returns to
        // the Portal's sign-in page.
        await signOut();
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

        // NATS refuses the Console's connection after the provider signed the
        // person in; a corrupted CONNECT signature gets that refusal from the
        // real broker. The Portal login is lost too before the person starts
        // over, so there is no Portal login left to end.
        let tamper = true;
        await user.routeWebSocket(/.*/, (socket) => {
          const broker = socket.connectToServer();
          socket.onMessage((message) => {
            const text = message.toString();
            if (!tamper || !text.startsWith("CONNECT "))
              return broker.send(message);
            const tampered = text.replace(/"sig":"(.)/, (_, first: string) =>
              first === "A" ? '"sig":"B' : '"sig":"A',
            );
            broker.send(
              typeof message === "string" ? tampered : Buffer.from(tampered),
            );
          });
        });
        await submitAtDex();
        // The Console's retry finds the Portal login and asks before it
        // continues as that person; continuing fails the same way.
        await user.getByRole("link", { name: /^Continue as / }).click();
        await user
          .getByText("Sign-in could not be completed", { exact: false })
          .waitFor();
        await context.clearCookies({ name: "__Host-trellis-portal" });
        await context.clearCookies({ name: "__Host-trellis-portal-csrf" });
        tamper = false;
        // Starting over is an explicit sign-out, so the provider asks again.
        await user
          .getByRole("button", { name: "Sign in again", exact: true })
          .click();
        await user.getByLabel("Username", { exact: true }).waitFor();
        assert.deepEqual(await continueWithDex(), ["login"]);
        await signInAtDex();

        // The provider's configured prompt is what a sign-out sends.
        await run(
          "sign-in",
          "providers",
          "edit",
          provider.providerId,
          "--prompt-after-sign-out",
          "select-account",
        );
        await signOut();
        assert.deepEqual(await continueWithDex(), ["select_account"]);
        await signInAtDex();

        // A provider that refuses the prompt answers with an OIDC error,
        // here the one Google gives for a prompt it does not support. The
        // person learns which setting to change, and their next try from
        // this browser goes without the prompt instead of failing again.
        await signOut();
        // Playwright routes only the first request of a redirect chain, so
        // the provider's answer replaces the redirect to it.
        const sentPrompts: (string | null)[] = [];
        const begin = (url: URL) =>
          url.pathname === `/portal/oidc/${provider.providerId}/begin`;
        await context.route(begin, async (route) => {
          const started = await route.fetch({ maxRedirects: 0 });
          const authorize = new URL(started.headers().location ?? "");
          sentPrompts.push(authorize.searchParams.get("prompt"));
          const callback = new URL(
            `${runtime.trellisUrl}/portal/oidc/callback`,
          );
          callback.searchParams.set(
            "state",
            authorize.searchParams.get("state") ?? "",
          );
          callback.searchParams.set("error", "invalid_request");
          callback.searchParams.set(
            "error_description",
            "Invalid parameter value for prompt.",
          );
          await route.fulfill({
            status: 302,
            headers: { location: callback.href },
          });
        });
        await user
          .getByRole("link", { name: "Continue with Company", exact: true })
          .click();
        const refusal = user.getByRole("alert");
        await refusal
          .filter({ hasText: '"Prompt after sign-out" setting' })
          .waitFor();
        assert.match(await refusal.innerText(), /prompt=select_account/);
        assert.deepEqual(sentPrompts, ["select_account"]);
        await context.unroute(begin);
        // The refusal was Company's alone: choosing Partner from the same
        // page still sends Partner's prompt.
        await user
          .getByRole("link", { name: "Continue with Partner", exact: true })
          .click();
        await user.waitForURL(
          (url) => url.origin === new URL(partnerDex.issuer).origin,
        );
        await runtime.waitFor(async () => toPartnerDex.length > 0);
        assert.deepEqual(
          toPartnerDex.map((url) => url.searchParams.get("prompt")),
          ["login"],
        );
        await user.goBack({ waitUntil: "domcontentloaded" });
        assert.deepEqual(await continueWithDex(), [null]);
        await signInAtDex();
      } catch (error) {
        if (page)
          t.diagnostic(
            `${page.url()}\n${await page.locator("body").innerText()}`,
          );
        t.diagnostic(await dex.log());
        t.diagnostic(await partnerDex.log());
        throw error;
      } finally {
        await browser.close();
        await dex.stop();
        await partnerDex.stop();
      }
    }, browserRuntimeOptions());
  },
);
