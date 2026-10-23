import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { mkdir, readFile } from "node:fs/promises";
import { request } from "node:http";
import { createServer } from "node:https";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { chromium, type Page } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { build } from "vite";
import { z } from "zod";
import { issuerChainSchema } from "../packages/trellis/auth/protocol_wasm.ts";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import {
  MemoryNativeOAuthCustody,
  NativeOAuthClient,
  type NativeOAuthSession,
  type NativeCredentialScope,
  type RuntimeAuthorizationTrust,
} from "../packages/trellis/native.ts";

const mode = process.argv[2];
assert.ok(mode === "native" || mode === "browser");
const {
  TRELLIS_ROTATION_ISSUER: issuer,
  TRELLIS_ROTATION_BACKEND: backend,
  TRELLIS_ROTATION_APP: appOrigin,
  TRELLIS_ROTATION_DIRECTORY: directory,
  TRELLIS_ROTATION_CERT: certificate,
  TRELLIS_ROTATION_KEY: privateKey,
} = z
  .object({
    TRELLIS_ROTATION_ISSUER: z.string().url(),
    TRELLIS_ROTATION_BACKEND: z.string().url(),
    TRELLIS_ROTATION_APP: z.string().url(),
    TRELLIS_ROTATION_DIRECTORY: z.string().min(1),
    TRELLIS_ROTATION_CERT: z.string().min(1),
    TRELLIS_ROTATION_KEY: z.string().min(1),
  })
  .parse(process.env);
await mkdir(directory, { recursive: true });
const tls = {
  cert: await readFile(certificate),
  key: await readFile(privateKey),
};
const lines = createInterface({ input: process.stdin });
const replies = lines[Symbol.asyncIterator]();
async function rotate(steps = 1) {
  console.log(JSON.stringify({ stage: "rotate", steps }));
  const reply = await replies.next();
  assert.equal(reply.done, false);
  const value = JSON.parse(reply.value);
  assert.equal(typeof value.keyId, "string");
  return value.keyId as string;
}
const digest = (value: string) =>
  createHash("sha256").update(value).digest("hex");
let chain: "valid" | "missing" | "forged" = "valid";
let pauseRefresh = false;
let failChainAfterToken = false;
let rotatedInFlight: string | undefined;
let refreshRequests = 0;
let proxyError: unknown;
const tampered = { missing: 0, forged: 0 };
const successes: { spent: string; successor: string }[] = [];

// Every response originates in the real engine. This proxy only removes or
// corrupts signed chain bytes, or pauses a real request while its signer rotates.
const proxy = createServer(tls, async (incoming, outgoing) => {
  try {
    const chunks: Buffer[] = [];
    for await (const chunk of incoming) chunks.push(Buffer.from(chunk));
    const body = Buffer.concat(chunks);
    const path = new URL(incoming.url ?? "/", issuer);
    const form = new URLSearchParams(body.toString());
    const refreshing =
      path.pathname === "/token" && form.get("grant_type") === "refresh_token";
    if (refreshing) {
      refreshRequests++;
      if (pauseRefresh) {
        pauseRefresh = false;
        rotatedInFlight = await rotate();
        failChainAfterToken = true;
      }
    }
    const upstream = request(
      new URL(incoming.url ?? "/", backend),
      {
        method: incoming.method,
        headers: incoming.headers,
      },
      async (response) => {
        try {
          const chunks: Buffer[] = [];
          for await (const chunk of response) chunks.push(Buffer.from(chunk));
          let bytes = Buffer.concat(chunks);
          if (refreshing && response.statusCode === 200) {
            const result = JSON.parse(bytes.toString());
            const spent = form.get("refresh_token");
            assert.ok(spent && typeof result.refresh_token === "string");
            successes.push({
              spent: digest(spent),
              successor: digest(result.refresh_token),
            });
            if (failChainAfterToken) {
              chain = "missing";
              failChainAfterToken = false;
            }
          }
          if (
            path.pathname === "/.well-known/trellis/issuer-chain" &&
            response.statusCode === 200 &&
            chain !== "valid"
          ) {
            const real = issuerChainSchema.parse(JSON.parse(bytes.toString()));
            assert.ok(
              real.rotations.length > 0,
              "tampering requires a real signed successor",
            );
            if (chain === "missing") real.rotations.splice(0);
            else {
              const rotation = JSON.parse(real.rotations[0]);
              assert.equal(typeof rotation.signature, "string");
              rotation.signature =
                (rotation.signature[0] === "A" ? "B" : "A") +
                rotation.signature.slice(1);
              real.rotations[0] = JSON.stringify(rotation);
            }
            tampered[chain]++;
            bytes = Buffer.from(JSON.stringify(real));
          }
          const headers = { ...response.headers };
          delete headers["transfer-encoding"];
          headers["content-length"] = String(bytes.length);
          outgoing.writeHead(response.statusCode ?? 502, headers);
          outgoing.end(bytes);
        } catch (error) {
          proxyError = error;
          console.error(error);
          outgoing.destroy();
        }
      },
    );
    upstream.on("error", (error) => {
      proxyError = error;
      console.error(error);
      outgoing.destroy();
    });
    upstream.end(body);
  } catch (error) {
    proxyError = error;
    console.error(error);
    outgoing.destroy();
  }
});
proxy.listen(Number(new URL(issuer).port), new URL(issuer).hostname);
await once(proxy, "listening");

