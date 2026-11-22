import assert from "node:assert/strict";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { chromium } from "playwright";
import { ulid } from "ulid";

import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";
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
