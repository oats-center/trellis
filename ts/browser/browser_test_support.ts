import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir } from "node:fs/promises";
import { createInterface } from "node:readline";
import { promisify } from "node:util";
import { ulid } from "ulid";
import { type BrowserContext, chromium, type Page } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { join } from "node:path";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import type { TrellisTestRuntimeStartOptions } from "@oatscenter/trellis-testkit";

import { repoTrellisSource } from "../integration/_support/runtime.ts";

/** Local administrator credentials created through the browser bootstrap page. */
export const BROWSER_ADMIN = {
  username: "admin",
  password: "browser-admin-password",
};

/** Returns the prebuilt server binary required by browser acceptance tests. */
export function prebuiltServer(): string {
  const server = process.env["TRELLIS_TEST_SERVER_BIN"];
  if (server === undefined) {
    throw new Error(
      "TRELLIS_TEST_SERVER_BIN must point to a prebuilt trellis-server",
    );
  }
  return server;
}

/** Runtime options that run the prebuilt control-plane binary. */
export function browserRuntimeOptions(
  options: Partial<TrellisTestRuntimeStartOptions> = {},
): Partial<TrellisTestRuntimeStartOptions> {
  return {
    ...options,
    adminPassword: options.adminPassword ?? BROWSER_ADMIN.password,
    trellis: {
      ...options.trellis,
      source: options.trellis?.source ?? repoTrellisSource(),
    },
  };
}

/** Launches a persistent Chromium profile owned by the test runtime workdir. */
export async function launchProfile(
  runtime: TrellisTestRuntime,
): Promise<BrowserContext> {
  return await chromium.launchPersistentContext(
    join(runtime.workdir, "browser-profile"),
    { headless: true, env: browserTrustEnvironment() },
  );
}

async function visibleWithin(
  locator: ReturnType<Page["getByLabel"]>,
  timeoutMs: number,
): Promise<boolean> {
  return await locator
    .first()
    .waitFor({ state: "visible", timeout: timeoutMs })
    .then(() => true)
    .catch(() => false);
}

/** Clicks the portal consent approval button when the flow asks for approval. */
export async function approveConsentIfRequired(
  page: Page,
  timeoutMs = 5_000,
): Promise<void> {
  const approve = page.getByRole("button", {
    name: "Allow",
    exact: true,
  });
  if (await visibleWithin(approve, timeoutMs)) {
    for (const choice of await page.getByRole("checkbox").all()) {
      if (await choice.isEnabled()) await choice.check();
    }
    await approve.click();
    await confirmAdministrationIfAsked(page);
  }
}

/** Completes the portal's separate confirmation step for administration, when shown. */
export async function confirmAdministrationIfAsked(
  page: Page,
  timeoutMs = 2_000,
): Promise<void> {
  const allow = page.getByRole("button", {
    name: "Allow management",
    exact: true,
  });
  if (await visibleWithin(allow, timeoutMs)) {
    await page.getByRole("checkbox", { name: /I understand this app/ }).check();
    await allow.click();
  }
}

/** Completes the local sign-in form when the portal displays it. */
export async function signInIfPrompted(
  page: Page,
  credentials: { username: string; password: string },
  timeoutMs = 20_000,
): Promise<void> {
  const username = page.getByLabel("Username", { exact: true });
  if (await visibleWithin(username, timeoutMs)) {
    await username.fill(credentials.username);
    await page
      .getByLabel("Password", { exact: true })
      .fill(credentials.password);
    await page.getByRole("button", { name: "Sign in" }).click();
  }
  await approveConsentIfRequired(page);
}

/**
 * Completes a real portal password-reset link without creating a new browser
 * context. Used by journeys that then sign in as the created user.
 */
export async function completePasswordResetThroughPortal(
  page: Page,
  resetUrl: string,
  password: string,
): Promise<void> {
  await completePasswordReset(page, resetUrl, password);
}

/**
 * Opens the console as an ordinary username/password account. The harness admin
 * is ensured first so the portal has a real account to sign in against.
 */
