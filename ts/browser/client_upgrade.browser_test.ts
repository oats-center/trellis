import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { type Browser, chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { ulid } from "ulid";
import { build } from "vite";

import { participants as breakingProviders } from "../../integration/fixtures/client-upgrade-breaking/packages/client-upgrade/index.js";
import { participants as revisedProviders } from "../../integration/fixtures/client-upgrade-consent/packages/client-upgrade/index.js";
import { apps as narrow } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
import { participants } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { apps } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { Result } from "../packages/result/mod.ts";
import { types } from "../packages/trellis-testkit/trellis/index.js";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";

test("browser consent renewal and incompatible API recovery retain the authenticated Portal login", async () => {
  const directory = await mkdtemp(join(tmpdir(), "trellis-browser-upgrade-"));
  const root = fileURLToPath(new URL("../../", import.meta.url));
  try {
    const entry = join(directory, "entry.ts");
    await writeFile(
      entry,
      `
      export { TrellisClient } from ${JSON.stringify(`${root}/ts/packages/trellis/client_connect.ts`)};
      export { BrowserOAuthClient } from ${JSON.stringify(`${root}/ts/packages/trellis/auth/browser/oauth.ts`)};
      export { apps as base } from ${JSON.stringify(`${root}/integration/fixtures/client-upgrade/packages/client-upgrade/index.js`)};
      export { apps as narrow } from ${JSON.stringify(`${root}/integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js`)};
      export { apps as revised } from ${JSON.stringify(`${root}/integration/fixtures/client-upgrade-consent/packages/client-upgrade/index.js`)};
      export { apps as breaking } from ${JSON.stringify(`${root}/integration/fixtures/client-upgrade-breaking/packages/client-upgrade/index.js`)};
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
    const server = createServer((request, response) => {
      const script = request.url === "/app.js";
      response.writeHead(200, {
        "Content-Type": script ? "text/javascript" : "text/html",
      });
      response.end(
        script ? source : "<!doctype html><title>Browser upgrade</title>",
      );
    });
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    assert(address && typeof address !== "string");
    const origin = `http://127.0.0.1:${address.port}`;
    // Launch inside the runtime so the browser trusts its generated CA.
    let browser: Browser | undefined;
    try {
      await withTrellisRuntime(async (runtime) => {
        const identity = await runtime.registerService({
          name: "browser-upgrade",
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
        let replacement: { stop(): Promise<void> } | undefined;
        let replacementExit: Promise<unknown> | undefined;
        browser = await chromium.launch({
          headless: true,
          env: browserTrustEnvironment(),
        });
        const page = await browser.newPage();
        const clientId = ulid();
        try {
          await runtime.callAdminRpc("authOAuthClientsPut", {
            clientId,
            kind: "browser",
            title: "Browser upgrade",
            description: "Real browser upgrade acceptance",
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
            ...apps.BrowserCaller.request.requiredCapabilities,
            ...narrow.BrowserCaller.request.requiredCapabilities,
          ])) {
            await runtime.callAdminRpc("authCapabilityGrantsGrant", {
              principalId,
              capabilityId,
              expectedRevision: 0n,
              idempotencyKey: ulid(),
            });
          }
          let loginSessionId: string | undefined;
          for (const variant of [
            "narrow",
            "base",
            "revised",
            "breaking",
          ] as const) {
            if (variant === "revised" || variant === "breaking") {
              const contract =
                variant === "revised"
                  ? revisedProviders.Provider.participant
                  : breakingProviders.Provider.participant;
              if (variant === "breaking") {
                const { review } = await runtime.callAdminRpc(
                  "authApisReview",
                  {
                    apiId:
                      breakingProviders.Provider.participant.implements[0]
                        .identity,
                    packageEvidence: types.AuthPackageEvidenceCodec.decode(
                      contract.packageEvidence,
                    ),
                  },
                );
                assert.equal(review.compatible, false);
                await runtime.callAdminRpc("authApisForceReplace", {
                  reviewId: review.reviewId,
                  canonicalDigest: review.canonicalDigest,
                  expectedGeneration: review.currentGeneration ?? 0n,
                  expectedAcceptedRevision:
                    review.currentAcceptedRevision ?? 0n,
                  acknowledgeBreakage: true,
                  acknowledgeConsentImpact: true,
                  roleEdits: [],
                  mappingEdits: [],
                  idempotencyKey: ulid(),
                });
                await runtime.contracts.apply({ contract });
              } else {
                await runtime.contracts.install({ contract });
              }
              const grants = await runtime.callAdminRpc(
                "authCapabilityGrantsList",
                {
                  principalId,
                  capabilityId:
                    apps.BrowserCaller.request.requiredCapabilities[0],
                },
              );
              assert.equal(grants.items.length, 1);
              await runtime.callAdminRpc("authCapabilityGrantsGrant", {
                principalId,
                capabilityId:
                  apps.BrowserCaller.request.requiredCapabilities[0],
                expectedRevision: grants.items[0].revision,
                idempotencyKey: ulid(),
              });
              if (variant === "breaking") {
                await service.stop();
                await serviceExit;
                const nextIdentity = await runtime.registerService({
                  name: "browser-upgrade",
                  contract: breakingProviders.Provider.participant,
                });
                const next = await TrellisService.connect({
                  trellisUrl: runtime.trellisUrl,
                  participant: breakingProviders.Provider.participant,
                  seed: nextIdentity.seed,
                }).orThrow();
                replacement = next;
                replacementExit = next.wait().catch(() => undefined);
                await next.handleEcho(({ input }) => Result.ok(input));
                await next.handleExtra(({ input }) => Result.ok(input));
              }
            }
            await page.goto(origin);
            const authorization = await page.evaluate(
              async ({ issuer, origin, clientId, variant }) => {
                const module: {
                  BrowserOAuthClient: typeof import("../packages/trellis/auth/browser/oauth.ts").BrowserOAuthClient;
                  narrow: typeof import("../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js").apps;
                  base: typeof import("../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js").apps;
                  revised: typeof import("../../integration/fixtures/client-upgrade-consent/packages/client-upgrade/index.js").apps;
                  breaking: typeof import("../../integration/fixtures/client-upgrade-breaking/packages/client-upgrade/index.js").apps;
                } = await import(`${origin}/app.js`);
                const oauth = (
                  await module.BrowserOAuthClient.open({
                    issuer,
                    clientId,
                    credentialHandle: clientId,
                  })
                ).unwrapOrElse((error) => {
                  throw error;
                });
                const request = module[variant].BrowserCaller.request;
                const url = (
                  await oauth.authorize(`${origin}/callback`, {
                    required: request.requiredCapabilities,
                    optional: request.optionalCapabilities,
                  })
                ).unwrapOrElse((error) => {
                  throw error;
                });
                oauth.store.close();
                return url;
              },
              { issuer: runtime.trellisUrl, origin, clientId, variant },
            );
            await page.goto(authorization);
            if (variant === "narrow") {
              await page
                .getByLabel("Username", { exact: true })
                .fill(runtime.adminUsername);
              await page
                .getByLabel("Password", { exact: true })
                .fill(runtime.adminPassword);
              await page
                .getByRole("button", { name: "Sign in", exact: true })
                .click();
            }
            await page
              .getByRole("heading", { name: /wants to use your account/ })
              .waitFor();
            assert.equal(
              await page.getByLabel("Password", { exact: true }).count(),
              0,
            );
            await page
              .getByRole("button", {
                name: "Allow",
                exact: true,
              })
              .click();
            await page.waitForURL(
              (url) => url.origin === origin && url.pathname === "/callback",
            );
            const result = await page.evaluate(
              async ({ issuer, origin, clientId, variant }) => {
                const module: {
                  TrellisClient: typeof import("../packages/trellis/client_connect.ts").TrellisClient;
                  narrow: typeof import("../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js").apps;
                  base: typeof import("../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js").apps;
                  revised: typeof import("../../integration/fixtures/client-upgrade-consent/packages/client-upgrade/index.js").apps;
                  breaking: typeof import("../../integration/fixtures/client-upgrade-breaking/packages/client-upgrade/index.js").apps;
                } = await import(`${origin}/app.js`);
                const auth = {
                  clientId,
                  redirectTo: `${origin}/callback`,
                };
                if (variant === "breaking") {
                  const client = (
                    await module.TrellisClient.connect({
                      trellisUrl: issuer,
                      request: module.breaking.BrowserCaller.request,
                      auth,
                    })
                  ).unwrapOrElse((error) => {
                    throw error;
                  });
                  try {
                    return {
                      value: (
                        await client.extra({ value: variant, revision: 1 })
                      ).unwrapOrElse((error) => {
                        throw error;
                      }),
                      loginSessionId:
                        client.ownAuthority.current?.loginSessionId,
                    };
                  } finally {
                    await client.connection.close();
                  }
                }
                const client = (
                  await module.TrellisClient.connect({
                    trellisUrl: issuer,
                    request: module[variant].BrowserCaller.request,
                    auth,
                  })
                ).unwrapOrElse((error) => {
                  throw error;
                });
                try {
                  return {
                    value: (await client.echo({ value: variant })).unwrapOrElse(
                      (error) => {
                        throw error;
                      },
                    ),
                    loginSessionId: client.ownAuthority.current?.loginSessionId,
                  };
                } finally {
                  await client.connection.close();
                }
              },
              { issuer: runtime.trellisUrl, origin, clientId, variant },
            );
            assert.deepEqual(
              result.value,
              variant === "breaking"
                ? { value: variant, revision: 1 }
                : { value: variant },
            );
            assert(result.loginSessionId);
            loginSessionId ??= result.loginSessionId;
            assert.equal(result.loginSessionId, loginSessionId);
          }
        } finally {
          await page.close();
          await replacement?.stop();
          await replacementExit;
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
