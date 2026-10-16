import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { promisify } from "node:util";
import { chromium } from "playwright";
import { ulid } from "ulid";
import { participants } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";

test(
  "CLI explicitly consents to administration, resumes custody, and manages clients and catalog revisions",
  { timeout: 90_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const issuer = runtime.oauthIssuer;
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;
      const binary = process.env.TRELLIS_TEST_CLI_BIN;
      assert.ok(
        issuer && username && password && binary,
        "supply an ordinary runtime, administrator, and producer-built CLI",
      );
      const directory = await mkdtemp("/tmp/opencode/cli-management-");
      const env = { ...process.env, XDG_CONFIG_HOME: directory };
      const browser = await chromium.launch({ headless: true });
      const page = await browser.newPage();
      page.setDefaultTimeout(10_000);
      const login = spawn(binary, ["--format", "json", "login", issuer], {
        env,
        stdio: ["ignore", "pipe", "pipe"],
      });
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
        await page
          .getByLabel("Device code", { exact: true })
          .fill(challenge.userCode);
        await page
          .getByRole("button", { name: "Review device request" })
          .click();
        await page.getByLabel("Username", { exact: true }).fill(username);
        await page.getByLabel("Password", { exact: true }).fill(password);
        await page
          .getByRole("button", { name: "Sign in", exact: true })
          .click();
        await page
          .getByRole("heading", { name: /Allow .* to access Trellis/ })
          .waitFor();
        for (const checkbox of await page.getByRole("checkbox").all())
          if (await checkbox.isEnabled()) await checkbox.check();
        await page
          .getByRole("button", { name: "Authorize application" })
          .click();
        assert.equal((await exited)[0], 0);
        const run = async (...args: string[]) =>
          JSON.parse(
            (
              await promisify(execFile)(binary, ["--format", "json", ...args], {
                env,
                timeout: 15_000,
              })
            ).stdout,
          );
        const clientId = `cli-development-${ulid()}`;
        const requestPath = `${directory}/client.json`;
        await writeFile(
          requestPath,
          JSON.stringify({
            clientId,
            expectedRevision: "0",
            title: clientId,
            description: "CLI browser development",
            kind: "browser",
            development: true,
            redirectUris: ["http://localhost:49761/callback"],
            impliedConsent: [],
            idempotencyKey: ulid(),
          }),
        );
        const created = await run("clients", "put", requestPath);
        const loaded = await run("clients", "get", clientId);
        assert.equal(loaded.client.title, clientId);
        assert.equal(loaded.client.revision, created.client.revision);
        // The second process must resume saved OAuth custody; a stale mutation must
        // not overwrite the stored registration.
        await writeFile(
          requestPath,
          JSON.stringify({
            clientId,
            expectedRevision: "0",
            title: "Stale overwrite",
            description: "",
            kind: "browser",
            development: true,
            redirectUris: ["http://localhost:49761/callback"],
            impliedConsent: [],
            idempotencyKey: ulid(),
          }),
        );
        await assert.rejects(run("clients", "put", requestPath));
        assert.equal(
          (await run("clients", "get", clientId)).client.title,
          clientId,
        );
        const disabled = await run(
          "clients",
          "disable",
          clientId,
          "--expected-revision",
          loaded.client.revision,
        );
        assert.equal(
          (await run("clients", "get", clientId)).client.enabled,
          false,
        );
        await run(
          "clients",
          "delete",
          clientId,
          "--expected-revision",
          disabled.revision,
        );
        await assert.rejects(run("clients", "get", clientId));
        const provider = participants.Provider.participant;
        const apiId = provider.implements[0].identity;
        const reviewPath = `${directory}/review.json`;
        await writeFile(
          reviewPath,
          JSON.stringify({ apiId, packageEvidence: provider.packageEvidence }),
        );
        const { review } = await run("apis", "review", reviewPath);
        const acceptPath = `${directory}/accept.json`;
        await writeFile(
          acceptPath,
          JSON.stringify({
            reviewId: review.reviewId,
            canonicalDigest: review.canonicalDigest,
            expectedGeneration: review.currentGeneration ?? "0",
            expectedAcceptedRevision: review.currentAcceptedRevision ?? "0",
            roleEdits: [],
            mappingEdits: [],
            idempotencyKey: ulid(),
          }),
        );
        await run("apis", "accept", acceptPath);
        const accepted = await run("apis", "get", apiId);
        assert.equal(accepted.api.apiId, apiId);
        assert.equal(accepted.api.canonicalDigest, review.canonicalDigest);
        await run("logout");
        await assert.rejects(run("clients", "list"));
      } finally {
        login.kill("SIGTERM");
        progress.close();
        await browser.close();
        await rm(directory, { recursive: true, force: true });
      }
    }, browserRuntimeOptions());
  },
);