export async function openConsoleAsCredentials(
  page: Page,
  runtime: TrellisTestRuntime,
  credentials: { username: string; password: string },
): Promise<void> {
  await runtime.ensureAdmin();
  await openConsole(page, runtime, credentials);
}

/** Sets the initialized administrator's password through its one-time account link. */
export async function completeAdminBootstrapInBrowser(
  page: Page,
  runtime: TrellisTestRuntime,
  credentials: { username: string; password: string } = BROWSER_ADMIN,
): Promise<void> {
  await page.goto(await runtime.bootstrapUrl(), {
    waitUntil: "domcontentloaded",
  });
  await page
    .getByLabel("New password", { exact: true })
    .fill(credentials.password);
  await page
    .getByRole("button", {
      name: "Set password",
      exact: true,
    })
    .click();
  await page
    .getByText("Your new password is set")
    .waitFor({ state: "visible" });
  await page.goto(`${runtime.trellisUrl}/console`, {
    waitUntil: "domcontentloaded",
  });
}

/** Completes console entry whether the portal prompts for sign-in, consent, or attaches directly. */
export async function completeConsoleEntry(
  page: Page,
  credentials: { username: string; password: string },
): Promise<void> {
  const ready = waitForConsoleConnected(page)
    .then(() => "ready" as const)
    .catch(() => "waiting" as const);
  const form = page
    .getByLabel("Username", { exact: true })
    .first()
    .waitFor({ state: "visible", timeout: 60_000 })
    .then(() => "form" as const);
  const approval = page
    .getByRole("button", { name: "Allow", exact: true })
    .waitFor({ state: "visible", timeout: 60_000 })
    .then(() => "approval" as const);

  const outcome = await Promise.race([ready, form, approval]);
  if (outcome === "form") {
    await page
      .getByLabel("Username", { exact: true })
      .fill(credentials.username);
    await page
      .getByLabel("Password", { exact: true })
      .fill(credentials.password);
    await page.getByRole("button", { name: "Sign in" }).click();
  }
  await approveConsentIfRequired(page);
  await waitForConsoleConnected(page);
}

/** Completes a password-reset link in the portal and returns the fixed username when shown. */
export async function completePasswordReset(
  page: Page,
  resetUrl: string,
  password: string,
): Promise<string | undefined> {
  const flowId = new URL(resetUrl).pathname.split("/").filter(Boolean).at(-1);
  const pageUrl = new URL("/login/account/password", resetUrl);
  pageUrl.searchParams.set("flowId", flowId ?? "");
  await page.goto(pageUrl.toString(), { waitUntil: "domcontentloaded" });
  const passwordFields = page.locator('input[autocomplete="new-password"]');
  await passwordFields.first().waitFor({ state: "visible", timeout: 30_000 });
  assert.deepEqual(await passwordFields.count(), 2);
  const usernameField = page.locator('input[autocomplete="username"]');
  let username: string | undefined;
  if (await visibleWithin(usernameField, 5_000)) {
    username = await usernameField.inputValue();
  }
  await passwordFields.first().fill(password);
  await passwordFields.nth(1).fill(password);
  await page.getByRole("button", { name: /password/i }).click();
  await page
    .getByText("Password saved")
    .waitFor({ state: "visible", timeout: 30_000 });
  return username;
}

/** Opens the console and completes any sign-in or consent it prompts for. */ export async function openConsole(
  page: Page,
  runtime: TrellisTestRuntime,
  credentials: { username: string; password: string },
): Promise<void> {
  await page.goto(`${runtime.trellisUrl}/console`, {
    waitUntil: "domcontentloaded",
  });
  await completeConsoleEntry(page, credentials);
}

