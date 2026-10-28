import assert from "node:assert/strict";
import { test } from "node:test";
import { types } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { X509Certificate } from "node:crypto";
import { statSync } from "node:fs";
import { mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { isIP } from "node:net";
import { connect } from "node:tls";
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

/** The `kind` record a JSON-mode CLI run reported on stderr. */
function progressRecord(stderr: string, kind: string): Record<string, unknown> {
  for (const line of stderr.split("\n")) {
    if (!line.startsWith("{")) continue;
    const record: unknown = JSON.parse(line);
    if (typeof record === "object" && record !== null && kind in record) {
      const value: unknown = Reflect.get(record, kind);
      assert(typeof value === "object" && value !== null, line);
      return Object.fromEntries(Object.entries(value));
    }
  }
  assert.fail(`no ${kind} record in: ${stderr}`);
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
  await withTrellisRuntime(
    async (runtime) => {
      await runtime.ensureAdmin();
      const configHome = await mkdtemp(join(tmpdir(), "trellis-cli-login-"));
      const ca = new X509Certificate(await readFile(runtime.caCertificate))
        .fingerprint256;

      // No system root vouches for the generated CA, so a first login stops and
      // names the CA it was offered rather than trusting it.
      const untrusted = await runCli(
        ["--format", "json", "login", runtime.oauthIssuer],
        configHome,
      );
      assert.notEqual(untrusted.code, 0);
      assert.equal(
        progressRecord(untrusted.stderr, "untrustedCa")["presented"],
        ca,
      );
      const wrong = await runCli(
        [
          "--format",
          "json",
          "login",
          runtime.oauthIssuer,
          "--ca-fingerprint",
          Array(32).fill("00").join(":"),
        ],
        configHome,
      );
      assert.notEqual(wrong.code, 0);
      assert.deepEqual(await readdir(configHome), []);

      const child = spawnTracked(
        cliBinary(),
        [
          "--format",
          "json",
          "login",
          runtime.oauthIssuer,
          "--ca-fingerprint",
          ca,
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
        const loginUrl = new URL(challenge.loginUrl);
        assert.equal(loginUrl.origin, runtime.oauthIssuer);
        // The printed link and QR code carry the code, so nothing is typed.
        assert.equal(
          loginUrl.searchParams.get("user_code"),
          challenge.userCode,
        );

        const context = await launchProfile(runtime);
        try {
          const page = await context.newPage();
          const pageErrors: string[] = [];
          page.on("pageerror", (error) => pageErrors.push(String(error)));
          await page.goto(loginUrl.href, { waitUntil: "domcontentloaded" });
          const code = page.getByLabel("Code", { exact: true });
          assert.equal(await code.inputValue(), challenge.userCode);
          // A code typed by hand is grouped and uppercased as it is entered.
          await code.clear();
          await code.pressSequentially(
            challenge.userCode.replace("-", "").toLowerCase(),
          );
          assert.equal(await code.inputValue(), challenge.userCode);
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

        // The accepted CA is pinned for this deployment: a later login trusts
        // it without being told, and a different pinned CA is a hard error.
        const again = spawnTracked(
          cliBinary(),
          ["--format", "json", "login", runtime.oauthIssuer],
          { env: { ...process.env, XDG_CONFIG_HOME: configHome } },
        );
        let againProgress = "";
        for await (const line of createInterface({ input: again.stderr })) {
          againProgress += `${line}\n`;
          if (line.includes("loginUrl")) break;
        }
        again.kill("SIGTERM");
        await again.exited;
        assert(againProgress.includes("loginUrl"), againProgress);

        const pins = join(configHome, "trellis/trusted-ca");
        const [pin] = await readdir(pins);
        assert(pin);
        const issuer = new URL(runtime.oauthIssuer);
        const leaf = await new Promise<Buffer>((resolve, reject) => {
          const socket = connect({
            host: issuer.hostname,
            port: Number(issuer.port),
            ...(isIP(issuer.hostname) ? {} : { servername: issuer.hostname }),
            rejectUnauthorized: false,
          });
          socket.once("secureConnect", () => {
            resolve(socket.getPeerCertificate().raw);
            socket.end();
          });
          socket.once("error", reject);
        });
        await writeFile(join(pins, pin), new X509Certificate(leaf).toString());
        const replaced = await runCli(
          ["--format", "json", "login", runtime.oauthIssuer],
          configHome,
        );
        assert.notEqual(replaced.code, 0);
        const mismatch = progressRecord(replaced.stderr, "pinnedCaMismatch");
        assert.equal(mismatch["presented"], ca);
        assert.notEqual(mismatch["pinned"], ca);
      } finally {
        if (child.exitCode === null && child.signalCode === null)
          child.kill("SIGTERM");
        await child.exited;
        await rm(configHome, { recursive: true, force: true });
      }
      // Out of the box, generated TLS relays browser WebSockets at a path on the
      // Console origin, which native discovery must accept.
    },
    browserRuntimeOptions({ sameOriginWebsocket: true }),
  );
});
