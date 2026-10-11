import { deepStrictEqual as assertEquals } from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { ulid } from "ulid";
import { build } from "vite";

test("browser OAuth custody survives Chromium restart without exporting private keys or mixing users", async () => {
  const directory = await mkdtemp(join(tmpdir(), "trellis-oauth-custody-"));
  const bundle = join(directory, "custody.js");
  await build({
    configFile: false,
    logLevel: "error",
    build: {
      outDir: directory,
      emptyOutDir: false,
      lib: {
        entry: fileURLToPath(
          new URL(
            "../packages/trellis/auth/browser/storage.ts",
            import.meta.url,
          ),
        ),
        formats: ["es"],
        fileName: () => "custody.js",
      },
    },
  });
  const source = await readFile(bundle);
  const server = createServer((request, response) => {
    const script = request.url === "/custody.js";
    response.writeHead(200, {
      "Content-Type": script ? "text/javascript" : "text/html",
    });
    response.end(
      script
        ? source
        : "<!doctype html><title>OAuth credential custody</title>",
    );
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("missing HTTP address");
  const origin = `http://127.0.0.1:${address.port}`;
  const handle = ulid();
  const otherHandle = ulid();
  let browser = await chromium.launchPersistentContext(
    join(directory, "profile"),
    { headless: true },
  );
  try {
    const page = await browser.newPage();
    await page.goto(origin);
    const publicJwk = await page.evaluate(
      async ({ origin, handle }) => {
        const module: typeof import("../packages/trellis/auth/browser/storage.ts") =
          await import(`${origin}/custody.js`);
        const store = (
          await module.BrowserSessionStore.open({
            issuer: "https://oauth.example",
            instanceId: "trellis-one",
            account: "public-one",
            clientId: "browser-app",
            browserOrigin: origin,
            credentialHandle: handle,
          })
        ).unwrapOrElse((error) => {
          throw error;
        });
        const saved = await store.rememberGrant({
          principalId: "user-one",
          oauthGrantId: "grant-one",
          loginSessionId: "login-one",
          refreshToken: "confidential-refresh-one",
        });
        if (saved.isErr()) throw saved.error;
        let exported = false;
        try {
          await crypto.subtle.exportKey("pkcs8", store.dpop.privateKey);
          exported = true;
        } catch {
          /* expected non-extractability */
        }
        if (exported) throw new Error("persistent private key was exportable");
        const publicJwk = await crypto.subtle.exportKey(
          "jwk",
          store.dpop.publicKey,
        );
        store.close();
        return publicJwk;
      },
      { origin, handle },
    );
    await browser.close();
    browser = await chromium.launchPersistentContext(
      join(directory, "profile"),
      { headless: true },
    );
    const restored = await browser.newPage();
    await restored.goto(origin);
    const result = await restored.evaluate(
      async ({ origin, handle, otherHandle, publicJwk }) => {
        const module: typeof import("../packages/trellis/auth/browser/storage.ts") =
          await import(`${origin}/custody.js`);
        const scope = {
          issuer: "https://oauth.example",
          instanceId: "trellis-one",
          account: "public-one",
          clientId: "browser-app",
          browserOrigin: origin,
          credentialHandle: handle,
        };
        const store = (
          await module.BrowserSessionStore.open(scope)
        ).unwrapOrElse((error) => {
          throw error;
        });
        const token = (await store.refreshToken()).unwrapOrElse((error) => {
          throw error;
        });
        const payload = new TextEncoder().encode(
          "real persisted DPoP key possession",
        );
        const signature = await crypto.subtle.sign(
          { name: "ECDSA", hash: "SHA-256" },
          store.dpop.privateKey,
          payload,
        );
        const publicKey = await crypto.subtle.importKey(
          "jwk",
          publicJwk,
          { name: "ECDSA", namedCurve: "P-256" },
          false,
          ["verify"],
        );
        const verified = await crypto.subtle.verify(
          { name: "ECDSA", hash: "SHA-256" },
          publicKey,
          signature,
          payload,
        );
        const other = (
          await module.BrowserSessionStore.open({
            ...scope,
            credentialHandle: otherHandle,
          })
        ).unwrapOrElse((error) => {
          throw error;
        });
        const otherToken = (await other.refreshToken()).unwrapOrElse(
          (error) => {
            throw error;
          },
        );
        const replacement = await store.rememberGrant({
          principalId: "user-two",
          oauthGrantId: "grant-two",
          loginSessionId: "login-two",
          refreshToken: "confidential-refresh-two",
        });
        const preserved = (await store.refreshToken()).unwrapOrElse((error) => {
          throw error;
        });
        store.close();
        other.close();
        return {
          token,
          verified,
          otherToken,
          changedUser: replacement.isOk(),
          preserved,
        };
      },
      { origin, handle, otherHandle, publicJwk },
    );
    assertEquals(result, {
      token: "confidential-refresh-one",
      verified: true,
      otherToken: undefined,
      changedUser: false,
      preserved: "confidential-refresh-one",
    });
  } finally {
    await browser.close();
    await new Promise<void>((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
    await rm(directory, { recursive: true, force: true });
  }
});
