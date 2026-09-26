import { assertEquals } from "@std/assert";
import { ulid } from "ulid";
import { type BrowserContext, chromium, type Page } from "playwright";
import { join } from "@std/path";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import type { TrellisTestRuntimeStartOptions } from "@oatscenter/trellis-testkit";

/** Local administrator credentials created through the browser bootstrap page. */
export const BROWSER_ADMIN = {
  username: "browser-admin",
  password: "browser-admin-password",
};

/** Returns the prebuilt server binary required by browser acceptance tests. */
export function prebuiltServer(): string {
  const server = Deno.env.get("TRELLIS_TEST_SERVER_BIN");
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
    trellis: {
      command: {
        cmd: prebuiltServer(),
        args: ["--config", "{config}", "all"],
      },
    },
  };
}

/** Launches a persistent Chromium profile owned by the test runtime workdir. */
export async function launchProfile(
  runtime: TrellisTestRuntime,
): Promise<BrowserContext> {
  return await chromium.launchPersistentContext(
    join(runtime.workdir, "browser-profile"),
    { headless: true },
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
  const approve = page.getByRole("button", { name: "Approve" });
  if (await visibleWithin(approve, timeoutMs)) {
    await approve.click();
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
    await page.getByLabel("Password", { exact: true }).fill(
      credentials.password,
    );
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

/** Creates the first administrator through the real portal bootstrap page. */
export async function completeAdminBootstrapInBrowser(
  page: Page,
  runtime: TrellisTestRuntime,
  credentials: { username: string; password: string } = BROWSER_ADMIN,
): Promise<void> {
  await page.goto(await runtime.bootstrapUrl(), {
    waitUntil: "domcontentloaded",
  });
  await page.getByLabel("Username", { exact: true }).fill(credentials.username);
  await page.getByLabel("Password", { exact: true }).fill(credentials.password);
  await page.getByLabel("Confirm password").fill(credentials.password);
  await page.getByRole("button", { name: "Create administrator" }).click();
  await page.waitForURL(
    (url) => !url.pathname.startsWith("/login/admin/bootstrap"),
    { timeout: 60_000 },
  );
}

/** Completes console entry whether the portal prompts for sign-in, consent, or attaches directly. */
export async function completeConsoleEntry(
  page: Page,
  credentials: { username: string; password: string },
): Promise<void> {
  const ready = waitForConsoleConnected(page).then(() => "ready" as const)
    .catch(
      () => "waiting" as const,
    );
  const form = page
    .getByLabel("Username", { exact: true })
    .first()
    .waitFor({ state: "visible", timeout: 60_000 })
    .then(() => "form" as const);
  const approval = page
    .getByRole("button", { name: "Approve" })
    .waitFor({ state: "visible", timeout: 60_000 })
    .then(() => "approval" as const);

  const outcome = await Promise.race([ready, form, approval]);
  if (outcome === "form") {
    await page.getByLabel("Username", { exact: true }).fill(
      credentials.username,
    );
    await page.getByLabel("Password", { exact: true }).fill(
      credentials.password,
    );
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
  assertEquals(await passwordFields.count(), 2);
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
  await page.waitForURL(/\/console(\/|$)/, { timeout: timeoutMs });
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const text = await page.locator("body").innerText().catch(() => "");
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

/** Creates an ordinary user through admin RPCs and completes its password setup in the portal. */
export async function createOrdinaryUserWithPassword(
  runtime: TrellisTestRuntime,
  user: { username: string; password: string; name?: string },
): Promise<string> {
  const created = await runtime.callAdminRpc("authUsersCreate", {
    email: null,
    idempotencyKey: ulid(),
    image: null,
    name: user.name ?? user.username,
    username: user.username,
  });
  const userId = created.user.userId;
  const reset = await runtime.callAdminRpc("authUsersPasswordResetCreate", {
    idempotencyKey: ulid(),
    returnTarget: null,
    userId,
  });
  const resetUrl = reset.flow.completionUrl;
  const context = await launchProfile(runtime);
  try {
    const page = await context.newPage();
    await completePasswordReset(page, resetUrl, user.password);
  } finally {
    await context.close();
  }
  return userId;
}
