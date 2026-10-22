import assert from "node:assert/strict";
import { test } from "node:test";
import { types } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { statSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { text } from "node:stream/consumers";
import { createInterface } from "node:readline";
import { spawnTracked } from "../integration/_support/process.ts";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  approveConsentIfRequired,
  browserRuntimeOptions,
  launchProfile,
  signInIfPrompted,
} from "./browser_test_support.ts";

function cliBinary(): string {
  const binary = process.env["TRELLIS_TEST_CLI_BIN"] ?? "target/debug/trellis";
  try {
    statSync(binary);
  } catch {
    throw new Error(
      `the trellis CLI binary is missing at ${binary}; build it or set TRELLIS_TEST_CLI_BIN`,
    );
  }
  return binary;
}

function jsonOutput(stdout: string): Record<string, unknown> {
  return JSON.parse(
    stdout.slice(stdout.indexOf("{"), stdout.lastIndexOf("}") + 1),
  );
}

async function runCli(
  args: readonly string[],
  configHome: string,
): Promise<{ code: number; stdout: string; stderr: string }> {
  const child = spawnTracked(cliBinary(), args, {
    env: { ...process.env, XDG_CONFIG_HOME: configHome },
  });
  const [status, stdout, stderr] = await Promise.all([
    child.exited,
    text(child.stdout),
    text(child.stderr),
  ]);
  return { code: status.code ?? 1, stdout, stderr };
}

test("CLI login authorizes independent runtime keys through one durable login", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const configHome = await mkdtemp(join(tmpdir(), "trellis-cli-login-"));

    const child = spawnTracked(
      cliBinary(),
      [
        "--format",
        "json",
        "login",
        runtime.oauthIssuer,
        "--ca-certificate",
        runtime.caCertificate,
      ],
      { env: { ...process.env, XDG_CONFIG_HOME: configHome } },
    );
    const stdout = text(child.stdout);
    try {
      let progress = "";
      const progressReader = createInterface({ input: child.stderr })[
        Symbol.asyncIterator
      ]();
      while (!progress.includes("loginUrl")) {
        const chunk = await progressReader.next();
        assert(
          !chunk.done,
          `CLI exited before printing a login URL: ${progress}`,
        );
        progress += `${chunk.value}\n`;
      }
      const challenge = JSON.parse(
        progress
          .trim()
          .split("\n")
          .find((line) => line.includes("loginUrl"))!,
      );
      const loginUrl = challenge.loginUrl;
      assert.equal(new URL(loginUrl).origin, runtime.oauthIssuer);

      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const pageErrors: string[] = [];
        page.on("pageerror", (error) => pageErrors.push(String(error)));
        await page.goto(loginUrl, { waitUntil: "domcontentloaded" });
        await page
          .getByLabel("Device code", { exact: true })
          .fill(challenge.userCode);
        await page.getByRole("button", { name: /continue|review/i }).click();
        await signInIfPrompted(page, {
          username: runtime.adminUsername,
          password: runtime.adminPassword,
        });
        await approveConsentIfRequired(page, 15_000);
        assert.deepEqual(pageErrors, []);
      } finally {
        await context.close();
      }

      while (true) {
        const chunk = await progressReader.next();
        if (chunk.done) break;
        progress += `${chunk.value}\n`;
      }
      assert.deepEqual((await child.exited).code, 0, progress);
      const login = jsonOutput(await stdout);
      assert(login.principalId, JSON.stringify(login));

      // Every `whoami` run opens its own connection with a runtime key it
      // generates locally and calls the deployment-bound Sessions.Me through the
      // stored durable login, so both runs must authenticate the same user.
      const first = await runCli(["--format", "json", "whoami"], configHome);
      assert.deepEqual(first.code, 0, first.stderr);
      assert.deepEqual(
        types.AuthSessionsMeResponseCodec.decode(jsonOutput(first.stdout))
          .principal.principalId,
        login.principalId,
      );

      const second = await runCli(["--format", "json", "whoami"], configHome);
      assert.deepEqual(second.code, 0, second.stderr);
      assert.deepEqual(
        types.AuthSessionsMeResponseCodec.decode(jsonOutput(second.stdout))
          .principal.principalId,
        login.principalId,
      );
    } finally {
      if (child.exitCode === null && child.signalCode === null)
        child.kill("SIGTERM");
      await child.exited;
      await rm(configHome, { recursive: true, force: true });
    }
  }, browserRuntimeOptions());
});
