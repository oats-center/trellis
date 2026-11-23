import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readdir } from "node:fs/promises";
import { basename, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { chromium } from "playwright";
import { ulid } from "ulid";

import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";
import { openConsoleAsRuntimeAdmin } from "./console_test_support.ts";
import { startDex } from "./dex_support.ts";

// An OIDC provider that went away after an administrator saved it: starting a
// sign-in with it fails on the server. The Portal page must name a reference,
// and trellis-server must have logged exactly one line under it, with the
// cause the browser is not shown. This catches a sign-in failure that answers
// a bare "server_error" page, or that logs nothing an operator can find.
test(
  "a sign-in the server cannot start shows the reference of its one log line",
  { timeout: 300_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const dex = await startDex({
        directory: runtime.workdir,
        redirectUri: `${runtime.trellisUrl}/portal/oidc/callback`,
        emails: ["someone@example.com"],
      });
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      let dexStopped = false;
      try {
        // Saving checks the issuer, so the provider is reachable now.
        await runtime.callAdminRpc("authOIDCProvidersPut", {
          title: "Company",
          issuer: dex.issuer,
          clientId: dex.clientId,
          clientSecret: dex.clientSecret,
          certificateFingerprint: dex.fingerprint,
          scopes: [],
          roleClaim: "",
          claimFreshnessSeconds: 3600,
          expectedRevision: 0n,
          idempotencyKey: codecs.ulid.decode(ulid()),
        });
        await dex.stop();
        dexStopped = true;

        const page = await (await browser.newContext()).newPage();
        page.setDefaultTimeout(30_000);
        await page.goto(`${runtime.consoleUrl}/console`, {
          waitUntil: "domcontentloaded",
        });
        await page.getByRole("link", { name: "Continue with Company" }).click();
        const alert = page.getByRole("alert").filter({ hasText: "Reference:" });
        await alert.waitFor();
        const text = (await alert.textContent()) ?? "";
        assert(text.includes("didn't finish"), text);
        const reference = /Reference: ([0-9A-HJKMNP-TV-Z]{26})/.exec(text)?.[1];
        assert(reference, text);

        await runtime.waitFor(() =>
          runtime.controlPlaneOutput().includes(reference),
        );
        const lines = runtime
          .controlPlaneOutput()
          .split("\n")
          .filter((line) => line.includes(reference));
        assert.equal(lines.length, 1, lines.join("\n"));
        const line = JSON.parse(lines[0]!) as {
          level: string;
          fields: Record<string, unknown>;
        };
        assert.equal(line.level, "ERROR");
        assert.equal(line.fields.error_id, reference);
        assert.equal(line.fields.request_id, reference);
        assert.equal(line.fields.code, "server_error");
        assert.match(
          String(line.fields.route),
          /^GET \/portal\/oidc\/.+\/begin$/,
        );
        assert(
          String(line.fields.error).includes(dex.issuer),
          String(line.fields.error),
        );
        // The cause names why the provider could not be reached; the page
        // never shows it.
        assert.equal(typeof line.fields.cause, "string");
        assert(!text.includes(String(line.fields.cause)), text);
      } finally {
        await browser.close();
        if (!dexStopped) await dex.stop();
      }
    }, browserRuntimeOptions());
  },
);

// The Trellis database refuses to end logins while the Portal signs out (a
// trigger another process added aborts the write), so ending the login fails
// on the server. The sign-out page must stay on its retry screen with a
// reference that finds exactly one log line, and the login must still be
// there: once the database accepts the write again, trying again ends it.
// This catches a failed sign-out answered as "no login left", which the page
// shows as signed out while the login stays valid.
test(
  "a sign-out the server cannot finish keeps the login and shows its reference",
  { timeout: 300_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      try {
        const page = await (await browser.newContext()).newPage();
        page.setDefaultTimeout(30_000);
        await openConsoleAsRuntimeAdmin(page, runtime);

        const files = await readdir(runtime.workdir, { recursive: true });
        const path = files.find((file) => basename(file) === "platform.sqlite");
        assert(path, files.join("\n"));
        const database = new DatabaseSync(join(runtime.workdir, path), {
          timeout: 10_000,
        });
        database.exec(
          "CREATE TRIGGER refuse_sign_out BEFORE UPDATE OF revoked_at ON auth_login_sessions " +
            "BEGIN SELECT RAISE(ABORT, 'sign-out refused'); END",
        );

        const signOut = new URL("/logout", runtime.trellisUrl);
        signOut.searchParams.set("client_id", "trellis.console");
        signOut.searchParams.set(
          "post_logout_redirect_uri",
          `${runtime.consoleUrl}/console`,
        );
        await page.goto(signOut.href, { waitUntil: "domcontentloaded" });
        const alert = page.getByRole("alert").filter({ hasText: "Reference:" });
        await alert.waitFor();
        database.exec("DROP TRIGGER refuse_sign_out");
        database.close();
        const text = (await alert.textContent()) ?? "";
        assert(text.includes("still signed in"), text);
        const reference = /Reference: ([0-9A-HJKMNP-TV-Z]{26})/.exec(text)?.[1];
        assert(reference, text);

        await runtime.waitFor(() =>
          runtime.controlPlaneOutput().includes(reference),
        );
        const lines = runtime
          .controlPlaneOutput()
          .split("\n")
          .filter((line) => line.includes(reference));
        assert.equal(lines.length, 1, lines.join("\n"));
        const line = JSON.parse(lines[0]!) as {
          fields: Record<string, unknown>;
        };
        assert.equal(line.fields.route, "POST /portal/logout");
        assert.equal(line.fields.code, "server_error");
        assert.match(String(line.fields.cause), /sign-out refused/);

        // The login survived: the retry ends it, rather than being told there
        // is no login left, and the page returns to the Console.
        const retried = page.waitForResponse((response) =>
          response.url().endsWith("/portal/logout"),
        );
        await page.getByRole("button", { name: "Try again" }).click();
        assert.equal((await retried).status(), 200);
        await page.waitForURL(`${runtime.consoleUrl}/console`);
      } finally {
        await browser.close();
      }
    }, browserRuntimeOptions());
  },
);

