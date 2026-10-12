import { deepStrictEqual as assertEquals } from "node:assert/strict";
import { once } from "node:events";
import { createHash, generateKeyPairSync } from "node:crypto";
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
  const handle = "trellis.console";
  const otherHandle = ulid();
  const publicKey = generateKeyPairSync("ed25519").publicKey.export({
    format: "jwk",
  });
  if (!publicKey.x) throw new Error("missing issuer public key");
  const trust = {
    instanceId: "trellis-one",
    account: "public-one",
    issuer: {
      keyId: createHash("sha256")
        .update(Buffer.from(publicKey.x, "base64url"))
        .digest("base64url"),
      publicKey: publicKey.x,
      state: "active" as const,
    },
    rotationSequence: "7",
  };
  let browser = await chromium.launchPersistentContext(
    join(directory, "profile"),
    { headless: true },
  );
  try {
    const page = await browser.newPage();
    await page.goto(origin);
    const publicJwk = await page.evaluate(
      async ({ origin, handle, trust }) => {
        const module: typeof import("../packages/trellis/auth/browser/storage.ts") =
          await import(`${origin}/custody.js`);
        const store = (
          await module.BrowserSessionStore.open(
            {
              issuer: "https://oauth.example",
              instanceId: "trellis-one",
              account: "public-one",
              clientId: "browser-app",
              browserOrigin: origin,
              credentialHandle: handle,
            },
            trust,
          )
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
      { origin, handle, trust },
    );
    await browser.close();
    browser = await chromium.launchPersistentContext(
      join(directory, "profile"),
      { headless: true },
    );
    const restored = await browser.newPage();
    await restored.goto(origin);
    const result = await restored.evaluate(
      async ({ origin, handle, otherHandle, publicJwk, trust }) => {
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
          await module.BrowserSessionStore.open(scope, {
            ...trust,
            issuer: {
              ...trust.issuer,
              keyId: "fresh-discovery-must-not-replace-the-pin",
            },
            rotationSequence: "999",
          })
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
          await module.BrowserSessionStore.open(
            {
              ...scope,
              credentialHandle: otherHandle,
            },
            trust,
          )
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
        const retainedTrust = store.trust;
        // Simulate a pre-release unpinned record on disk. Reopening must not
        // associate its still-encrypted secret with newly discovered trust.
        const database = await new Promise<IDBDatabase>((resolve, reject) => {
          const request = indexedDB.open("trellis-oauth", 1);
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error);
        });
        await new Promise<void>((resolve, reject) => {
          const transaction = database.transaction("credentials", "readwrite");
          const records = transaction.objectStore("credentials");
          const request = records.openCursor();
          request.onsuccess = () => {
            const cursor = request.result;
            if (!cursor) return;
            const record = cursor.value;
            delete record.trust;
            cursor.update(record);
            cursor.continue();
          };
          transaction.oncomplete = () => resolve();
          transaction.onabort = () => reject(transaction.error);
        });
        database.close();
        const unpinned = await module.BrowserSessionStore.open(scope, trust);
        const recovery = unpinned.isErr() ? unpinned.error.reason : "accepted";
        store.close();
        other.close();
        return {
          token,
          verified,
          otherToken,
          changedUser: replacement.isOk(),
          preserved,
          retainedTrust,
          recovery,
        };
      },
      { origin, handle, otherHandle, publicJwk, trust },
    );
    assertEquals(result, {
      token: "confidential-refresh-one",
      verified: true,
      otherToken: undefined,
      changedUser: false,
      preserved: "confidential-refresh-one",
      retainedTrust: trust,
      recovery: "OAuthRecoveryRequired",
    });
  } finally {
    await browser.close();
    await new Promise<void>((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
    await rm(directory, { recursive: true, force: true });
  }
});
