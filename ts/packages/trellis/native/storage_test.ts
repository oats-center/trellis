import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import {
  mkdtemp,
  rm,
  symlink,
  chmod,
  link,
  readdir,
  rename,
  open,
} from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { createAccount } from "@nats-io/nkeys";
import { createHash } from "node:crypto";
import { NativeCredentialLease } from "./storage.ts";
import type { NativeCredentialScope } from "./storage.ts";
import {
  SessionAuthorityOwner,
  type RuntimeAuthorizationTrust,
} from "../auth/authority.ts";

const scope: NativeCredentialScope = {
  issuer: "http://127.0.0.1:1234",
  instanceId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
  account: createAccount().getPublicKey(),
  clientId: "native-client",
  credentialHandle: "01ARZ3NDEKTSV4RRFFQ69G5FAW",
};
const issuerPair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
  "sign",
  "verify",
]);
const issuerBytes = new Uint8Array(
  await crypto.subtle.exportKey("raw", issuerPair.publicKey),
);
const trust: RuntimeAuthorizationTrust = {
  instanceId: scope.instanceId,
  account: scope.account,
  issuer: {
    keyId: createHash("sha256").update(issuerBytes).digest("base64url"),
    publicKey: Buffer.from(issuerBytes).toString("base64url"),
    state: "active",
  },
  rotationSequence: "0",
};

