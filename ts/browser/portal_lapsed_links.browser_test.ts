import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { ulid } from "ulid";
import type { Page } from "playwright";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  enterDeviceCode,
  launchProfile,
} from "./browser_test_support.ts";

/**
 * Continues as the signed-in account, submits the code page, and returns the
 * answer the browser showed.
 */
async function enterCode(page: Page, url: string, code: string) {
  await page.goto(url);
  await page.getByRole("link", { name: "Continue", exact: true }).click();
  await page.getByLabel("Code", { exact: true }).fill(code);
  const answer = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/portal/device",
  );
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  const response = await answer;
  await page.getByRole("heading", { name: "Check the code" }).waitFor();
  return {
    status: response.status(),
    type: response.headers()["content-type"],
    main: await page.locator("main").innerHTML(),
  };
}

test("expired, used and unknown Portal links answer a browser with a page, and API callers with JSON", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const binary = process.env.TRELLIS_TEST_CLI_BIN;
    assert.ok(binary, "supply a producer-built CLI");
    const directory = join(runtime.workdir, `cli-${ulid()}`);
    await mkdir(directory);
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
      {
        env: { ...process.env, XDG_CONFIG_HOME: directory },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );
    const exited = once(login, "exit");
    const progress = createInterface({ input: login.stderr });
    // Read the link before anything else is awaited: lines the CLI prints
    // before iteration starts would be lost.
    let challenge: { loginUrl: string; userCode: string } | undefined;
    for await (const line of progress) {
      if (line.startsWith("{")) {
        challenge = JSON.parse(line);
        break;
      }
    }
    const context = await launchProfile(runtime);
    try {
      assert.ok(challenge);
      const { loginUrl, userCode } = challenge;
      const page = await context.newPage();
      page.setDefaultTimeout(30_000);
      await enterDeviceCode(page, loginUrl, userCode, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      await page.getByRole("button", { name: "Allow", exact: true }).click();
      const management = page.getByRole("button", {
        name: "Allow management",
        exact: true,
      });
      const connected = page.getByText("Your device is connected");
      if (
        (await Promise.race([
          management.waitFor().then(() => "management"),
          connected.waitFor().then(() => "connected"),
        ])) === "management"
      )
        await management.click();
      await connected.waitFor();
      assert.equal((await exited)[0], 0);

      // The used CLI link and a code that never existed get the same page.
      const used = await enterCode(page, loginUrl, userCode);
      assert.match(used.type ?? "", /^text\/html/);
      assert.match(
        used.main,
        /This code isn't valid\. Check the code on your device, or start again on the device\./,
      );
      const unknown = await enterCode(
        page,
        `${runtime.trellisUrl}/device`,
        "ZZZZ-ZZZZ",
      );
      assert.deepEqual(unknown, used);
      await page.getByRole("link", { name: "Enter a code" }).click();
      await page.getByRole("heading", { name: "Device activation" }).waitFor();

      // A sign-in link whose transaction is gone explains itself in a page.
      const gone = await page.goto(
        `${runtime.trellisUrl}/login?transaction=${ulid()}`,
      );
      assert.match(gone?.headers()["content-type"] ?? "", /^text\/html/);
      await page
        .getByRole("heading", { name: "This sign-in link has expired" })
        .waitFor();
      await page
        .getByText(
          "This sign-in link has expired or was already used. Start the sign-in again from the app.",
        )
        .waitFor();
      assert.doesNotMatch(await page.locator("body").innerText(), /"error"/);

      // The same refusals still answer the Portal's own API calls with JSON.
      await page.goto(`${runtime.trellisUrl}/device`);
      await page.getByRole("heading", { name: "Device activation" }).waitFor();
      const api = await page.evaluate(async (transaction) => {
        const csrf =
          document.cookie
            .split("; ")
            .find((cookie) => cookie.startsWith("__Host-trellis-portal-csrf="))
            ?.split("=")[1] ?? "";
        const device = await fetch("/portal/device", {
          method: "POST",
          body: new URLSearchParams({
            csrf,
            transaction:
              new URL(location.href).searchParams.get("transaction") ?? "",
            user_code: "ZZZZ-ZZZZ",
          }),
        });
        const consent = await fetch(`/portal/authorization/${transaction}`);
        return [
          device.status,
          (await device.json()).error,
          (await consent.json()).error,
        ];
      }, ulid());
      assert.deepEqual(api, [
        used.status,
        "invalid_request",
        "invalid_request",
      ]);
    } finally {
      login.kill("SIGTERM");
      progress.close();
      await context.close();
    }
  }, browserRuntimeOptions());
});
