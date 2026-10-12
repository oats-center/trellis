import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { test } from "node:test";
import { createAccount } from "@nats-io/nkeys";
import { discoverNativeOAuth, NativeOAuthClient } from "../native.ts";

test("public native custody requires OAuth for a fresh slot and creates a new runtime identity after reopen", async () => {
  const directory = await mkdtemp("/tmp/opencode/native-public-client-");
  const issuer = await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
    "sign",
    "verify",
  ]);
  const bytes = new Uint8Array(
    await crypto.subtle.exportKey("raw", issuer.publicKey),
  );
  const options = {
    directory,
    scope: {
      issuer: "http://127.0.0.1:1234",
      instanceId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
      account: createAccount().getPublicKey(),
      clientId: "native-app",
      credentialHandle: "01ARZ3NDEKTSV4RRFFQ69G5FAW",
    },
    trust: {
      instanceId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
      account: "",
      issuer: {
        keyId: createHash("sha256").update(bytes).digest("base64url"),
        publicKey: Buffer.from(bytes).toString("base64url"),
        state: "active" as const,
      },
      rotationSequence: "0",
    },
  };
  options.trust.account = options.scope.account;
  const clients: NativeOAuthClient[] = [];
  try {
    const first = (await NativeOAuthClient.open(options)).orThrow();
    clients.push(first);
    const firstKey = first.auth.sessionKey;
    const firstRuntime = first.runtimeId;
    const missing = await first.resume();
    assert.equal(missing.isErr(), true);
    assert.equal(missing.error.reason, "OAuthRecoveryRequired");
    await first.close();
    const reopened = (await NativeOAuthClient.open(options)).orThrow();
    clients.push(reopened);
    assert.notEqual(reopened.auth.sessionKey, firstKey);
    assert.notEqual(reopened.runtimeId, firstRuntime);
    await reopened.close();
  } finally {
    await Promise.all(clients.map((client) => client.close()));
    await rm(directory, { recursive: true, force: true });
  }
});

test("native discovery rejects insecure non-loopback issuers and URLs carrying credentials", async () => {
  for (const origin of [
    "http://example.com",
    "https://user:password@example.com",
    "https://example.com/issuer",
  ]) {
    const result = await discoverNativeOAuth(origin);
    assert.equal(result.isErr(), true);
    assert.equal(result.error.reason, "InvalidIssuer");
  }
});
