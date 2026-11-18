import assert from "node:assert/strict";
import { once } from "node:events";
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
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
import { browserRuntimeOptions } from "./browser_test_support.ts";

type BrowserApp = {
  TrellisClient: typeof import("../packages/trellis/client_connect.ts").TrellisClient;
  BrowserOAuthClient: typeof import("../packages/trellis/auth/browser/oauth.ts").BrowserOAuthClient;
  request: typeof apps.BrowserCaller.request;
};

test("a browser app bundled from one generated app subpath carries no IDL source and calls its service", async () => {
  const directory = await mkdtemp(join(tmpdir(), "trellis-browser-subpath-"));
  const root = fileURLToPath(new URL("../../", import.meta.url));
  try {
    // An ordinary npm browser app: it names the generated package's app
    // subpath, not its files or root barrel.
    await mkdir(join(directory, "node_modules/@oatscenter"), {
      recursive: true,
    });
    await symlink(
      join(root, "integration/fixtures/client-upgrade/packages/client-upgrade"),
      join(directory, "node_modules/client-upgrade-fixture"),
      "dir",
    );
    for (const name of ["trellis", "result"])
      await symlink(
        join(root, "ts/packages", name),
        join(directory, "node_modules/@oatscenter", name),
        "dir",
      );
    const entry = join(directory, "entry.js");
    await writeFile(
      entry,
      `export { TrellisClient } from "@oatscenter/trellis";
export { BrowserOAuthClient } from "@oatscenter/trellis/auth/browser";
export { request } from "client-upgrade-fixture/apps/BrowserCaller";
`,
    );
    await build({
      configFile: false,
      root: directory,
      logLevel: "error",
      build: {
        outDir: join(directory, "dist"),
        lib: { entry, formats: ["es"], fileName: () => "app.js" },
        rollupOptions: { output: { inlineDynamicImports: true } },
      },
    });
    const source = await readFile(join(directory, "dist/app.js"), "utf8");
    // Package evidence holds each package's canonical source, which declares
    // its models; an app request without settings never needs it.
    assert.doesNotMatch(source, /model [A-Z]\w* \{/);

    const server = createServer((request, response) => {
      const script = request.url === "/app.js";
      response.writeHead(200, {
        "Content-Type": script ? "text/javascript" : "text/html",
      });
      response.end(
        script ? source : "<!doctype html><title>Generated subpath</title>",
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
          name: "browser-subpath",
          contract: participants.Provider.participant,
        });
        const service = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          seed: identity.seed,
        }).orThrow();
        try {
          await service.handleEcho(({ input }) => Result.ok(input));
          const clientId = ulid();
          await runtime.callAdminRpc("authOAuthClientsPut", {
            clientId,
            kind: "browser",
            title: "Generated subpath",
            description: "Browser app bundled from a generated app subpath",
            redirectUris: [`${origin}/callback`],
            development: true,
            impliedConsent: [],
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          });
          const {
            user: { principalId },
          } = await runtime.callAdminRpc("authUsersResolve", {});
          for (const capabilityId of apps.BrowserCaller.request
            .requiredCapabilities) {
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
          const page = await browser.newPage();
          await page.goto(origin);
          const authorization = await page.evaluate(
            async ({ issuer, origin, clientId }) => {
              const app: BrowserApp = await import(`${origin}/app.js`);
              const oauth = (
                await app.BrowserOAuthClient.open({
                  issuer,
                  clientId,
                  credentialHandle: clientId,
                })
              ).unwrapOrElse((error) => {
                throw error;
              });
              const url = (
                await oauth.authorize(`${origin}/callback`, {
                  required: app.request.requiredCapabilities,
                  optional: app.request.optionalCapabilities,
                })
              ).unwrapOrElse((error) => {
                throw error;
              });
              oauth.store.close();
              return url;
            },
            { issuer: runtime.trellisUrl, origin, clientId },
          );
          await page.goto(authorization);
          await page
            .getByLabel("Username", { exact: true })
            .fill(runtime.adminUsername);
          await page
            .getByLabel("Password", { exact: true })
            .fill(runtime.adminPassword);
          await page
            .getByRole("button", { name: "Sign in", exact: true })
            .click();
          await page
            .getByRole("button", { name: "Allow", exact: true })
            .click();
          await page.waitForURL(
            (url) => url.origin === origin && url.pathname === "/callback",
          );
          const echoed = await page.evaluate(
            async ({ issuer, origin, clientId }) => {
              const app: BrowserApp = await import(`${origin}/app.js`);
              const client = (
                await app.TrellisClient.connect({
                  trellisUrl: issuer,
                  request: app.request,
                  auth: { clientId, redirectTo: `${origin}/callback` },
                })
              ).unwrapOrElse((error) => {
                throw error;
              });
              try {
                return (await client.echo({ value: "subpath" })).unwrapOrElse(
                  (error) => {
                    throw error;
                  },
                );
              } finally {
                await client.connection.close();
              }
            },
            { issuer: runtime.trellisUrl, origin, clientId },
          );
          assert.deepEqual(echoed, { value: "subpath" });
        } finally {
          await service.stop();
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
