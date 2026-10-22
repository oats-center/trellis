import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { type Browser, chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { build } from "vite";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";

test("first-contact browser discovery does not require a registered application origin", async () => {
  const directory = await mkdtemp(join(tmpdir(), "trellis-public-discovery-"));
  try {
    await build({
      configFile: false,
      logLevel: "error",
      build: {
        outDir: directory,
        lib: {
          entry: fileURLToPath(
            new URL(
              "../packages/trellis/auth/browser/oauth.ts",
              import.meta.url,
            ),
          ),
          formats: ["es"],
          fileName: () => "client.js",
        },
      },
    });
    const source = await readFile(join(directory, "client.js"));
    const server = createServer((request, response) => {
      const script = request.url === "/client.js";
      response.writeHead(200, {
        "Content-Type": script ? "text/javascript" : "text/html",
      });
      response.end(
        script ? source : "<!doctype html><title>New application</title>",
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
        browser = await chromium.launch({
          headless: true,
          env: browserTrustEnvironment(),
        });
        const page = await browser.newPage();
        await page.goto(origin);
        const result = await page.evaluate(
          async ({ issuer, origin }) => {
            const {
              BrowserOAuthClient,
            }: typeof import("../packages/trellis/auth/browser/oauth.ts") =
              await import(`${origin}/client.js`);
            const client = (
              await BrowserOAuthClient.open({
                issuer,
                clientId: "https://new-app.example/oauth-client.json",
                credentialHandle: "first-contact",
                loopbackDevelopment: true,
              })
            ).unwrapOrElse((error) => {
              throw error;
            });
            const active = client.metadata.trellis.issuers.find(
              (key) => key.state === "active",
            );
            if (!active) throw new Error("missing active issuer");
            const chainUrl = new URL(
              "/.well-known/trellis/issuer-chain",
              issuer,
            );
            chainUrl.search = new URLSearchParams({
              pinnedKeyId: active.keyId,
              targetKeyId: active.keyId,
            }).toString();
            const chain = await fetch(chainUrl);
            const chainBody: unknown = await chain.json();
            const forbidden: boolean[] = [];
            for (const [path, init] of [
              ["/portal/authorization/unknown", { credentials: "include" }],
              ["/portal/approve", { method: "POST", credentials: "include" }],
              [
                "/token",
                {
                  method: "POST",
                  body: new URLSearchParams({
                    grant_type: "authorization_code",
                    client_id: "unknown",
                    code: "invalid",
                  }),
                },
              ],
            ] as const) {
              forbidden.push(
                await fetch(new URL(path, issuer), init).then(
                  () => false,
                  () => true,
                ),
              );
            }
            const selectedIssuer = client.metadata.issuer;
            client.store.close();
            return {
              selectedIssuer,
              chainStatus: chain.status,
              chainBody,
              forbidden,
            };
          },
          { issuer: runtime.oauthIssuer, origin },
        );
        assert.equal(
          new URL(result.selectedIssuer).origin,
          new URL(runtime.oauthIssuer).origin,
        );
        assert.equal(result.chainStatus, 200);
        assert(result.chainBody);
        assert.deepEqual(result.forbidden, [true, true, true]);
        await page.close();
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
