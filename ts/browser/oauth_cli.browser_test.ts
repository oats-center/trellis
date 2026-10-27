import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { promisify } from "node:util";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { ulid } from "ulid";
import { participants } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  confirmAdministrationIfAsked,
} from "./browser_test_support.ts";

test(
  "CLI explicitly consents to administration, resumes custody, and manages clients, catalog revisions, Portal themes, and roles for new users",
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
      const directory = await mkdtemp(join(tmpdir(), "cli-management-"));
      const env = { ...process.env, XDG_CONFIG_HOME: directory };
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      const page = await browser.newPage();
      page.setDefaultTimeout(10_000);
      const login = spawn(
        binary,
        [
          "--format",
          "json",
          "login",
          issuer,
          "--ca-certificate",
          runtime.caCertificate,
        ],
        {
          env,
          stdio: ["ignore", "pipe", "pipe"],
        },
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
        await page
          .getByRole("button", { name: "Continue", exact: true })
          .click();
        await page.getByLabel("Username", { exact: true }).fill(username);
        await page.getByLabel("Password", { exact: true }).fill(password);
        await page
          .getByRole("button", { name: "Sign in", exact: true })
          .click();
        await page
          .getByRole("heading", { name: /wants to use your account/ })
          .waitFor();
        for (const checkbox of await page.getByRole("checkbox").all())
          if (await checkbox.isEnabled()) await checkbox.check();
        await page.getByRole("button", { name: "Allow", exact: true }).click();
        await confirmAdministrationIfAsked(page);
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
        const source = [
          "--source",
          new URL("../../integration/fixtures/client-upgrade", import.meta.url)
            .pathname,
          "--participant",
          provider.id,
        ];
        // A deployment needs its implemented API accepted, and says which.
        await assert.rejects(
          run("svc", "provider", "create", ...source),
          (error: Error) =>
            // The terminal report wraps the message in a box.
            error.message
              .replace(/[\s│]+/g, " ")
              .includes(`API ${apiId} is not accepted`),
        );
        const { reviews } = await run("apis", "review", ...source);
        assert.deepEqual(
          reviews.map((review: { apiId: string }) => review.apiId),
          [apiId],
        );
        const { apis } = await run("apis", "accept", ...source);
        assert.equal(apis[0].canonicalDigest, reviews[0].canonicalDigest);
        const accepted = await run("apis", "get", apiId);
        assert.equal(accepted.api.canonicalDigest, reviews[0].canonicalDigest);
        // Accepting the same definition again changes nothing.
        const again = await run("apis", "accept", ...source);
        assert.equal(again.apis[0].acceptedRevision, apis[0].acceptedRevision);
        const { deployment } = await run(
          "svc",
          "provider",
          "create",
          ...source,
        );
        assert.equal(deployment.participantId, provider.id);
        const logo = `${directory}/logo.svg`;
        await writeFile(logo, '<svg xmlns="http://www.w3.org/2000/svg"/>');
        const themed = await run(
          "theme",
          "set",
          "--client",
          "trellis.console",
          "--name",
          "Acme Console",
          "--logo",
          logo,
          "--color",
          "primary-hover=#003a80",
          "--copy",
          "footer=Acme support",
        );
        assert.equal(themed.theme.colors.primaryHover, "#003a80");
        const shown = await run("theme", "show", "--client", "trellis.console");
        assert.equal(shown.theme.name, "Acme Console");
        assert.equal(shown.theme.logo.format, "svg");
        assert.equal(shown.theme.copy.footer, "Acme support");
        // An app theme cannot restyle the Portal for everyone.
        await assert.rejects(
          run(
            "theme",
            "set",
            "--client",
            "trellis.console",
            "--stylesheet",
            logo,
          ),
        );
        await run("theme", "remove", "--client", "trellis.console");
        assert.equal(
          (await run("theme", "show", "--client", "trellis.console")).theme,
          undefined,
        );
        // A role marked for new users is given to each user created while the
        // mark stands, and to nobody created after it is cleared.
        const { role } = await run(
          "roles",
          "create",
          "--title",
          "Readers",
          "--capability",
          "trellis.auth.v1:users.read",
          "--new-users",
        );
        const createUser = async () =>
          (await run("users", "create", "--username", `cli-${ulid()}`)).user
            .principalId;
        const before = await createUser();
        const renamed = await run(
          "roles",
          "edit",
          role.roleId,
          "--title",
          "Directory readers",
          "--no-new-users",
        );
        assert.deepEqual(
          renamed.role.capabilities.map(
            (capability: { capabilityId: string }) => capability.capabilityId,
          ),
          ["trellis.auth.v1:users.read"],
        );
        await createUser();
        const readers = await run("roles", "show", role.roleId);
        assert.equal(readers.role.title, "Directory readers");
        assert.equal(readers.role.assignToNewUsers, false);
        assert.deepEqual(readers.members, [before]);
        await run("roles", "delete", role.roleId);
        await assert.rejects(run("roles", "show", role.roleId));
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
