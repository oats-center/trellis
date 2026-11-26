// Every OAuth flow Trellis serves, run against apps and people an
// installation export carried. Started by the Rust test
// crates/trellis-testkit/tests/installation_oauth.rs, which owns both
// installations and moves one into the other; this driver owns Dex and the
// browser. They talk in lines: the driver writes `@<step> <json>` to stdout
// and reads the Rust test's JSON answer from stdin.
import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdir, readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { chromium, type Page } from "playwright";
import { build } from "vite";
import {
  browserTrustEnvironment,
  trustInstallationCa,
} from "../packages/trellis-testkit/src/tls_trust.ts";
import { approveConsentIfRequired } from "./browser_test_support.ts";
import { startDex } from "./dex_support.ts";

const SOURCE = process.env.TRELLIS_SOURCE_URL!;
const TARGET = process.env.TRELLIS_TARGET_URL!;
const APP = process.env.TRELLIS_APP_ORIGIN!;
const DIRECTORY = process.env.TRELLIS_DRIVER_DIRECTORY!;
/** Signs in on both installations, and its account moves. */
const MOVER = "mover@example.com";
/** Signs in on both before the move, so the target already has them. */
const TWIN = "twin@example.com";

type Setup = {
  provider: string;
  browserClient: string;
  local: { username: string; password: string };
};
type Person = { dex: string } | { username: string; password: string };

const answers = createInterface({ input: process.stdin })[
  Symbol.asyncIterator
]();
/** Tells the Rust test `step` happened and waits for its answer. */
async function ask<T>(step: string, data: unknown = {}): Promise<T> {
  process.stdout.write(`@${step} ${JSON.stringify(data)}\n`);
  const answer = await answers.next();
  assert.ok(!answer.done, `the Rust test answers ${step}`);
  return JSON.parse(answer.value) as T;
}