function nativeSummary(session: NativeOAuthSession) {
  return {
    scope: session.scope,
    trust: session.trust,
    principalId: session.principalId,
    grantId: session.oauthGrantId,
    loginId: session.loginSessionId,
    dpop: session.durableDpopJkt,
    runtime: session.runtimeId,
    nkey: session.auth.sessionNkey,
  };
}
const nativeCredentials = new Map<
  string,
  {
    custody: MemoryNativeOAuthCustody;
    scope: NativeCredentialScope;
    trust: RuntimeAuthorizationTrust;
    initial: ReturnType<typeof nativeSummary>;
  }
>();

async function nativeAction(operation: string, slot: string, inflight = false) {
  if (operation === "login") {
    const custody = new MemoryNativeOAuthCustody();
    const { session } = await loginNativeTestClient({
      issuer,
      custody,
      clientId: "trellis.cli",
      request: apps.Denied.request,
      approvedCapabilities: apps.Denied.request.requiredCapabilities,
      username: "admin",
      password: "a strong bootstrap password",
    });
    try {
      const initial = nativeSummary(session);
      nativeCredentials.set(slot, {
        custody,
        scope: session.scope,
        trust: session.trust,
        initial,
      });
      return initial;
    } finally {
      await session.close();
    }
  }
  const previous = nativeCredentials.get(slot);
  assert(previous);
  const client = (await NativeOAuthClient.open(previous)).orThrow();
  try {
    if (operation === "reauthorize") {
      await assert.rejects(
        loginNativeTestClient({
          issuer,
          client,
          clientId: previous.scope.clientId,
          request: apps.Denied.request,
          approvedCapabilities: apps.Denied.request.requiredCapabilities,
          username: "other-user",
          password: "another strong local password",
        }),
        { reason: "InvalidCredential" },
      );
      const reopened = (await NativeOAuthClient.open(previous)).orThrow();
      try {
        const retained = nativeSummary((await reopened.resume()).orThrow());
        for (const key of [
          "principalId",
          "grantId",
          "loginId",
          "dpop",
        ] as const)
          assert.equal(retained[key], previous.initial[key]);
        return retained;
      } finally {
        await reopened.close();
      }
    }
    const resumed = await client.resume();
    if (operation === "denied") {
      assert.equal(resumed.error?.reason, "OAuthTrustUnavailable");
      return previous.initial;
    }
    const session = resumed.orThrow();
    const current = nativeSummary(session);
    for (const key of ["principalId", "grantId", "loginId", "dpop"] as const)
      assert.equal(current[key], previous.initial[key]);
    assert.notEqual(current.runtime, previous.initial.runtime);
    assert.notEqual(current.nkey, previous.initial.nkey);
    if (inflight) {
      pauseRefresh = true;
      const refreshed = await session.refresh();
      assert.equal(refreshed.error?.reason, "OAuthTrustUnavailable");
    }
    return current;
  } finally {
    await client.close();
  }
}

