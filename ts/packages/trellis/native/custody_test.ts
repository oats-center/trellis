import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { createAccount } from "@nats-io/nkeys";
import { ulid } from "ulid";
import {
  MemoryNativeOAuthCustody,
  NativeOAuthClient,
  type NativeCredentialScope,
  type RuntimeAuthorizationTrust,
} from "../native.ts";

const scope: NativeCredentialScope = {
  issuer: "http://127.0.0.1:1234",
  instanceId: ulid(),
  account: createAccount().getPublicKey(),
  clientId: "native-client",
  credentialHandle: ulid(),
};
const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
  "sign",
  "verify",
]);
const bytes = new Uint8Array(
  await crypto.subtle.exportKey("raw", pair.publicKey),
);
const trust: RuntimeAuthorizationTrust = {
  instanceId: scope.instanceId,
  account: scope.account,
  issuer: {
    keyId: createHash("sha256").update(bytes).digest("base64url"),
    publicKey: Buffer.from(bytes).toString("base64url"),
    state: "active",
  },
  rotationSequence: "0",
};

test("explicit memory custody retains an unsubmitted successor and its signing key across lease replacement", async () => {
  const custody = new MemoryNativeOAuthCustody();
  const first = await custody.acquire(scope, trust);
  const input = new Uint8Array(
    new TextEncoder().encode("real native DPoP signing input"),
  );
  const key = await crypto.subtle.importKey(
    "jwk",
    first.publicJwk,
    { name: "ECDSA", namedCurve: "P-256" },
    false,
    ["verify"],
  );
  await first.install({
    principalId: ulid(),
    oauthGrantId: ulid(),
    loginSessionId: ulid(),
    refreshToken: "predecessor",
  });
  assert.equal(await first.beginRotation(), "predecessor");
  await assert.rejects(async () => await first.beginRotation(), {
    reason: "OAuthRecoveryRequired",
  });
  await assert.rejects(
    async () => await first.retainRotatedToken("predecessor"),
    { reason: "OAuthRecoveryRequired" },
  );
  await first.retainRotatedToken("received-successor");
  await first.close();
  const next = await custody.acquire(scope, trust);
  try {
    assert.equal(
      await crypto.subtle.verify(
        { name: "ECDSA", hash: "SHA-256" },
        key,
        await next.sign(input),
        input,
      ),
      true,
    );
    assert.equal(await next.beginRotation(), "received-successor");
    await assert.rejects(async () => await next.beginRotation(), {
      reason: "OAuthRecoveryRequired",
    });
  } finally {
    await next.close();
  }
});

test("an ambiguous refresh remains spent when the process-local family is acquired again", async () => {
  const custody = new MemoryNativeOAuthCustody();
  const first = await custody.acquire(scope, trust);
  await first.install({
    principalId: ulid(),
    oauthGrantId: ulid(),
    loginSessionId: ulid(),
    refreshToken: "one-use",
  });
  await first.beginRotation();
  await first.close();
  const next = await custody.acquire(scope, trust);
  try {
    await assert.rejects(async () => await next.beginRotation(), {
      reason: "OAuthRecoveryRequired",
    });
  } finally {
    await next.close();
  }
});

test("a second client cannot acquire an owned family and releasing ownership permits signing again", async () => {
  const custody = new MemoryNativeOAuthCustody();
  const client = (
    await NativeOAuthClient.open({ scope, trust, custody })
  ).orThrow();
  try {
    const contender = await NativeOAuthClient.open({ scope, trust, custody });
    assert.equal(contender.error?.reason, "CredentialAlreadyConnected");
  } finally {
    await client.close();
  }
  const next = await custody.acquire(scope, trust);
  try {
    await client.close();
    await assert.rejects(custody.acquire(scope, trust), {
      reason: "CredentialAlreadyConnected",
    });
    const input = new Uint8Array([1, 2, 3]);
    const key = await crypto.subtle.importKey(
      "jwk",
      next.publicJwk,
      { name: "ECDSA", namedCurve: "P-256" },
      false,
      ["verify"],
    );
    assert.equal(
      await crypto.subtle.verify(
        { name: "ECDSA", hash: "SHA-256" },
        key,
        await next.sign(input),
        input,
      ),
      true,
    );
  } finally {
    await next.close();
  }
});

test("scope replacement and account-switch reauthorization preserve the existing credential", async () => {
  const custody = new MemoryNativeOAuthCustody();
  const first = await custody.acquire(scope, trust);
  const grant = {
    principalId: ulid(),
    oauthGrantId: ulid(),
    loginSessionId: ulid(),
    refreshToken: "original",
  };
  await first.install(grant);
  await assert.rejects(
    async () =>
      await first.install({
        ...grant,
        principalId: ulid(),
        refreshToken: "foreign",
      }),
    { reason: "InvalidCredential" },
  );
  await first.close();
  await assert.rejects(
    custody.acquire({ ...scope, credentialHandle: ulid() }, trust),
    { reason: "InvalidCredential" },
  );
  const next = await custody.acquire(scope, trust);
  try {
    assert.equal(await next.beginRotation(), "original");
  } finally {
    await next.close();
  }
});

test("a default client cannot implicitly resume another process-local credential family", async () => {
  const custody = new MemoryNativeOAuthCustody();
  const original = await custody.acquire(scope, trust);
  await original.install({
    principalId: ulid(),
    oauthGrantId: ulid(),
    loginSessionId: ulid(),
    refreshToken: "owned",
  });
  await original.close();
  const independent = (
    await NativeOAuthClient.open({ scope, trust })
  ).orThrow();
  try {
    assert.equal(
      (await independent.resume()).error?.reason,
      "OAuthRecoveryRequired",
    );
  } finally {
    await independent.close();
  }
  const retained = await custody.acquire(scope, trust);
  try {
    assert.equal(await retained.beginRotation(), "owned");
  } finally {
    await retained.close();
  }
});

test("reacquiring custody preserves its authenticated issuer trust floor", async () => {
  const custody = new MemoryNativeOAuthCustody();
  const first = await custody.acquire(scope, trust);
  const successor = await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
    "sign",
    "verify",
  ]);
  const publicKey = new Uint8Array(
    await crypto.subtle.exportKey("raw", successor.publicKey),
  );
  await first.retainTrust({
    ...trust,
    issuer: {
      keyId: createHash("sha256").update(publicKey).digest("base64url"),
      publicKey: Buffer.from(publicKey).toString("base64url"),
      state: "active",
    },
    rotationSequence: "1",
  });
  await first.close();
  const next = await custody.acquire(scope, trust);
  try {
    await assert.rejects(async () => await next.retainTrust(trust), {
      reason: "InvalidCredential",
    });
    assert.equal(next.trust.rotationSequence, "1");
  } finally {
    await next.close();
  }
});
