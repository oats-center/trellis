import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir } from "node:fs/promises";
import { hostname } from "node:os";
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
} from "./browser_test_support.ts";

/** Starts `trellis login` in its own configuration and reads its challenge. */
async function startCliLogin(runtime: TrellisTestRuntime) {
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
  const finished = async () => {
    try {
      return (await exited)[0] as number | null;
    } finally {
      progress.close();
    }
  };
  return {
    ...challenge,
    /** The principal the CLI's stored login authenticates as. */
    async principal(): Promise<string> {
      assert.equal(await finished(), 0);
      const { stdout } = await promisify(execFile)(
        binary,
        ["--format", "json", "whoami"],
        { env, timeout: 15_000 },
      );
      return JSON.parse(stdout).principal.principalId;
    },
    /** The CLI's exit code once it gives up or finishes. */
    exitCode: finished,
    stop: () => login.kill("SIGTERM"),
  };
}

/** Types `code` on the code step and continues. */
async function typeCode(page: Page, code: string): Promise<void> {
  await page.getByRole("heading", { name: "Connect a device" }).waitFor();
  await page.getByLabel("Code", { exact: true }).fill(code);
  await page.getByRole("button", { name: "Continue", exact: true }).click();
}

/** Checks the device details the confirmation step shows for the CLI. */
async function checkDetails(page: Page, userCode: string): Promise<void> {
  await page.getByRole("heading", { name: "Is this your device?" }).waitFor();
  const details = page.locator(".device-details");
  const detail = (term: string) =>
    details.locator(`dt:text-is("${term}") + dd`).innerText();
  assert.equal(await detail("App"), "Trellis CLI");
  assert.match(await detail("Kind"), /^Installed app/);
  assert.equal(await detail("Code"), userCode);
  assert.match(await detail("Requested"), /^(Just now|1 minute ago)$/);
  // The CLI asked from this machine, and names it by its hostname.
  assert.match(await detail("From"), /^(127\.0\.0\.1|::1)$/);
  assert.equal(await detail("Device says it is"), hostname());
}

/** Allows what the consent page offers and waits for the done page. */
async function allow(page: Page): Promise<void> {
  await page
    .getByRole("heading", { name: /wants to use your account/ })
    .waitFor();
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  await page
    .getByRole("heading", { name: "Your device is connected" })
    .waitFor();
  await page.getByText("You can close this window.", { exact: true }).waitFor();
}

test("a Device Code sign-in takes the account, then the code, then the device's details, then the consent", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const avery = { username: "avery", password: "avery-portal-password" };
    const averyId = await createOrdinaryUserWithPassword(runtime, {
      ...avery,
      name: "Avery Able",
    });
    const context = await launchProfile(runtime);
    const logins: { stop(): void }[] = [];
    try {
      const page = await context.newPage();
      page.setDefaultTimeout(30_000);

      // Not signed in: the device's link signs in first, then lands straight
      // on the code step.
      const first = await startCliLogin(runtime);
      logins.push(first);
      await page.goto(first.loginUrl);
      await page.getByLabel("Username", { exact: true }).fill(avery.username);
      await page.getByLabel("Password", { exact: true }).fill(avery.password);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
      await typeCode(page, first.userCode);
      await checkDetails(page, first.userCode);
      await page
        .getByRole("button", { name: "Yes, this is my device", exact: true })
        .click();
      await allow(page);
      assert.equal(await first.principal(), averyId);

      // Signed in: the link shows who, and continues as them on request.
      const second = await startCliLogin(runtime);
      logins.push(second);
      await page.goto(second.loginUrl);
      await page.getByRole("heading", { name: "Device activation" }).waitFor();
      const account = page.locator(".account");
      await account.getByText("Avery Able", { exact: true }).waitFor();
      await account.getByText("avery", { exact: true }).waitFor();
      // Initials stand in for a profile picture the account doesn't have.
      assert.equal(await account.locator(".avatar").innerText(), "AA");
      await page.getByRole("link", { name: "Continue", exact: true }).click();
      const codeStep = new URL(page.url());
      // The consent can't be reached before the code: it leads back here.
      await page.goto(
        `${runtime.trellisUrl}/consent?${codeStep.searchParams.toString()}`,
      );
      await page.getByRole("heading", { name: "Connect a device" }).waitFor();
      await typeCode(page, second.userCode);
      await checkDetails(page, second.userCode);
      // Nor before the device is confirmed: the consent page leads back to
      // the details, and the Portal refuses an approval sent anyway.
      await page.goto(
        `${runtime.trellisUrl}/consent?${codeStep.searchParams.toString()}`,
      );
      await page
        .getByRole("heading", { name: "Is this your device?" })
        .waitFor();
      const skipped = await page.evaluate(
        async (transaction) => {
          const csrf =
            document.cookie
              .split("; ")
              .find((cookie) =>
                cookie.startsWith("__Host-trellis-portal-csrf="),
              )
              ?.split("=")[1] ?? "";
          const approve = await fetch("/portal/approve", {
            method: "POST",
            body: new URLSearchParams({
              csrf,
              transaction,
              decision: "approve",
            }),
          });
          const consent = await fetch(`/portal/authorization/${transaction}`);
          return [approve.status, consent.status];
        },
        codeStep.searchParams.get("transaction") ?? "",
      );
      assert.deepEqual(skipped, [400, 400]);
      await page
        .getByRole("button", { name: "Yes, this is my device", exact: true })
        .click();
      await allow(page);
      assert.equal(await second.principal(), averyId);

      // Unknown and used codes get one identical page.
      const invalid = async (code: string) => {
        await page.goto(`${runtime.trellisUrl}/device`);
        await page.getByRole("link", { name: "Continue", exact: true }).click();
        await typeCode(page, code);
        await page.getByRole("heading", { name: "Check the code" }).waitFor();
        return await page.locator("main").innerHTML();
      };
      const used = await invalid(second.userCode);
      assert.match(used, /This code isn't valid\./);
      assert.equal(await invalid("ZZZZ-ZZZZ"), used);

      // Cancelling at the device's details gives the CLI nothing.
      const cancelled = await startCliLogin(runtime);
      logins.push(cancelled);
      await page.goto(cancelled.loginUrl);
      await page.getByRole("link", { name: "Continue", exact: true }).click();
      await typeCode(page, cancelled.userCode);
      await checkDetails(page, cancelled.userCode);
      await page.getByRole("button", { name: "Cancel", exact: true }).click();
      await page
        .getByRole("heading", { name: "Device not connected" })
        .waitFor();
      assert.notEqual(await cancelled.exitCode(), 0);
    } finally {
      for (const login of logins) login.stop();
      await context.close();
    }
  }, browserRuntimeOptions());
});
