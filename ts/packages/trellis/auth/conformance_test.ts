import { assert, assertEquals } from "@std/assert";

import {
  base64urlEncode,
  buildEventProofInput,
  buildProofInput,
  createAuth,
  sha256,
  utf8,
  verifyEventProof,
  verifyProof,
} from "./mod.ts";
import vectors from "../../../../integration/fixtures/protocol/authorization-context/vectors.json" with {
  type: "json",
};

type Chain = {
  sessionSeed: string;
  sessionPublicKey: string;
  contextDigest: string;
  requestProofInputHex: string;
  requestProofDigest: string;
  requestProof: string;
  eventProofInputHex: string;
  eventProofDigest: string;
  eventProof: string;
};

type VectorDefaults = {
  request: {
    subject: string;
    reply: string;
    payload: string;
    iat: number;
    requestId: string;
  };
  event: {
    descriptorIdentity: string;
    subject: string;
    payload: string;
    eventId: string;
    eventTime: string;
  };
};

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes).map((b) => b.toString(16).padStart(2, "0")).join("");
}

Deno.test("request and event proof v1 match language-neutral vectors", async () => {
  const chain = vectors.completeChain as unknown as Chain;
  const defaults = vectors.defaults as unknown as VectorDefaults;
  const auth = await createAuth({
    sessionKeySeed: chain.sessionSeed,
    contextDigest: chain.contextDigest,
  });
  assertEquals(auth.sessionKey, chain.sessionPublicKey);

  const requestPayloadHash = await sha256(utf8(defaults.request.payload));
  const requestProofInput = buildProofInput(
    chain.contextDigest,
    defaults.request.subject,
    defaults.request.reply,
    requestPayloadHash,
    defaults.request.iat,
    defaults.request.requestId,
  );
  assertEquals(toHex(requestProofInput), chain.requestProofInputHex);
  assertEquals(
    base64urlEncode(await sha256(requestProofInput)),
    chain.requestProofDigest,
  );
  const requestProof = await auth.createProof(
    defaults.request.subject,
    requestPayloadHash,
    defaults.request.reply,
    defaults.request.requestId,
    defaults.request.iat,
  );
  assertEquals(requestProof, chain.requestProof);
  assert(
    await verifyProof(
      auth.sessionKey,
      {
        contextDigest: chain.contextDigest,
        subject: defaults.request.subject,
        reply: defaults.request.reply,
        payloadHash: requestPayloadHash,
        iat: defaults.request.iat,
        requestId: defaults.request.requestId,
      },
      requestProof,
    ),
  );
  // A different reply subject breaks verification: the proof is bound to the
  // exact inbox the response arrives on.
  assert(
    !(await verifyProof(
      auth.sessionKey,
      {
        contextDigest: chain.contextDigest,
        subject: defaults.request.subject,
        reply: "_INBOX.other.reply",
        payloadHash: requestPayloadHash,
        iat: defaults.request.iat,
        requestId: defaults.request.requestId,
      },
      requestProof,
    )),
  );

  const eventPayloadHash = await sha256(utf8(defaults.event.payload));
  const eventProofInput = buildEventProofInput(
    chain.contextDigest,
    defaults.event.descriptorIdentity,
    defaults.event.subject,
    eventPayloadHash,
    defaults.event.eventId,
    defaults.event.eventTime,
  );
  assertEquals(toHex(eventProofInput), chain.eventProofInputHex);
  assertEquals(
    base64urlEncode(await sha256(eventProofInput)),
    chain.eventProofDigest,
  );
  const eventProof = base64urlEncode(
    await auth.sign(await sha256(eventProofInput)),
  );
  assertEquals(eventProof, chain.eventProof);
  assert(
    await verifyEventProof(
      auth.sessionKey,
      {
        contextDigest: chain.contextDigest,
        descriptorIdentity: defaults.event.descriptorIdentity,
        subject: defaults.event.subject,
        payloadHash: eventPayloadHash,
        eventId: defaults.event.eventId,
        eventTime: defaults.event.eventTime,
      },
      eventProof,
    ),
  );
  assert(
    !(await verifyEventProof(
      auth.sessionKey,
      {
        contextDigest: chain.contextDigest,
        descriptorIdentity: defaults.event.descriptorIdentity,
        subject: defaults.event.subject,
        payloadHash: eventPayloadHash,
        eventId: "evt_other",
        eventTime: defaults.event.eventTime,
      },
      eventProof,
    )),
  );
  assert(
    !(await verifyEventProof(
      auth.sessionKey,
      {
        contextDigest: chain.contextDigest,
        descriptorIdentity:
          "v1.ZG9jdW1lbnRzQHYx.RG9jdW1lbnRzLkNoYW5nZWQuT3RoZXI.1",
        subject: defaults.event.subject,
        payloadHash: eventPayloadHash,
        eventId: defaults.event.eventId,
        eventTime: defaults.event.eventTime,
      },
      eventProof,
    )),
  );
});