async function native() {
  const first = await nativeAction("login", "single");
  const second = await nativeAction("login", "double");
  await nativeAction("reauthorize", "single");
  assert.equal(first.trust.issuer.keyId, second.trust.issuer.keyId);
  const k2 = await rotate();
  for (const malformed of ["missing", "forged"] as const) {
    chain = malformed;
    const requests = refreshRequests;
    const priorTampering = tampered[malformed];
    await nativeAction("denied", "double");
    assert.equal(tampered[malformed], priorTampering + 1);
    assert.equal(
      refreshRequests,
      requests,
      "invalid trust must not submit a refresh credential",
    );
  }
  chain = "valid";
  const resumed = await nativeAction("resume", "single");
  assert.equal(resumed.trust.issuer.keyId, k2);
  const k3 = await rotate();
  const twoHop = await nativeAction("resume", "double");
  assert.equal(twoHop.trust.issuer.keyId, k3);
  assert.equal(
    BigInt(twoHop.trust.rotationSequence) -
      BigInt(second.trust.rotationSequence),
    2n,
  );
  await nativeAction("inflight", "single", true);
  const checkpoint = successes.at(-1);
  assert.ok(checkpoint);
  chain = "valid";
  const recovered = await nativeAction("resume", "single");
  assert.equal(recovered.trust.issuer.keyId, rotatedInFlight);
  assert.equal(
    successes.at(-1)?.spent,
    checkpoint.successor,
    "reopening must use the fresh unspent successor, never the old spent credential",
  );
  assert.notEqual(checkpoint.spent, checkpoint.successor);
}