// The browser kept its Portal login cookie but lost the CSRF cookie the
// sign-out page sends with it. Trellis refuses that sign-out, since the login
// is still there, and the page gets a new CSRF token for the login and signs
// out with it: the login ends and the browser returns to the app. Another
// origin cannot ask for the token. This catches a sign-out stuck on "Try
// again" forever, and a token reissue a foreign page could trigger.
test(
  "a browser that lost its CSRF cookie still signs out",
  { timeout: 300_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      try {
        const context = await browser.newContext();
        const page = await context.newPage();
        page.setDefaultTimeout(30_000);
        await openConsoleAsRuntimeAdmin(page, runtime);

        const files = await readdir(runtime.workdir, { recursive: true });
        const path = files.find((file) => basename(file) === "platform.sqlite");
        assert(path, files.join("\n"));
        const database = new DatabaseSync(join(runtime.workdir, path), {
          timeout: 10_000,
        });
        const liveLogins = () =>
          (
            database
              .prepare(
                "SELECT count(*) AS live FROM auth_login_sessions WHERE revoked_at IS NULL",
              )
              .get() as { live: number }
          ).live;
        try {
          const signedIn = liveLogins();
          const csrf = (await context.cookies(runtime.trellisUrl)).find(
            (cookie) => cookie.name === "__Host-trellis-portal-csrf",
          );
          assert(csrf, "the Portal login has a CSRF cookie");
          await context.clearCookies({ name: "__Host-trellis-portal-csrf" });

          // A page on another origin cannot have the login's token renewed.
          const foreign = await context.request.post(
            `${runtime.trellisUrl}/portal/csrf`,
            { headers: { Origin: "https://elsewhere.example" } },
          );
          assert.equal(foreign.status(), 400);
          assert.equal(
            (await context.cookies(runtime.trellisUrl)).some(
              (cookie) => cookie.name === "__Host-trellis-portal-csrf",
            ),
            false,
          );

          const answers: string[] = [];
          page.on("response", (response) => {
            const path = new URL(response.url()).pathname;
            if (path === "/portal/logout" || path === "/portal/csrf") {
              answers.push(`${path} ${response.status()}`);
            }
          });
          await page.getByLabel("Open user menu", { exact: true }).click();
          await page
            .getByRole("button", { name: "Sign out", exact: true })
            .click();
          await page.waitForURL((url) => url.pathname === "/logout");
          await page.waitForURL((url) =>
            url.href.startsWith(`${runtime.consoleUrl}/console`),
          );
          // Refused for its missing token, not as "no login left", then
          // signed out with the renewed one.
          assert.deepEqual(answers, [
            "/portal/logout 400",
            "/portal/csrf 200",
            "/portal/logout 200",
          ]);
          assert.equal(liveLogins(), signedIn - 1);
        } finally {
          database.close();
        }
      } finally {
        await browser.close();
      }
    }, browserRuntimeOptions());
  },
);

