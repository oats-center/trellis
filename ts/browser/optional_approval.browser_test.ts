import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { type Browser, chromium } from "playwright";
import { ulid } from "ulid";
import { build } from "vite";

import {
  apps,
  participants,
} from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { Result } from "../packages/result/mod.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  approveConsentIfRequired,
  browserRuntimeOptions,
} from "./browser_test_support.ts";

type Module = {
  TrellisClient: typeof import("../packages/trellis/client_connect.ts").TrellisClient;
  narrow: typeof import("../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js").apps;
  base: typeof import("../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js").apps;
};

test("an app asks for an optional capability once on load, and when used, in place through a popup", async () => {
  const directory = await mkdtemp(join(tmpdir(), "trellis-optional-approval-"));
  const root = fileURLToPath(new URL("../../", import.meta.url));
  try {
    const entry = join(directory, "entry.ts");
    await writeFile(
      entry,
      `
      export { TrellisClient } from ${JSON.stringify(`${root}/ts/packages/trellis/client_connect.ts`)};
      export { apps as base } from ${JSON.stringify(`${root}/integration/fixtures/client-upgrade/packages/client-upgrade/index.js`)};
      export { apps as narrow } from ${JSON.stringify(`${root}/integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js`)};
    `,
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
    // `/use` connects and calls Extra from its own script as it loads, with
    // no user gesture, then offers a button whose click calls Extra again.
    const use = `
      const params = new URLSearchParams(location.search);
      const module = await import("/app.js");
      const asked = [];
      const client = (
        await module.TrellisClient.connect({
          trellisUrl: params.get("issuer"),
          request: module.base.OptionalCaller.request,
          auth: {
            clientId: params.get("clientId"),
            redirectTo: location.origin + "/callback",
            approval: params.get("approval"),
          },
          onAuthRequired: ({ loginUrl }) => { asked.push(loginUrl); },
        })
      ).unwrapOrElse((error) => { throw error; });
      const run = async () => {
        const result = await client.extra({ value: params.get("approval") });
        return result.isOk()
          ? { value: result.unwrapOrElse((error) => { throw error; }),
              grant: client.ownAuthority.current?.oauthGrantId }
          : { failed: true };
      };
      const automatic = await run();
      const button = document.createElement("button");
      button.id = "extra";
      button.textContent = "Extra";
      button.onclick = () => { window.clicked = run(); };
      document.body.replaceChildren(button);
      Object.assign(window, {
        asked,
        automatic,
        grant: client.ownAuthority.current?.oauthGrantId,
        closeClient: () => client.connection.close(),
      });
      console.log("use ready");
    `;
    const server = createServer((request, response) => {
      const path = new URL(request.url ?? "/", "http://localhost").pathname;
      const script = path === "/app.js";
      response.writeHead(200, {
        "Content-Type": script ? "text/javascript" : "text/html",
      });
      response.end(
        script
          ? source
          : path === "/use"
            ? `<!doctype html><title>Optional approval</title><script type="module">${use}</script>`
            : "<!doctype html><title>Optional approval</title>",
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
        const identity = await runtime.registerService({
          name: "optional-approval",
          contract: participants.Provider.participant,
        });
        const service = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          seed: identity.seed,
        }).orThrow();
        const serviceExit = service.wait().catch(() => undefined);
        await service.handleEcho(({ input }) => Result.ok(input));
        await service.handleExtra(({ input }) => Result.ok(input));
        browser = await chromium.launch({
          headless: true,
          env: browserTrustEnvironment(),
        });
        const profile = await browser.newContext();
        const page = await profile.newPage();
        const clientId = ulid();
        const context = { issuer: runtime.trellisUrl, origin, clientId };
        try {
          await runtime.callAdminRpc("authOAuthClientsPut", {
            clientId,
            kind: "browser",
            title: "Optional approval",
            description: "Real browser optional approval acceptance",
            redirectUris: [`${origin}/callback`],
            development: true,
            impliedConsent: [],
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          });
          const {
            user: { principalId },
          } = await runtime.callAdminRpc("authUsersResolve", {});
          for (const capabilityId of new Set([
            ...apps.OptionalCaller.request.requiredCapabilities,
            ...apps.OptionalCaller.request.optionalCapabilities,
          ])) {
            await runtime.callAdminRpc("authCapabilityGrantsGrant", {
              principalId,
              capabilityId,
              expectedRevision: 0n,
              idempotencyKey: ulid(),
            });
          }

          // The user first approves the app's earlier version, which needs
          // only Echo.
          await page.goto(origin);
          const signIn = await page.evaluate(
            async ({ issuer, origin, clientId }) => {
              const module: Module = await import(`${origin}/app.js`);
              let loginUrl = "";
              await module.TrellisClient.connect({
                trellisUrl: issuer,
                request: module.narrow.BrowserCaller.request,
                auth: { clientId, redirectTo: `${origin}/callback` },
                onAuthRequired: (required) => {
                  loginUrl = required.loginUrl;
                },
              });
              return loginUrl;
            },
            context,
          );
          await page.goto(signIn);
          await page
            .getByLabel("Username", { exact: true })
            .fill(runtime.adminUsername);
          await page
            .getByLabel("Password", { exact: true })
            .fill(runtime.adminPassword);
          await page
            .getByRole("button", { name: "Sign in", exact: true })
            .click();
          await approveConsentIfRequired(page, 15_000);
          await page.waitForURL(
            (url) => url.origin === origin && url.pathname === "/callback",
          );
          await page.evaluate(async ({ issuer, origin, clientId }) => {
            const module: Module = await import(`${origin}/app.js`);
            const client = (
              await module.TrellisClient.connect({
                trellisUrl: issuer,
                request: module.narrow.BrowserCaller.request,
                auth: { clientId, redirectTo: `${origin}/callback` },
              })
            ).unwrapOrElse((error) => {
              throw error;
            });
            await client.connection.close();
          }, context);

          // The new version adds an optional capability. Opted in, the app
          // asks about it on its first load, and only that once.
          await page.goto(origin);
          const loads = await page.evaluate(
            async ({ issuer, origin, clientId }) => {
              const module: Module = await import(`${origin}/app.js`);
              const asked: string[] = [];
              const outcomes: string[] = [];
              for (let load = 0; load < 2; load += 1) {
                const connected = await module.TrellisClient.connect({
                  trellisUrl: issuer,
                  request: module.base.OptionalCaller.request,
                  auth: {
                    clientId,
                    redirectTo: `${origin}/callback`,
                    askForNewOptional: true,
                  },
                  onAuthRequired: ({ loginUrl }) => {
                    asked.push(loginUrl);
                  },
                });
                if (connected.isErr())
                  outcomes.push(
                    "reason" in connected.error
                      ? connected.error.reason
                      : connected.error.name,
                  );
                else {
                  outcomes.push("connected");
                  await connected
                    .unwrapOrElse((error) => {
                      throw error;
                    })
                    .connection.close();
                }
              }
              return { asked, outcomes };
            },
            context,
          );
          assert.deepEqual(loads.outcomes, [
            "OAuthAuthorizationRequired",
            "connected",
          ]);
          const [details] = JSON.parse(
            new URL(loads.asked[0] ?? "").searchParams.get(
              "authorization_details",
            ) ?? "[]",
          );
          assert.deepEqual(
            details.optional,
            apps.OptionalCaller.request.optionalCapabilities,
          );

          // Asking at use is opt-in, and only a person's action asks. Each
          // case opens a fresh page in the same browser profile, so no earlier
          // click counts as the person acting there.
          const openUse = async (approval: "redirect" | "popup") => {
            const url = new URL("/use", origin);
            url.search = new URLSearchParams({
              issuer: runtime.trellisUrl,
              clientId,
              approval,
            }).toString();
            const use = await profile.newPage();
            // Waiting on a console message never runs script in the page,
            // which Playwright would count as the person acting there.
            const ready = use.waitForEvent("console", {
              predicate: (message) => message.text() === "use ready",
            });
            await use.goto(url.href);
            await ready;
            const loaded = await use.evaluate(() => ({
              automatic: Reflect.get(window, "automatic"),
              grant: Reflect.get(window, "grant"),
            }));
            const clicked = async () => {
              const settled = await use.evaluate(async () => {
                const result: unknown = await Reflect.get(window, "clicked");
                const asked: unknown = Reflect.get(window, "asked");
                await Reflect.get(window, "closeClient")();
                return { result, asked, location: location.href };
              });
              await use.close();
              return settled;
            };
            return { ...loaded, page: use, clicked };
          };

          // By redirect: the click's call fails and asks once, the automatic
          // call before it not at all.
          const redirecting = await openUse("redirect");
          assert.deepEqual(redirecting.automatic, { failed: true });
          await redirecting.page.click("#extra");
          await redirecting.page.waitForFunction(() => {
            const asked: unknown = Reflect.get(window, "asked");
            return Array.isArray(asked) && asked.length > 0;
          });
          const redirected = await redirecting.clicked();
          assert.deepEqual(redirected.result, { failed: true });
          assert(Array.isArray(redirected.asked));
          assert.equal(redirected.asked.length, 1);
          assert.match(String(redirected.asked[0]), /authorization_details/);

          // By popup: the click's call asks in a popup and goes through once
          // approved, on the same grant and without leaving the page.
          const popping = await openUse("popup");
          assert.deepEqual(popping.automatic, { failed: true });
          assert(popping.grant);
          const opened = popping.page.waitForEvent("popup");
          await popping.page.click("#extra");
          const popup = await opened;
          await approveConsentIfRequired(popup, 15_000);
          const approved = await popping.clicked();
          assert.deepEqual(approved.result, {
            value: { value: "popup" },
            grant: popping.grant,
          });
          assert.equal(new URL(approved.location).pathname, "/use");
          await popup
            .waitForEvent("close", { timeout: 10_000 })
            .catch(() => {});
          assert(popup.isClosed());
        } finally {
          await profile.close();
          await service.stop();
          await serviceExit;
        }
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