async function browser() {
  const bundleDirectory = join(directory, "bundle");
  await build({
    configFile: false,
    logLevel: "error",
    build: {
      outDir: bundleDirectory,
      lib: {
        entry: fileURLToPath(
          new URL("./oauth_issuer_rotation_client.ts", import.meta.url),
        ),
        formats: ["es"],
        fileName: () => "sdk.js",
      },
    },
  });
  const script = await readFile(join(bundleDirectory, "sdk.js"));
  const app = createServer(tls, (incoming, outgoing) => {
    outgoing.writeHead(200, {
      "Content-Type":
        incoming.url === "/sdk.js" ? "text/javascript" : "text/html",
      "Cache-Control": "no-store",
    });
    outgoing.end(
      incoming.url === "/sdk.js"
        ? script
        : "<!doctype html><title>Real issuer rotation browser consumer</title>",
    );
  });
  app.listen(Number(new URL(appOrigin).port), new URL(appOrigin).hostname);
  await once(app, "listening");
  const browser = await chromium.launch({
    headless: true,
    env: browserTrustEnvironment(),
  });
  const context = await browser.newContext({ ignoreHTTPSErrors: true });
  const page = await context.newPage();
  page.setDefaultTimeout(10000);
  page.setDefaultNavigationTimeout(10000);
  const failures: string[] = [];
  page.on("pageerror", (error) => failures.push(error.message));
  type Summary = {
    principalId: string;
    grantId: string;
    loginId: string;
    dpop: string;
    runtime: string;
    nkey: string;
    keyId: string;
    rotationSequence: string;
  };
  async function call(
    target: Page,
    handle: string,
    action: "authorize" | "complete" | "resume" | "tokenDigest",
    reopen = true,
    rejectInstall = false,
  ) {
    return await target.evaluate(
      async ({ issuer, handle, appOrigin, action, reopen, rejectInstall }) => {
        const sdk: typeof import("./oauth_issuer_rotation_client.ts") =
          await import(`${appOrigin}/sdk.js`);
        if (reopen) await sdk.open(issuer, handle);
        return action === "resume"
          ? await sdk.resume(rejectInstall)
          : await sdk[action]();
      },
      { issuer, handle, appOrigin, action, reopen, rejectInstall },
    );
  }
  async function login(handle: string) {
    await page.goto(appOrigin);
    const authorization = await call(page, handle, "authorize");
    assert.equal(typeof authorization, "string");
    await page.goto(authorization as string);
    const signIn = page.getByRole("heading", { name: "Sign in", exact: true });
    if (
      await signIn.waitFor({ state: "visible", timeout: 2000 }).then(
        () => true,
        () => false,
      )
    ) {
      await page.getByLabel("Username", { exact: true }).fill("admin");
      await page
        .getByLabel("Password", { exact: true })
        .fill("a strong bootstrap password");
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
    }
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    await page.waitForURL(
      (url) => url.origin === appOrigin && url.pathname === "/callback",
    );
    return (await call(page, handle, "complete")) as Summary;
  }
  async function resume(handle: string, expected: Summary, key: string) {
    await page.goto(appOrigin);
    const result = (await call(page, handle, "resume")) as Summary;
    for (const field of ["principalId", "grantId", "loginId", "dpop"] as const)
      assert.equal(result[field], expected[field]);
    assert.notEqual(result.runtime, expected.runtime);
    assert.notEqual(result.nkey, expected.nkey);
    assert.equal(result.keyId, key);
    return result;
  }
  try {
    const first = await login("rotation-single");
    const second = await login("rotation-double");
    assert.equal(first.keyId, second.keyId);
    const retained = await context.newPage();
    await retained.goto(appOrigin);
    await call(retained, "rotation-single", "tokenDigest");
    const k2 = await rotate();
    await page.goto(appOrigin);
    const before = await call(page, "rotation-double", "tokenDigest");
    for (const malformed of ["missing", "forged"] as const) {
      chain = malformed;
      const requests = refreshRequests;
      const priorTampering = tampered[malformed];
      await page.reload();
      assert.deepEqual(await call(page, "rotation-double", "resume"), {
        error: "OAuthTrustUnavailable",
      });
      assert.equal(tampered[malformed], priorTampering + 1);
      assert.equal(await call(page, "rotation-double", "tokenDigest"), before);
      assert.equal(
        refreshRequests,
        requests,
        "invalid trust must precede the spent marker and HTTP submission",
      );
    }
    chain = "valid";
    await resume("rotation-single", first, k2);
    const retainedResume = (await call(
      retained,
      "rotation-single",
      "resume",
      false,
    )) as Summary;
    assert.equal(retainedResume.keyId, k2);
    assert.equal(retainedResume.principalId, first.principalId);
    assert.equal(retainedResume.grantId, first.grantId);
    assert.deepEqual(
      await call(retained, "rotation-single", "resume", false, true),
      { error: "OAuthRecoveryRequired" },
    );
    const retried = (await call(
      retained,
      "rotation-single",
      "resume",
      false,
    )) as Summary;
    assert.equal(retried.keyId, k2);
    assert.equal(retried.runtime, retainedResume.runtime);
    assert.equal(retried.nkey, retainedResume.nkey);
    const k3 = await rotate();
    const twoHop = await resume("rotation-double", second, k3);
    assert.equal(
      BigInt(twoHop.rotationSequence) - BigInt(second.rotationSequence),
      2n,
    );
    await resume("rotation-single", first, k3);
    // The next metadata/preflight still names K3. Hold its actual token request,
    // rotate the real signer, and make only the subsequent chain response absent.
    pauseRefresh = true;
    assert.deepEqual(await call(page, "rotation-single", "resume"), {
      error: "OAuthTrustUnavailable",
    });
    const checkpoint = successes.at(-1);
    assert.ok(checkpoint && rotatedInFlight);
    assert.equal(
      await call(page, "rotation-single", "tokenDigest"),
      checkpoint.successor,
    );
    assert.notEqual(checkpoint.successor, checkpoint.spent);
    chain = "valid";
    await resume("rotation-single", first, rotatedInFlight);
    assert.equal(successes.at(-1)?.spent, checkpoint.successor);
    // Two ordinary tabs share real IndexedDB custody and the platform Web Lock.
    const other = await context.newPage();
    await other.goto(appOrigin);
    const start = successes.length;
    const concurrent = (await Promise.all([
      call(page, "rotation-single", "resume"),
      call(other, "rotation-single", "resume"),
    ])) as Summary[];
    assert.equal(successes.length - start, 2);
    assert.notEqual(successes[start].spent, successes[start + 1].spent);
    for (const value of concurrent) {
      assert.equal(value.grantId, first.grantId);
      assert.equal(value.keyId, rotatedInFlight);
    }
    assert.equal(failures.length, 0, failures.join("\n"));
  } finally {
    await browser.close();
    app.closeAllConnections();
    await new Promise<void>((resolve, reject) =>
      app.close((error) => (error ? reject(error) : resolve())),
    );
  }
}

try {
  if (mode === "native") await native();
  else await browser();
  assert.equal(
    proxyError,
    undefined,
    "chain failures must be intentional signed-byte tampering, not proxy errors",
  );
  console.log(JSON.stringify({ stage: "done", mode }));
} finally {
  lines.close();
  proxy.closeAllConnections();
  await new Promise<void>((resolve, reject) =>
    proxy.close((error) => (error ? reject(error) : resolve())),
  );
}
