import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { type Browser, chromium, type Page } from "playwright";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { build } from "vite";

import {
  apps,
  participants,
} from "../../integration/fixtures/consent-review/packages/consent-review/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";

type Module = {
  BrowserOAuthClient: typeof import("../packages/trellis/auth/browser/oauth.ts").BrowserOAuthClient;
};

test("the consent page offers only what the person can have, names them and each choice, and returning sign-ins skip it", async () => {
  const directory = await mkdtemp(join(tmpdir(), "trellis-consent-review-"));
  const root = fileURLToPath(new URL("../../", import.meta.url));
  try {
    const entry = join(directory, "entry.ts");
    await writeFile(
      entry,
      `export { BrowserOAuthClient } from ${JSON.stringify(`${root}/ts/packages/trellis/auth/browser/oauth.ts`)};`,
    );
    await build({
      configFile: false,
      logLevel: "error",
      build: {
        outDir: directory,
        emptyOutDir: false,
        lib: { entry, formats: ["es"], fileName: () => "app.js" },
        rollupOptions: { output: { inlineDynamicImports: true } },
      },
    });
    const source = await readFile(join(directory, "app.js"));
    const server = createServer((request, response) => {
      const script = request.url === "/app.js";
      response.writeHead(200, {
        "Content-Type": script ? "text/javascript" : "text/html",
      });
      response.end(
        script ? source : "<!doctype html><title>Consent review</title>",
      );
    });
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    assert(address && typeof address !== "string");
    const origin = `http://127.0.0.1:${address.port}`;
    let browser: Browser | undefined;
    try {
      await withTrellisRuntime(async (runtime) => {
        await runtime.registerService({
          name: "consent-review",
          contract: participants.Provider.participant,
        });
        const password = "consent-review-password";
        const { user } = await runtime.callAdminRpc("authUsersCreate", {
          username: "asha",
          password,
          displayName: "Asha Rao",
          email: "asha@example.com",
          idempotencyKey: codecs.ulid.decode(ulid()),
        });
        const { requiredCapabilities, optionalCapabilities } =
          apps.Reviewer.request;
        const plans = optionalCapabilities.find((id) =>
          id.endsWith(":plans.edit"),
        );
        assert(plans);
        // The person may have everything the app asks for except crop plans.
        const grant = async (capabilityId: string) =>
          await runtime.callAdminRpc("authCapabilityGrantsGrant", {
            principalId: user.principalId,
            capabilityId,
            expectedRevision: 0n,
            idempotencyKey: codecs.ulid.decode(ulid()),
          });
        for (const capabilityId of [
          ...requiredCapabilities,
          ...optionalCapabilities,
        ])
          if (capabilityId !== plans) await grant(capabilityId);
        const withoutPlans = optionalCapabilities.filter((id) => id !== plans);

        const register = async (title: string) => {
          const clientId = ulid();
          await runtime.callAdminRpc("authOAuthClientsPut", {
            clientId,
            kind: "browser",
            title,
            description: "Real browser consent review acceptance",
            redirectUris: [`${origin}/callback`],
            development: true,
            impliedConsent: [],
            expectedRevision: 0n,
            idempotencyKey: codecs.ulid.decode(ulid()),
          });
          return clientId;
        };
        const returning = await register("Field notes");
        const fresh = await register("Crop planner");

        browser = await chromium.launch({
          headless: true,
          env: browserTrustEnvironment(),
        });
        const page = await browser.newPage();
        page.setDefaultTimeout(15_000);
        // Pages the browser actually showed. Approving without asking
        // redirects from the consent URL before anything renders there.
        const shown: string[] = [];
        page.on("framenavigated", (frame) => {
          if (frame === page.mainFrame())
            shown.push(new URL(frame.url()).pathname);
        });
        await page.goto(origin);

        /** Starts a sign-in to `clientId` asking for `optional`. */
        const signIn = async (
          clientId: string,
          optional: readonly string[],
        ) => {
          const url = await page.evaluate(
            async ({ issuer, origin, clientId, required, optional }) => {
              const module: Module = await import(`${origin}/app.js`);
              const oauth = (
                await module.BrowserOAuthClient.open({
                  issuer,
                  clientId,
                  credentialHandle: clientId,
                })
              ).unwrapOrElse((error) => {
                throw error;
              });
              const url = (
                await oauth.authorize(`${origin}/callback`, {
                  required,
                  optional,
                })
              ).unwrapOrElse((error) => {
                throw error;
              });
              oauth.store.close();
              return url;
            },
            {
              issuer: runtime.trellisUrl,
              origin,
              clientId,
              required: requiredCapabilities,
              optional,
            },
          );
          shown.length = 0;
          await page.goto(url);
        };
        const returnedToApp = async () => {
          await page.waitForURL(
            (url) => url.origin === origin && url.pathname === "/callback",
          );
          const callback = new URL(page.url());
          assert(callback.searchParams.get("code"), callback.href);
        };
        const allow = async (current: Page) => {
          for (const choice of await current.getByRole("checkbox").all())
            if (await choice.isEnabled()) await choice.check();
          await current
            .getByRole("button", { name: "Allow", exact: true })
            .click();
          await returnedToApp();
        };
        const consent = page.getByRole("main");

        // First sign-in: the page asks, naming the person and each choice.
        await signIn(returning, withoutPlans);
        await page.getByLabel("Username", { exact: true }).fill("asha");
        await page.getByLabel("Password", { exact: true }).fill(password);
        await page
          .getByRole("button", { name: "Sign in", exact: true })
          .click();
        await page
          .getByRole("heading", {
            name: "Field notes wants to use your account",
          })
          .waitFor();
        const signedIn = await consent
          .getByText(/^Signed in as /)
          .textContent();
        assert.equal(signedIn?.trim(), "Signed in as Asha Rao (asha)");
        // Both APIs have a "records" area, so each API heads its own.
        for (const api of ["Fields", "Crops"])
          await consent
            .getByRole("heading", { name: api, exact: true })
            .waitFor();
        // What the app requires is listed as what approving grants, with no
        // checkbox; each optional choice has one.
        await consent.getByText("See field records", { exact: true }).waitFor();
        assert.equal(
          await consent
            .getByRole("checkbox", { name: /^See field records\b/ })
            .count(),
          0,
        );
        for (const title of ["Change field records", "See crop records"])
          await consent
            .getByRole("checkbox", { name: new RegExp(`^${title}\\b`) })
            .waitFor();
        await allow(page);

        // Signing in again with the same request skips the page.
        await signIn(returning, withoutPlans);
        await returnedToApp();
        assert(!shown.includes("/consent"), shown.join(" "));

        // So does a request whose only addition is optional access the
        // person can't be granted.
        await signIn(returning, optionalCapabilities);
        await returnedToApp();
        assert(!shown.includes("/consent"), shown.join(" "));

        // Another app asking for the same never offers what the person
        // can't have, not even as a disabled choice.
        await signIn(fresh, optionalCapabilities);
        await page
          .getByRole("heading", {
            name: "Crop planner wants to use your account",
          })
          .waitFor();
        await consent
          .getByRole("checkbox", { name: /^See crop records\b/ })
          .waitFor();
        assert.equal(await consent.getByText("Change crop plans").count(), 0);
        assert.equal(
          await consent.getByText("Not available to you").count(),
          0,
        );
        await allow(page);

        // Once the person may have crop plans, the returning app asks about it.
        await grant(plans);
        await signIn(returning, optionalCapabilities);
        await consent
          .getByRole("checkbox", { name: /^Change crop plans\b/ })
          .waitFor();
        assert(shown.includes("/consent"), shown.join(" "));
        await allow(page);
      }, browserRuntimeOptions());
    } finally {
      await browser?.close();
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