/** Waits for the authenticated console shell without requiring admin navigation. */
export async function waitForConsoleConnected(
  page: Page,
  timeoutMs = 60_000,
): Promise<void> {
  await page
    .waitForURL(/\/console(\/|$)/, { timeout: timeoutMs })
    .catch(async (cause: unknown) => {
      throw new Error(
        `Console navigation failed at ${page.url()}: ${await page.locator("body").innerText()}`,
        { cause },
      );
    });
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const text = await page
      .locator("body")
      .innerText()
      .catch(() => "");
    if (text.includes("Connected")) return;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error("the console did not reach the connected state");
}

/** Waits for the authenticated console shell with its authorized navigation. */
export async function waitForConsoleReady(
  page: Page,
  timeoutMs = 60_000,
): Promise<void> {
  await waitForConsoleConnected(page, timeoutMs);
  await page
    .getByRole("link", { name: "Overview" })
    .waitFor({ state: "visible", timeout: timeoutMs });
}

/** Creates a local-password Console user with its required non-administrative capabilities. */
export async function createOrdinaryUserWithPassword(
  runtime: TrellisTestRuntime,
  user: { username: string; password: string; name?: string },
) {
  const created = await runtime.callAdminRpc("authUsersCreate", {
    idempotencyKey: ulid(),
    displayName: user.name ?? user.username,
    username: user.username,
    password: user.password,
  });
  const principalId = created.user.principalId;
  for (const capabilityId of [
    "trellis.auth.v1:account.read",
    "trellis.auth.v1:account.edit",
    "trellis.auth.v1:identities.read",
    "trellis.auth.v1:identities.manage",
    "trellis.auth.v1:account.changePassword",
    "trellis.auth.v1:sessions.read",
    "trellis.auth.v1:sessions.revoke",
  ]) {
    await runtime.callAdminRpc("authCapabilityGrantsGrant", {
      principalId,
      capabilityId,
      expectedRevision: 0n,
      idempotencyKey: ulid(),
    });
  }
  return principalId;
}

/**
 * Signs the producer-built CLI in as the runtime administrator through the
 * device flow, allowing everything it asks for, and returns its configuration
 * directory (inside the runtime workdir) and a runner for its JSON commands.
 */
export async function loginCliAsAdministrator(
  runtime: TrellisTestRuntime,
  page: Page,
): Promise<{
  directory: string;
  run: (...args: string[]) => Promise<any>;
}> {
  const binary = process.env.TRELLIS_TEST_CLI_BIN;
  assert.ok(binary, "supply a producer-built CLI");
  const directory = join(runtime.workdir, `cli-${ulid()}`);
  await mkdir(directory);
  const env = { ...process.env, XDG_CONFIG_HOME: directory };
  const login = spawn(
    binary,
    [
      "--format",
      "json",
      "login",
      runtime.oauthIssuer,
      "--ca-certificate",
      runtime.caCertificate,
    ],
    { env, stdio: ["ignore", "pipe", "pipe"] },
  );
  const exited = once(login, "exit");
  const progress = createInterface({ input: login.stderr });
  try {
    let challenge: { loginUrl: string; userCode: string } | undefined;
    for await (const line of progress) {
      if (line.startsWith("{")) {
        challenge = JSON.parse(line);
        break;
      }
    }
    assert.ok(challenge);
    await page.goto(challenge.loginUrl);
    await page.getByLabel("Code", { exact: true }).fill(challenge.userCode);
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await page
      .getByLabel("Username", { exact: true })
      .fill(runtime.adminUsername);
    await page
      .getByLabel("Password", { exact: true })
      .fill(runtime.adminPassword);
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page
      .getByRole("heading", { name: /wants to use your account/ })
      .waitFor();
    for (const checkbox of await page.getByRole("checkbox").all())
      if (await checkbox.isEnabled()) await checkbox.check();
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    await confirmAdministrationIfAsked(page);
    assert.equal((await exited)[0], 0);
  } finally {
    login.kill("SIGTERM");
    progress.close();
  }
  const run = async (...args: string[]) =>
    JSON.parse(
      (
        await promisify(execFile)(binary, ["--format", "json", ...args], {
          env,
          timeout: 15_000,
        })
      ).stdout,
    );
  return { directory, run };
}