// Revoking an app's token also revokes its refresh family, which ends the
// app's sessions. When the database refuses that part, the revocation must
// not answer success, with a reference to its one log line, and must leave the
// token in place: once the database accepts the write again, the app's retry
// revokes the family and its sessions end. This catches a sign-out shown as
// done while the sessions stay live, and a retry answered 200 with nothing
// left to revoke.
test(
  "an app sign-out whose session revocation fails reports it and can be retried",
  { timeout: 300_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      try {
        const page = await (await browser.newContext()).newPage();
        page.setDefaultTimeout(30_000);
        await openConsoleAsRuntimeAdmin(page, runtime);

        const files = await readdir(runtime.workdir, { recursive: true });
        const path = files.find((file) => basename(file) === "platform.sqlite");
        assert(path, files.join("\n"));
        const database = new DatabaseSync(join(runtime.workdir, path), {
          timeout: 10_000,
        });
        try {
          database.exec(
            "CREATE TRIGGER refuse_family_revocation BEFORE UPDATE OF revoked_at ON auth_oauth_families " +
              "BEGIN SELECT RAISE(ABORT, 'family revocation refused'); END",
          );
          const revoked = page.waitForResponse(
            (response) =>
              new URL(response.url()).pathname === "/revoke" &&
              response.request().method() === "POST",
          );
          await page.getByLabel("Open user menu", { exact: true }).click();
          await page
            .getByRole("button", { name: "Sign out", exact: true })
            .click();
          const response = await revoked;
          database.exec("DROP TRIGGER refuse_family_revocation");
          assert.equal(response.status(), 500);
          const reference = response.headers()["trellis-reference"];
          assert(reference);
          await runtime.waitFor(() =>
            runtime.controlPlaneOutput().includes(reference),
          );
          const lines = runtime
            .controlPlaneOutput()
            .split("\n")
            .filter((line) => line.includes(reference));
          assert.equal(lines.length, 1, lines.join("\n"));
          const line = JSON.parse(lines[0]!) as {
            fields: Record<string, unknown>;
          };
          assert.equal(line.fields.route, "POST /revoke");
          assert.match(String(line.fields.cause), /family revocation refused/);

          // The refresh token the app sent is still there to revoke.
          const form = new URLSearchParams(response.request().postData() ?? "");
          const token = form.get("token");
          assert(token);
          const family = database
            .prepare(
              "SELECT family_lookup_hash AS family, oauth_grant_id AS grant FROM auth_oauth_artifacts " +
                "WHERE kind='refresh' AND lookup_hash=?",
            )
            .get(createHash("sha256").update(token).digest("base64url")) as
            { family: string; grant: string } | undefined;
          assert(family, "the failed sign-out kept the refresh token");

          const retried = await page.request.post(
            new URL("/revoke", runtime.trellisUrl).href,
            { form: Object.fromEntries(form) },
          );
          assert.equal(retried.status(), 200);
          const familyRevoked = database
            .prepare(
              "SELECT revoked_at AS revoked FROM auth_oauth_families WHERE family_lookup_hash=?",
            )
            .get(family.family) as { revoked: number | null } | undefined;
          assert(
            familyRevoked?.revoked != null,
            "the retry revoked the family",
          );
          await runtime.waitFor(
            () =>
              (
                database
                  .prepare(
                    "SELECT count(*) AS live FROM auth_authorization_sessions " +
                      "WHERE oauth_grant_id=? AND state='active'",
                  )
                  .get(family.grant) as { live: number }
              ).live === 0,
          );
        } finally {
          database.close();
        }
      } finally {
        await browser.close();
      }
    }, browserRuntimeOptions());
  },
);

// The Trellis database can't answer while the Portal looks up the sign-in
// page's login (another process renamed the table it reads). The page must
// answer the server's own failure with a reference to its one log line, not
// `access_denied`, which tells the person their sign-in was refused. This
// catches a store failure reported as a denial with nothing an operator can
// find.
test(
  "a sign-in page the server cannot read answers with a reference, not a denial",
  { timeout: 300_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      try {
        const page = await (await browser.newContext()).newPage();
        page.setDefaultTimeout(30_000);
        await page.goto(`${runtime.consoleUrl}/console`, {
          waitUntil: "domcontentloaded",
        });
        await page.waitForURL(/\/login\?transaction=/);

        const files = await readdir(runtime.workdir, { recursive: true });
        const path = files.find((file) => basename(file) === "platform.sqlite");
        assert(path, files.join("\n"));
        const database = new DatabaseSync(join(runtime.workdir, path), {
          timeout: 10_000,
        });
        database.exec(
          "ALTER TABLE auth_portal_sessions RENAME TO auth_portal_sessions_away",
        );
        let response;
        try {
          response = await page.reload({ waitUntil: "domcontentloaded" });
        } finally {
          database.exec(
            "ALTER TABLE auth_portal_sessions_away RENAME TO auth_portal_sessions",
          );
          database.close();
        }
        assert(response);
        assert.equal(response.status(), 500);
        const body = await response.text();
        assert(!body.includes("access_denied"), body);
        const reference = response.headers()["trellis-reference"];
        assert(reference, body);
        assert(body.includes(reference), body);
        await runtime.waitFor(() =>
          runtime.controlPlaneOutput().includes(reference),
        );
        const lines = runtime
          .controlPlaneOutput()
          .split("\n")
          .filter((line) => line.includes(reference));
        assert.equal(lines.length, 1, lines.join("\n"));
        const line = JSON.parse(lines[0]!) as {
          fields: Record<string, unknown>;
        };
        assert.equal(line.fields.route, "GET /login");
        assert.equal(line.fields.code, "server_error");
        assert.match(String(line.fields.cause), /auth_portal_sessions/);
      } finally {
        await browser.close();
      }
    }, browserRuntimeOptions());
  },
);