await mkdir(DIRECTORY, { recursive: true });
trustInstallationCa(process.env.TRELLIS_SOURCE_CA!);
trustInstallationCa(process.env.TRELLIS_TARGET_CA!);
const dex = await startDex({
  directory: DIRECTORY,
  redirectUri: [SOURCE, TARGET].map((url) => `${url}/portal/oidc/callback`),
  emails: [MOVER, TWIN],
});
const servers: ReturnType<typeof createServer>[] = [];
let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
/** The page a step works in, whose address and text a failure shows. */
let shown: Page | undefined;
try {
  const setup = await ask<Setup>("dex", {
    issuer: dex.issuer,
    clientId: dex.clientId,
    clientSecret: dex.clientSecret,
    fingerprint: dex.fingerprint,
    subjects: { mover: dex.subjects.get(MOVER), twin: dex.subjects.get(TWIN) },
  });
  await build({
    configFile: false,
    logLevel: "error",
    build: {
      rollupOptions: { output: { inlineDynamicImports: true } },
      outDir: DIRECTORY,
      emptyOutDir: false,
      lib: {
        entry: new URL("./installation_oauth_client.ts", import.meta.url)
          .pathname,
        formats: ["es"],
        fileName: () => "client.js",
      },
    },
  });
  const script = await readFile(join(DIRECTORY, "client.js"));
  const app = createServer((request, response) => {
    const code = request.url === "/client.js";
    response.setHeader("Content-Type", code ? "text/javascript" : "text/html");
    response.end(code ? script : "<!doctype html><title>Moved app</title>");
  });
  servers.push(app);
  app.listen(Number(new URL(APP).port), "127.0.0.1");
  await once(app, "listening");
  browser = await chromium.launch({
    headless: true,
    env: browserTrustEnvironment(),
  });

  /** A browser profile, with each grant type it sent to `/token` and each `/revoke` status. */
  async function profile() {
    const context = await browser!.newContext();
    const grants: string[] = [];
    const revocations: number[] = [];
    context.on("request", (request) => {
      if (
        request.method() === "POST" &&
        new URL(request.url()).pathname === "/token"
      )
        grants.push(
          new URLSearchParams(request.postData() ?? "").get("grant_type") ?? "",
        );
    });
    context.on("response", (response) => {
      if (new URL(response.url()).pathname === "/revoke")
        revocations.push(response.status());
    });
    const page = await context.newPage();
    shown = page;
    page.setDefaultTimeout(30_000);
    page.on("pageerror", (error) => console.error(error));
    page.on("framenavigated", (frame) => {
      if (
        frame === page.mainFrame() &&
        new URL(frame.url()).pathname === "/consent"
      )
        consentPages.set(page, (consentPages.get(page) ?? 0) + 1);
    });
    return { context, page, grants, revocations };
  }
  /** How many times each page was shown the Portal's consent page. */
  const consentPages = new WeakMap<Page, number>();

  /** Signs `who` in on the Portal page in front of it, through Dex or with a password. */
  async function portalSignIn(page: Page, who: Person) {
    await page.getByLabel("Username", { exact: true }).waitFor();
    if ("dex" in who) {
      await page
        .getByRole("link", { name: `Continue with ${setup.provider}` })
        .click();
      await page.waitForURL((url) => url.origin === new URL(dex.issuer).origin);
      await page.locator('input[name="login"]').fill(who.dex);
      await page.locator('input[name="password"]').fill("password");
      await page.locator('button[type="submit"]').click();
    } else {
      await page.getByLabel("Username", { exact: true }).fill(who.username);
      await page.getByLabel("Password", { exact: true }).fill(who.password);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
    }
  }

  const call = (page: Page, method: string, ...args: string[]) =>
    page.evaluate(
      async ({ method, args }) =>
        (await import(new URL("/client.js", location.origin).href))[method](
          ...args,
        ),
      { method, args },
    );
  const navigated = (error: unknown) => {
    // Connecting without a sign-in leaves for the Portal.
    if (
      !String(error).includes("Execution context was destroyed") &&
      !String(error).includes("OAuthAuthorizationRequired")
    )
      throw error;
  };

  /**
   * Signs `who` in to the browser app on `trellis` with the authorization
   * code flow, from a page that is on the app and not signed in. Returns the
   * State only the role lets it read, and whether the Portal asked for
   * consent.
   */
  async function appSignIn(page: Page, trellis: string, who: Person) {
    const before = consentPages.get(page) ?? 0;
    if (new URL(page.url()).origin !== APP) await page.goto(APP);
    await call(page, "connect", trellis, setup.browserClient).catch(navigated);
    await portalSignIn(page, who);
    await approveConsentIfRequired(page, 30_000);
    await page.waitForURL(
      (url) => url.origin === APP && url.pathname === "/callback",
    );
    await call(page, "connect", trellis, setup.browserClient);
    return {
      state: await call(page, "read"),
      asked: (consentPages.get(page) ?? 0) > before,
    };
  }
  type Kept = { value: { value: string }; createdAt: string };
  /** The State each Dex person kept through the app on the source. */
  const kept = new Map<string, Kept>();
  /** Whether `state` is what `email` kept, with the time it was made. */
  const keeps = (state: unknown, email: string) => {
    const { value, createdAt } = state as Kept;
    assert.deepEqual({ value, createdAt }, kept.get(email));
  };

  // The source: both Dex people sign in through the app, with the provider
  // and roles the Rust test set up, consent, and keep State.
  for (const email of [MOVER, TWIN]) {
    const { context, page } = await profile();
    const signedIn = await appSignIn(page, SOURCE, { dex: email });
    assert.deepEqual(signedIn, { state: null, asked: true });
    await call(page, "write", `kept by ${email}`);
    const { value, createdAt } = (await call(page, "read")) as Kept;
    assert.deepEqual(value, { value: `kept by ${email}` });
    kept.set(email, { value, createdAt });
    await context.close();
  }
  // The target has its own copy of the provider before the move, and the
  // twin signs in there once, through the Console.
  await ask("source-signed-in");
  {
    const { context, page } = await profile();
    await page.goto(`${TARGET}/console`);
    await portalSignIn(page, { dex: TWIN });
    await approveConsentIfRequired(page, 30_000);
    await page.waitForURL(
      (url) => url.origin === TARGET && url.pathname.startsWith("/console"),
    );
    await context.close();
  }
  await ask("target-twin-signed-in");

  // The target, after the move. The moved person signs in to the moved app
  // through the moved provider: authorization code with PKCE. The consent
  // they gave moved, so the Portal does not ask again, and the app finds the
  // State they kept.
  const mover = await profile();
  const moved = await appSignIn(mover.page, TARGET, { dex: MOVER });
  assert.equal(moved.asked, false);
  keeps(moved.state, MOVER);
  assert.deepEqual(mover.grants, ["authorization_code"]);
  await ask("target-app-signed-in", { who: "mover" });

  // A reload renews the session with the refresh token, without the Portal.
  await mover.page.reload();
  await call(mover.page, "connect", TARGET, setup.browserClient);
  assert.deepEqual(mover.grants, ["authorization_code", "refresh_token"]);
  keeps(await call(mover.page, "read"), MOVER);

  // Signing out revokes the refresh family and ends the Portal login, so
  // connecting again needs a new sign-in; the moved local person signs in,
  // and has no State of their own.
  await call(mover.page, "logout").catch(navigated);
  await mover.page.waitForURL(
    (url) => url.origin === APP && url.pathname === "/",
  );
  assert.deepEqual(mover.revocations, [200]);
  assert.equal((await appSignIn(mover.page, TARGET, setup.local)).state, null);
  assert.deepEqual(mover.grants, [
    "authorization_code",
    "refresh_token",
    "authorization_code",
  ]);
  await ask("target-app-signed-in", { who: "local" });
  await mover.context.close();

  // The twin, whom the target already had under another ID, signs in to
  // the moved app with the role, the consent and the State the source gave
  // them.
  const twin = await profile();
  const matched = await appSignIn(twin.page, TARGET, { dex: TWIN });
  assert.equal(matched.asked, false);
  keeps(matched.state, TWIN);
  await ask("target-app-signed-in", { who: "twin" });
  await twin.context.close();

  // Device code: the moved native app, approved in the Portal through Dex
  // and with a password.
  for (const who of ["mover", "local"] as const) {
    const device = await ask<{ verificationUri: string; userCode: string }>(
      "device",
      { who },
    );
    const { context, page } = await profile();
    await page.goto(device.verificationUri);
    await portalSignIn(page, who === "mover" ? { dex: MOVER } : setup.local);
    const code = page.getByLabel("Code", { exact: true });
    const confirm = page.getByRole("heading", {
      name: "Is this your device?",
    });
    if (
      await Promise.any([
        code.waitFor().then(() => true),
        confirm.waitFor().then(() => false),
      ])
    ) {
      await code.fill(device.userCode);
      await page.getByRole("button", { name: "Continue", exact: true }).click();
    }
    await page
      .getByRole("button", { name: "Yes, this is my device", exact: true })
      .click();
    await approveConsentIfRequired(page, 30_000);
    await page
      .getByRole("heading", { name: "Your device is connected" })
      .waitFor();
    await context.close();
    await ask("device-approved", { who });
  }
  await ask("done");
} catch (error) {
  console.error(error);
  if (shown && !shown.isClosed())
    console.error(shown.url(), await shown.locator("body").innerText());
  console.error(await dex.log());
  process.exitCode = 1;
} finally {
  await browser?.close();
  for (const server of servers) server.close();
  await dex.stop();
  answers.return?.();
  process.stdin.destroy();
}