test(
  "exclusive native custody survives cross-process contention and crashes without replaying a spent refresh",
  { timeout: 10000 },
  async (context) => {
    const directory = await mkdtemp("/tmp/opencode/native-oauth-custody-");
    let child: ReturnType<typeof spawn> | undefined;
    let exited: Promise<unknown> | undefined;
    try {
      const first = await NativeCredentialLease.open(directory, scope, trust);
      first.install({
        principalId: "principal",
        oauthGrantId: "grant",
        loginSessionId: "login",
        refreshToken: "first",
      });
      const publicKey = first.publicJwk;
      const input = new TextEncoder().encode("real ES256 proof");
      const signature = await first.sign(input);
      const verify = await crypto.subtle.importKey(
        "jwk",
        publicKey,
        { name: "ECDSA", namedCurve: "P-256" },
        false,
        ["verify"],
      );
      assert.equal(
        await crypto.subtle.verify(
          { name: "ECDSA", hash: "SHA-256" },
          verify,
          signature,
          input,
        ),
        true,
      );
      await first.close();
      child = spawn(
        process.execPath,
        [
          "--experimental-transform-types",
          "--input-type=module",
          "-e",
          `
      import { NativeCredentialLease } from ${JSON.stringify(new URL("./storage.ts", import.meta.url).href)};
      const lease = await NativeCredentialLease.open(${JSON.stringify(directory)}, ${JSON.stringify(scope)}, ${JSON.stringify(trust)});
      if (lease.beginRotation() !== "first") throw new Error("wrong refresh");
      console.log("spent committed while lease held");
      setInterval(() => {}, 60000);
    `,
        ],
        { stdio: ["ignore", "pipe", "inherit"] },
      );
      exited = once(child, "exit");
      assert.ok(child.stdout);
      const [output] = await Promise.race([
        once(child.stdout, "data", { signal: context.signal }),
        exited.then(() => {
          throw new Error("custody holder exited before readiness");
        }),
      ]);
      assert.match(output.toString(), /spent committed/);
      await assert.rejects(
        NativeCredentialLease.open(
          directory,
          scope,
          trust,
          AbortSignal.timeout(100),
        ),
        { name: "AbortError" },
      );
      const [slot] = await readdir(directory);
      const leasePath = join(directory, slot, "lease.sqlite");
      await rename(leasePath, `${leasePath}.held`);
      const replacement = await open(leasePath, "wx+", 0o600);
      await replacement.close();
      await assert.rejects(
        NativeCredentialLease.open(directory, scope, trust),
        /ReplacedCredentialLease/,
      );
      await rm(leasePath);
      await rename(`${leasePath}.held`, leasePath);
      child.kill("SIGKILL");
      await exited;
      const recovered = await NativeCredentialLease.open(
        directory,
        scope,
        trust,
      );
      assert.deepEqual(recovered.publicJwk, publicKey);
      assert.throws(() => recovered.beginRotation(), /OAuthRecoveryRequired/);
      recovered.install({
        principalId: "principal",
        oauthGrantId: "new-grant",
        loginSessionId: "login",
        refreshToken: "replacement",
      });
      assert.equal(recovered.beginRotation(), "replacement");
      await recovered.close();
      const reopened = await NativeCredentialLease.open(
        directory,
        scope,
        trust,
      );
      assert.throws(() => reopened.beginRotation(), /OAuthRecoveryRequired/);
      await reopened.close();
    } finally {
      child?.kill("SIGKILL");
      if (exited) await exited;
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test("native custody rejects an unsigned higher-sequence issuer and preserves the pin across reopen", async () => {
  const directory = await mkdtemp("/tmp/opencode/native-oauth-trust-");
  try {
    const first = await NativeCredentialLease.open(directory, scope, trust);
    const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
      "sign",
      "verify",
    ]);
    const bytes = new Uint8Array(
      await crypto.subtle.exportKey("raw", pair.publicKey),
    );
    const forged = new SessionAuthorityOwner({
      instanceId: scope.instanceId,
      account: scope.account,
      rotationSequence: "1",
      issuers: [
        {
          state: "active",
          publicKey: Buffer.from(bytes).toString("base64url"),
          keyId: createHash("sha256").update(bytes).digest("base64url"),
        },
      ],
    });
    assert.throws(() => first.retainTrust(forged), /anchor mismatch/);
    forged.stop();
    await first.close();
    const reopened = await NativeCredentialLease.open(directory, scope, trust);
    assert.deepEqual(reopened.trust, trust);
    await reopened.close();
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("native custody rejects exposed directory permissions and hard-linked credential files", async () => {
  const directory = await mkdtemp("/tmp/opencode/native-oauth-protection-");
  try {
    const first = await NativeCredentialLease.open(directory, scope, trust);
    await first.close();
    await chmod(directory, 0o755);
    await assert.rejects(
      NativeCredentialLease.open(directory, scope, trust),
      /UnsafeCredentialStorage/,
    );
    await chmod(directory, 0o700);
    const [slot] = await readdir(directory);
    await link(
      join(directory, slot, "credential.sqlite"),
      join(directory, "copy"),
    );
    await assert.rejects(
      NativeCredentialLease.open(directory, scope, trust),
      /UnsafeCredentialStorage/,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test(
  "closing a same-process contender cannot drop the owner's cross-process SQLite lock",
  { timeout: 10000 },
  async () => {
    const directory = await mkdtemp("/tmp/opencode/native-oauth-process-lock-");
    const owner = await NativeCredentialLease.open(directory, scope, trust);
    try {
      await assert.rejects(
        NativeCredentialLease.open(
          directory,
          scope,
          trust,
          AbortSignal.timeout(100),
        ),
      );
      const child = spawn(
        process.execPath,
        [
          "--experimental-transform-types",
          "--input-type=module",
          "-e",
          `
      import assert from "node:assert/strict";
      import {NativeCredentialLease} from ${JSON.stringify(new URL("./storage.ts", import.meta.url).href)};
      await assert.rejects(NativeCredentialLease.open(${JSON.stringify(directory)},${JSON.stringify(scope)},${JSON.stringify(trust)},AbortSignal.timeout(100)),{name:"AbortError"});
    `,
        ],
        { stdio: ["ignore", "ignore", "inherit"] },
      );
      const [code] = await once(child, "exit");
      assert.equal(code, 0);
    } finally {
      await owner.close();
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test("native custody rejects symlink directories instead of writing credentials through them", async () => {
  const directory = await mkdtemp("/tmp/opencode/native-oauth-path-");
  try {
    const link = join(directory, "link");
    await symlink(directory, link);
    await assert.rejects(
      NativeCredentialLease.open(link, scope, trust),
      /UnsafeCredentialStorage/,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
