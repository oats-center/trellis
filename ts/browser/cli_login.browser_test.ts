import { assert, assertEquals } from "@std/assert";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  approveConsentIfRequired,
  browserRuntimeOptions,
  launchProfile,
  signInIfPrompted,
} from "./browser_test_support.ts";

function cliBinary(): string {
  const binary = Deno.env.get("TRELLIS_TEST_CLI_BIN") ??
    "rust/target/debug/trellis";
  try {
    Deno.statSync(binary);
  } catch {
    throw new Error(
      `the trellis CLI binary is missing at ${binary}; build it or set TRELLIS_TEST_CLI_BIN`,
    );
  }
  return binary;
}

type RuntimeLike = Parameters<typeof launchProfile>[0];

function jsonOutput(stdout: string): Record<string, unknown> {
  return JSON.parse(
    stdout.slice(stdout.indexOf("{"), stdout.lastIndexOf("}") + 1),
  );
}

async function runCli(
  args: readonly string[],
  configHome: string,
): Promise<{ code: number; stdout: string; stderr: string }> {
  const child = new Deno.Command(cliBinary(), {
    args: [...args],
    env: { XDG_CONFIG_HOME: configHome },
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const [status, stdout, stderr] = await Promise.all([
    child.status,
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
  ]);
  return { code: status.code, stdout, stderr };
}

Deno.test("CLI login authorizes independent runtime keys through one durable login", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const configHome = await Deno.makeTempDir({ prefix: "trellis-cli-login-" });

    const child = new Deno.Command(cliBinary(), {
      args: ["--format", "json", "login", runtime.trellisUrl],
      env: { XDG_CONFIG_HOME: configHome },
      stdout: "piped",
      stderr: "piped",
    }).spawn();

    let progress = "";
    const progressReader = child.stderr.pipeThrough(new TextDecoderStream())
      .getReader();
    while (!progress.includes("loginUrl")) {
      const chunk = await progressReader.read();
      assert(
        !chunk.done,
        `CLI exited before printing a login URL: ${progress}`,
      );
      progress += chunk.value;
    }
    const loginUrl = JSON.parse(
      progress.trim().split("\n").find((line) => line.includes("loginUrl"))!,
    ).loginUrl as string;
    assert(loginUrl.startsWith(runtime.trellisUrl));

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const pageErrors: string[] = [];
      page.on("pageerror", (error) => pageErrors.push(String(error)));
      await page.goto(loginUrl, { waitUntil: "domcontentloaded" });
      await signInIfPrompted(page, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      await approveConsentIfRequired(page, 15_000);
      assertEquals(pageErrors, []);
    } finally {
      await context.close();
    }

    while (true) {
      const chunk = await progressReader.read();
      if (chunk.done) break;
      progress += chunk.value;
    }
    assertEquals((await child.status).code, 0, progress);
    const login = jsonOutput(
      new TextDecoder().decode(await new Response(child.stdout).arrayBuffer()),
    );
    assert(login.sessionKey, JSON.stringify(login));
    assert(login.userId, JSON.stringify(login));

    // Every `whoami` run opens its own connection with a runtime key it
    // generates locally and calls the deployment-bound Sessions.Me through the
    // stored durable login, so both runs must authenticate the same user.
    const first = await runCli(["--format", "json", "whoami"], configHome);
    assertEquals(first.code, 0, first.stderr);
    assertEquals(jsonOutput(first.stdout).userId, login.userId);

    const second = await runCli(["--format", "json", "whoami"], configHome);
    assertEquals(second.code, 0, second.stderr);
    assertEquals(jsonOutput(second.stdout).userId, login.userId);
  }, browserRuntimeOptions());
});
