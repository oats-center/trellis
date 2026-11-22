import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { promisify } from "node:util";
import { ulid } from "ulid";
import type { Page } from "playwright";
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
  let loginUrl: string | undefined;
  for await (const line of progress) {
    if (line.startsWith("{")) {
      loginUrl = JSON.parse(line).loginUrl;
      break;
    }
  }
  assert.ok(loginUrl);
  await page.goto(loginUrl);
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

      // A registered app that needs nothing new asks before it continues as
      // the signed-in person, then signs them in without the consent page.
      await page.goto(`${runtime.consoleUrl}/console`);
      await page
        .getByRole("heading", { name: /^Continue to / })
        .waitFor({ state: "visible" });
      await page.getByRole("link", { name: /^Continue as / }).click();
      await waitForConsoleConnected(page);

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
