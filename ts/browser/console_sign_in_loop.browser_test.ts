import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import type { Frame, Page } from "playwright";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";

/**
 * Counts the tab's page navigations and its trips to the authorization
 * endpoint from now on. A sign-in loop makes several of each every second.
 */
function watchNavigations(page: Page) {
  const seen = { navigations: [] as string[], authorizations: 0 };
  const navigated = (frame: Frame) => {
    if (frame === page.mainFrame()) seen.navigations.push(frame.url());
  };
  const requested = (request: { url(): string }) => {
    if (new URL(request.url()).pathname === "/authorize") seen.authorizations++;
  };
  page.on("framenavigated", navigated);
  page.on("request", requested);
  return {
    seen,
    /** Stops counting once five quiet seconds show the tab has settled. */
    async settled() {
      await assert.rejects(
        page.waitForEvent("framenavigated", { timeout: 5_000 }),
        /Timeout/,
        `the tab kept navigating: ${seen.navigations.join(" ")}`,
      );
      page.off("framenavigated", navigated);
      page.off("request", requested);
      return seen;
    },
  };
}

async function profileOf(page: Page, username: string): Promise<void> {
  await page
    .getByRole("heading", { name: username, exact: true })
    .waitFor({ timeout: 30_000 })
    .catch(async (cause: unknown) => {
      throw new Error(
        `${page.url()}: ${await page.locator("body").innerText()}`,
        { cause },
      );
    });
}

/** Ends this browser's Portal login, as the Portal's sign-out page does. */
async function signOutOfPortal(
  page: Page,
  trellisUrl: string,
  returnTo: string,
): Promise<void> {
  const clientId = await page.evaluate(
    () =>
      (
        globalThis as typeof globalThis & {
          __TRELLIS_RUNTIME_CONFIG__: { clientId: string };
        }
      ).__TRELLIS_RUNTIME_CONFIG__.clientId,
  );
  const logout = new URL("/logout", trellisUrl);
  logout.searchParams.set("client_id", clientId);
  logout.searchParams.set("post_logout_redirect_uri", returnTo);
  await page.goto(logout.href);
}

test("an open Console tab carries on after a restart, an issuer key rotation and an old sign-in link", async () => {
  await withTrellisRuntime(async (runtime) => {
    const user = { username: "loop-user", password: "loop-user-password" };
    await createOrdinaryUserWithPassword(runtime, user);
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const callbacks: string[] = [];
      page.on("framenavigated", (frame) => {
        if (
          frame === page.mainFrame() &&
          new URL(frame.url()).searchParams.has("code")
        )
          callbacks.push(frame.url());
      });
      await openConsole(page, runtime, user);
      await page.goto(`${runtime.consoleUrl}/console/profile`);
      await profileOf(page, user.username);
      const spent = callbacks[0];
      assert.ok(spent, "signing in returned through the Console callback");

      // The server replaces its signing key and restarts behind the tab.
      const signer: string = await page.evaluate(
        async (discovery) =>
          (await (await fetch(discovery)).json()).trellis.issuers.find(
            (key: { state: string }) => key.state === "active",
          ).keyId,
        new URL("/.well-known/oauth-authorization-server", runtime.trellisUrl)
          .href,
      );
      await runtime.callAdminRpc("authIssuersRotate", {
        expectedCurrentKeyId: signer,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      await runtime.restart();
      let watch = watchNavigations(page);
      await page.reload({ waitUntil: "domcontentloaded" });
      await profileOf(page, user.username);
      let seen = await watch.settled();
      assert.equal(seen.authorizations, 0, seen.navigations.join(" "));
      assert.ok(seen.navigations.length <= 5, seen.navigations.join(" "));

      // An old callback, its code long spent, carries on with the session the
      // tab already has instead of signing in again.
      watch = watchNavigations(page);
      await page.goto(spent, { waitUntil: "domcontentloaded" });
      await profileOf(page, user.username);
      seen = await watch.settled();
      assert.equal(seen.authorizations, 0, seen.navigations.join(" "));
      assert.ok(seen.navigations.length <= 5, seen.navigations.join(" "));
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("signing in to the Console as someone else after a Portal sign-out opens their Console, and a refused sign-in stops on its problem page", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const dana = { username: "dana", password: "dana-portal-password" };
    const danaId = await createOrdinaryUserWithPassword(runtime, dana);
    const context = await launchProfile(runtime);
    let release: () => void = () => {};
    try {
      const page = await context.newPage();
      await openConsole(page, runtime, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      const profile = `${runtime.consoleUrl}/console/profile`;
      const signInAs = async (credentials: typeof dana) => {
        await page.getByLabel("Username", { exact: true }).waitFor();
        await page
          .getByLabel("Username", { exact: true })
          .fill(credentials.username);
        await page
          .getByLabel("Password", { exact: true })
          .fill(credentials.password);
        await page.getByRole("button", { name: "Sign in" }).click();
      };

      // Signing out of the Portal ends the login the Console's session came
      // from. The Console then sends the person to sign in, and they sign in
      // as someone else: the Console opens as them, once.
      await signOutOfPortal(page, runtime.trellisUrl, profile);
      let watch = watchNavigations(page);
      await signInAs(dana);
      await profileOf(page, dana.username);
      let seen = await watch.settled();
      assert.ok(seen.authorizations <= 1, seen.navigations.join(" "));
      assert.ok(seen.navigations.length <= 8, seen.navigations.join(" "));

      // The account is disabled while its sign-in is on the way back, so
      // Trellis refuses the code. The Console says so and stays there.
      await signOutOfPortal(page, runtime.trellisUrl, profile);
      const released = new Promise<void>((resolve) => (release = resolve));
      // The callback arrives by redirect, so the code exchange it starts is
      // the request held back.
      const atCallback = new Promise<void>((arrived) =>
        page.route(
          (url) => url.pathname === "/token",
          async (route) => {
            if (
              new URLSearchParams(route.request().postData() ?? "").get(
                "grant_type",
              ) === "authorization_code"
            ) {
              arrived();
              await released;
            }
            await route.continue();
          },
        ),
      );
      await signInAs(dana);
      let timer: ReturnType<typeof setTimeout> | undefined;
      await Promise.race([
        atCallback,
        new Promise((_, reject) => {
          timer = setTimeout(
            () => reject(new Error(`no sign-in callback from ${page.url()}`)),
            30_000,
          );
        }),
      ]).finally(() => clearTimeout(timer));
      const account = (
        await runtime.callAdminRpc("authUsersGet", { userId: danaId })
      ).user.account;
      assert.ok(account);
      await runtime.callAdminRpc("authUsersUpdate", {
        userId: danaId,
        enabled: false,
        expectedRevision: account.principal.revision,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      watch = watchNavigations(page);
      release();
      await page
        .getByText("Sign-in could not be completed", { exact: false })
        .waitFor({ timeout: 30_000 });
      await page
        .getByText("Trellis didn't accept this sign-in.", { exact: true })
        .waitFor();
      seen = await watch.settled();
      assert.equal(seen.authorizations, 0, seen.navigations.join(" "));
      await page
        .getByRole("button", { name: "Sign in again", exact: true })
        .waitFor();
    } finally {
      release();
      await context.close();
    }
  }, browserRuntimeOptions());
});
