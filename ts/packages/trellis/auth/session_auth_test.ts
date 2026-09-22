import { assert, assertEquals, assertThrows } from "@std/assert";
import {
  base64urlDecode,
  base64urlEncode,
  buildEventProofInput,
  correctedIatSeconds,
  createAuth,
  sha256,
  trellisIdFromOriginId,
  utf8,
  verifyEventProof,
  verifyProof,
} from "./mod.ts";

function authTokenFromAuthenticatorResult(value: unknown): string {
  if (!value || typeof value !== "object") {
    throw new Error(
      "Expected NATS authenticator to return an auth token payload",
    );
  }

  const record = value as { auth_token?: unknown };
  if (typeof record.auth_token !== "string") {
    throw new Error("Expected NATS authenticator to return auth_token");
  }

  return record.auth_token;
}

Deno.test("createAuth derives sessionKey from 32-byte seed", async () => {
  const seed = base64urlEncode(crypto.getRandomValues(new Uint8Array(32)));
  const auth = await createAuth({ sessionKeySeed: seed });

  const pk = base64urlDecode(auth.sessionKey);
  assertEquals(pk.length, 32);
});

Deno.test("proof creation and verification match ADR format", async () => {
  const seed = base64urlEncode(crypto.getRandomValues(new Uint8Array(32)));
  const contextDigest = base64urlEncode(await sha256(utf8("context")));
  const auth = await createAuth({ sessionKeySeed: seed, contextDigest });

  const subject = "rpc.v1.User.Find";
  const reply = "_INBOX.test.reply";
  const payloadHash = await sha256(
    utf8(JSON.stringify({ userId: { origin: "github", id: "1" } })),
  );
  const iat = 1_700_000_000;
  const requestId = "req_123";
  const proof = await auth.createProof(
    subject,
    payloadHash,
    reply,
    requestId,
    iat,
  );

  const ok = await verifyProof(
    auth.sessionKey,
    {
      contextDigest,
      subject,
      reply,
      payloadHash,
      iat,
      requestId,
    },
    proof,
  );
  assert(ok);

  const bad = await verifyProof(
    auth.sessionKey,
    {
      contextDigest,
      subject,
      reply,
      payloadHash: await sha256(utf8("different")),
      iat,
      requestId,
    },
    proof,
  );
  assertEquals(bad, false);

  const wrongReply = await verifyProof(
    auth.sessionKey,
    {
      contextDigest,
      subject,
      reply: "_INBOX.other.reply",
      payloadHash,
      iat,
      requestId,
    },
    proof,
  );
  assertEquals(wrongReply, false);
});

Deno.test("event proof uses event id and event time domain", async () => {
  const seed = base64urlEncode(crypto.getRandomValues(new Uint8Array(32)));
  const contextDigest = base64urlEncode(await sha256(utf8("context")));
  const auth = await createAuth({ sessionKeySeed: seed, contextDigest });
  const payloadHash = await sha256(utf8(JSON.stringify({ value: "one" })));
  const eventId = "evt_123";
  const eventTime = "2026-04-26T00:00:00.000Z";
  const descriptorIdentity = "v1.dGhpbmdAdjE.VGhpbmcuQ2hhbmdlZA.1";
  const digest = await sha256(
    buildEventProofInput(
      contextDigest,
      descriptorIdentity,
      "events.v1.Thing.Changed.one",
      payloadHash,
      eventId,
      eventTime,
    ),
  );
  const proof = base64urlEncode(await auth.sign(digest));

  assert(
    await verifyEventProof(
      auth.sessionKey,
      {
        contextDigest,
        descriptorIdentity,
        subject: "events.v1.Thing.Changed.one",
        payloadHash,
        eventId,
        eventTime,
      },
      proof,
    ),
  );
  assertEquals(
    await verifyEventProof(
      auth.sessionKey,
      {
        contextDigest,
        descriptorIdentity,
        subject: "events.v1.Thing.Changed.one",
        payloadHash,
        eventId: "evt_other",
        eventTime,
      },
      proof,
    ),
    false,
  );
});

Deno.test("trellisIdFromOriginId is stable and 22 chars", async () => {
  const id1 = await trellisIdFromOriginId("github", "123");
  const id2 = await trellisIdFromOriginId("github", "123");
  const id3 = await trellisIdFromOriginId("github", "124");

  assertEquals(id1.length, 22);
  assertEquals(id1, id2);
  assert(id1 !== id3);
});

Deno.test("natsConnectOptions returns context-bound reconnect tokens", async () => {
  const seed = base64urlEncode(crypto.getRandomValues(new Uint8Array(32)));
  const auth = await createAuth({ sessionKeySeed: seed });
  const contextDigest = base64urlEncode(await sha256(utf8("context")));
  const options = await auth.natsConnectOptions({
    sessionId: "ses_test",
    contextDigest,
    jwt: "deny-all-jwt",
  });
  const authenticators = Array.isArray(options.authenticator)
    ? options.authenticator
    : [options.authenticator];
  const jwt = authenticators[0]("nonce-a") as { jwt: string };
  const token = JSON.parse(
    authTokenFromAuthenticatorResult(authenticators[1]("nonce-a")),
  ) as { format: string; contextDigest: string };

  assertEquals(options.inboxPrefix, "_INBOX.ses_test");
  assertEquals(jwt.jwt, "deny-all-jwt");
  assertEquals(token.format, "trellis.nats-connect-token.v1");
  assertEquals(token.contextDigest, contextDigest);
  assertEquals(Object.keys(token).sort(), ["contextDigest", "format"]);
});

Deno.test("createAuth applies server clock offsets to current iat", async () => {
  const seed = base64urlEncode(crypto.getRandomValues(new Uint8Array(32)));
  const auth = await createAuth({ sessionKeySeed: seed });
  const originalNow = Date.now;

  try {
    Date.now = () => 1_700_000_000_250;
    auth.setServerClockOffsetMs(900);

    assertEquals(auth.currentIat(), correctedIatSeconds(Date.now(), 900));
  } finally {
    Date.now = originalNow;
  }
});
