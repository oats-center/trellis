import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { promisify } from "node:util";
import { ulid } from "ulid";
import { chromium, type Frame, type Page } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  loginCliAsAdministrator,
  waitForConsoleConnected,
} from "./browser_test_support.ts";

/**
 * Starts `trellis login` in its own configuration and opens its printed link
 * in `page`, up to the point the Portal shows what comes after the code.
 */
async function openCliLogin(runtime: TrellisTestRuntime, page: Page) {
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
      runtime.trellisUrl,
      "--ca-certificate",
      runtime.caCertificatePath,
    ],
    { env, stdio: ["ignore", "pipe", "pipe"] },
  );
  const exited = once(login, "exit");
  const progress = createInterface({ input: login.stderr });
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
  return {
    /** The principal the CLI signed in as, once it finishes. */
    async principal(): Promise<string> {
      try {
        assert.equal((await exited)[0], 0);
      } finally {
        login.kill("SIGTERM");
        progress.close();
      }
      const { stdout } = await promisify(execFile)(
        binary,
        ["--format", "json", "whoami"],
        { env, timeout: 15_000 },
      );
      return JSON.parse(stdout).principal.principalId;
    },
  };
}

/** Allows everything the consent page offers that the person may grant. */
async function allow(page: Page): Promise<void> {
  for (const checkbox of await page.getByRole("checkbox").all())
    if (await checkbox.isEnabled()) await checkbox.check();
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  const management = page.getByRole("button", {
    name: "Allow management",
    exact: true,
  });
  const done = page.getByText("Your device is connected");
  if (
    (await Promise.race([
      management.waitFor().then(() => "management"),
      done.waitFor().then(() => "done"),
    ])) === "management"
  )
    await management.click();
  await done.waitFor();
}

test("someone already signed in to the Portal sees who, and can sign a CLI in as someone else", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const other = { username: "dana", password: "dana-portal-password" };
    const danaId = await createOrdinaryUserWithPassword(runtime, {
      ...other,
      name: "Dana Other",
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      page.setDefaultTimeout(30_000);
      // The administrator signs in once; this browser stays signed in.
      const first = await loginCliAsAdministrator(runtime, page);
      const adminId = (await first.run("whoami")).principal.principalId;

      // A registered app with nothing left to decide signs the signed-in
      // person straight in: no Portal page shows on the way.
      const shown: string[] = [];
      const record = (frame: Frame) => {
        if (frame === page.mainFrame()) shown.push(new URL(frame.url()).href);
      };
      page.on("framenavigated", record);
      await page.goto(`${runtime.consoleUrl}/console`);
      await waitForConsoleConnected(page);
      page.off("framenavigated", record);
      assert(
        !shown.some(
          (url) =>
            new URL(url).origin === runtime.trellisUrl &&
            ["/consent", "/login"].includes(new URL(url).pathname),
        ),
        shown.join(" "),
      );

      // Unless it asks to choose (`prompt=select_account`): then it asks
      // whether to continue as the signed-in person, and does on request.
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      try {
        const asking = await browser.newContext();
        await asking.addCookies(
          (await context.cookies()).filter((cookie) =>
            cookie.name.startsWith("__Host-trellis-portal"),
          ),
        );
        const choosing = await asking.newPage();
        choosing.setDefaultTimeout(30_000);
        await choosing.route(
          (url) => url.pathname === "/authorize",
          (route) => {
            const url = new URL(route.request().url());
            url.searchParams.set("prompt", "select_account");
            return route.continue({ url: url.href });
          },
        );
        await choosing.goto(`${runtime.consoleUrl}/console`);
        await choosing
          .getByRole("heading", { name: /^Continue to / })
          .waitFor();
        await choosing.getByRole("link", { name: /^Continue as / }).click();
        await waitForConsoleConnected(choosing);
      } finally {
        await browser.close();
      }

      // Another CLI link shows the signed-in administrator. Choosing another
      // account signs in again for the same request, which the CLI receives.
      const switched = await openCliLogin(runtime, page);
      await page
        .getByText(new RegExp(`^Signed in as .*\\b${runtime.adminUsername}\\b`))
        .waitFor({ state: "visible" });
      await page
        .getByRole("button", { name: "Use a different account" })
        .click();
      await page.getByLabel("Username", { exact: true }).fill(other.username);
      await page.getByLabel("Password", { exact: true }).fill(other.password);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
      await page
        .getByText("Signed in as Dana Other (dana)")
        .waitFor({ state: "visible" });
      await allow(page);
      assert.equal(await switched.principal(), danaId);

      // Without choosing, the next sign-in shows and continues as whoever is
      // signed in now.
      const kept = await openCliLogin(runtime, page);
      await page
        .getByText("Signed in as Dana Other (dana)")
        .waitFor({ state: "visible" });
      await allow(page);
      assert.equal(await kept.principal(), danaId);
      assert.notEqual(danaId, adminId);

      // The administrator's own CLI login is untouched by the switch.
      assert.equal((await first.run("whoami")).principal.principalId, adminId);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
