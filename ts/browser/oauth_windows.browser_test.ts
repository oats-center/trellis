import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { type Browser, type Page, chromium } from "playwright";
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

type Outcome = {
  connected?: boolean;
  reason?: string;
  grant?: string;
  declined?: boolean[];
};

test("windows the app opens sign in on their own, consent's choices hold from the callback, and an approved optional capability carries into a version that requires it", async () => {
  const directory = await mkdtemp(join(tmpdir(), "trellis-oauth-windows-"));
  const root = fileURLToPath(new URL("../../", import.meta.url));
  try {
    const entry = join(directory, "entry.ts");
    await writeFile(
      entry,
      `
      export { TrellisClient } from ${JSON.stringify(`${root}/ts/packages/trellis/client_connect.ts`)};
      export { apps } from ${JSON.stringify(`${root}/integration/fixtures/client-upgrade/packages/client-upgrade/index.js`)};
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
    // `/connect` and the `/callback` it returns to connect as the app named in
    // `app`, kept for the tab across the sign-in round trip, and report how
    // that went in the console.
    const connect = `
      const params = new URLSearchParams(location.search);
      for (const name of ["app", "issuer", "clientId"])
        if (params.has(name)) sessionStorage.setItem(name, params.get(name));
      const app = sessionStorage.getItem("app");
      const module = await import("/app.js");
      const request = module.apps[app].request;
      const connected = await module.TrellisClient.connect({
        trellisUrl: sessionStorage.getItem("issuer"),
        request,
        auth: {
          clientId: sessionStorage.getItem("clientId"),
          redirectTo: location.origin + "/callback",
        },
      });
      let outcome;
      if (connected.isErr()) {
        outcome = { reason: connected.error.reason ?? connected.error.name };
      } else {
        const client = connected.unwrapOrElse((error) => { throw error; });
        outcome = {
          connected: true,
          grant: client.ownAuthority.current?.oauthGrantId,
          declined: request.optionalCapabilities.map(
            (capability) => client.availability().declined[capability],
          ),
        };
        await client.connection.close();
      }
      console.log("outcome " + JSON.stringify(outcome));
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
          : path === "/connect" || path === "/callback"
            ? `<!doctype html><title>OAuth windows</title><script type="module">${connect}</script>`
            : "<!doctype html><title>OAuth windows</title>",
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
          name: "oauth-windows",
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
        try {
          const clientId = ulid();
          await runtime.callAdminRpc("authOAuthClientsPut", {
            clientId,
            kind: "browser",
            title: "OAuth windows",
            description: "Real browser sign-in in opened windows",
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
          browser = await chromium.launch({
            headless: true,
            env: browserTrustEnvironment(),
          });
          const connectUrl = (
            app: string,
            extra: Record<string, string> = {},
          ) =>
            `${origin}/connect?${new URLSearchParams({
              app,
              issuer: runtime.trellisUrl,
              clientId,
              ...extra,
            })}`;
          const outcome = (page: Page) =>
            page
              .waitForEvent("console", {
                predicate: (message) => message.text().startsWith("outcome "),
                timeout: 30_000,
              })
              .then((message): Outcome =>
                JSON.parse(message.text().slice("outcome ".length)),
              );
          const signIn = async (page: Page) => {
            await page
              .getByLabel("Username", { exact: true })
              .fill(runtime.adminUsername);
            await page
              .getByLabel("Password", { exact: true })
              .fill(runtime.adminPassword);
            await page
              .getByRole("button", { name: "Sign in", exact: true })
              .click();
          };

          // A window the app opens itself signs in on its own: its callback is
          // no approval popup, though a same-origin window opened it. The
          // optional capability left unchecked is declined from the callback
          // on, not only after a reload.
          const first = await browser.newContext();
          try {
            const opener = await first.newPage();
            await opener.goto(origin);
            const opened = opener.waitForEvent("popup");
            await opener.evaluate(
              (url) => void window.open(url, "_blank"),
              connectUrl("OptionalCaller"),
            );
            const window1 = await opened;
            await signIn(window1);
            await window1
              .getByRole("button", { name: "Allow", exact: true })
              .click();
            const signedIn = await outcome(window1);
            assert.equal(signedIn.connected, true, JSON.stringify(signedIn));
            assert.deepEqual(signedIn.declined, [true]);

            // A `state` that isn't an OAuth callback is just part of the URL.
            const reopened = opener.waitForEvent("popup");
            await opener.evaluate(
              (url) => void window.open(url, "_blank"),
              connectUrl("OptionalCaller", { state: "unrelated" }),
            );
            const window2 = await reopened;
            const resumed = await outcome(window2);
            assert.equal(resumed.connected, true, JSON.stringify(resumed));
            assert.equal(resumed.grant, signedIn.grant);
            assert.deepEqual(resumed.declined, [true]);
          } finally {
            await first.close();
          }

          // Approved while optional, documents.read is already in the grant
          // when a later version of the app requires it: the app keeps its
          // grant instead of being revoked and sent back through consent.
          const second = await browser.newContext();
          try {
            const page = await second.newPage();
            await page.goto(connectUrl("OptionalCaller"));
            await signIn(page);
            const approved = outcome(page);
            await approveConsentIfRequired(page, 15_000);
            const optional = await approved;
            assert.equal(optional.connected, true, JSON.stringify(optional));
            assert.deepEqual(optional.declined, [false]);
            const upgraded = outcome(page);
            await page.goto(connectUrl("BrowserCaller"));
            const required = await upgraded;
            assert.equal(required.connected, true, JSON.stringify(required));
            assert.equal(required.grant, optional.grant);
            assert.equal(new URL(page.url()).origin, origin);
          } finally {
            await second.close();
          }
        } finally {
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
