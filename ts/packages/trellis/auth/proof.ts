import {
  base64urlDecode,
  base64urlEncode,
  sha256,
  toArrayBuffer,
  utf8,
} from "./utils.ts";
import { importEd25519PublicKeyFromBase64url } from "./keys.ts";
import { AsyncResult } from "@oatscenter/result";

export type ProofParams = {
  contextDigest: string;
  subject: string;
  reply: string;
  payloadHash: Uint8Array;
  iat: number;
  requestId: string;
};

export type EventProofParams = {
  contextDigest: string;
  descriptorIdentity: string;
  subject: string;
  payloadHash: Uint8Array;
  eventId: string;
  eventTime: string;
};

const REQUEST_PROOF_DOMAIN = utf8("trellis.authorization-request-proof.v1");
const EVENT_PROOF_DOMAIN = utf8("trellis.authorization-event-proof.v1");

function appendLengthPrefixed(
  buf: Uint8Array,
  view: DataView,
  offset: number,
  value: Uint8Array,
): number {
  view.setUint32(offset, value.length);
  const valueOffset = offset + 4;
  buf.set(value, valueOffset);
  return valueOffset + value.length;
}

function buildLengthPrefixed(
  components: Uint8Array[],
): Uint8Array {
  const total = components.reduce(
    (sum, value) => sum + 4 + value.length,
    0,
  );
  const buf = new Uint8Array(total);
  const view = new DataView(buf.buffer);

  let offset = 0;
  for (const component of components) {
    offset = appendLengthPrefixed(buf, view, offset, component);
  }
  return buf;
}

/**
 * Builds the canonical v1 context-bound request proof input bytes.
 *
 * The exact NATS reply inbox must be created before signing and is bound into
 * the proof input, matching the runtime request verifier.
 */
export function buildProofInput(
  contextDigest: string,
  subject: string,
  reply: string,
  payloadHash: Uint8Array,
  iat: number,
  requestId: string,
): Uint8Array {
  const contextDigestBytes = base64urlDecode(contextDigest);
  if (contextDigestBytes.length !== 32) {
    throw new Error("authorization context digest must encode 32 bytes");
  }
  if (reply.length === 0) {
    throw new Error("request reply subject must not be empty");
  }
  return buildLengthPrefixed([
    REQUEST_PROOF_DOMAIN,
    contextDigestBytes,
    utf8(subject),
    utf8(reply),
    payloadHash,
    utf8(String(iat)),
    utf8(requestId),
  ]);
}

/**
 * Builds the canonical v1 context-bound event proof input bytes.
 */
export function buildEventProofInput(
  contextDigest: string,
  descriptorIdentity: string,
  subject: string,
  payloadHash: Uint8Array,
  eventId: string,
  eventTime: string,
): Uint8Array {
  const contextDigestBytes = base64urlDecode(contextDigest);
  if (contextDigestBytes.length !== 32) {
    throw new Error("authorization context digest must encode 32 bytes");
  }
  return buildLengthPrefixed([
    EVENT_PROOF_DOMAIN,
    contextDigestBytes,
    utf8(descriptorIdentity),
    utf8(subject),
    payloadHash,
    utf8(eventId),
    utf8(eventTime),
  ]);
}

export async function createProof(
  privateKey: CryptoKey,
  params: ProofParams,
): Promise<string> {
  const input = buildProofInput(
    params.contextDigest,
    params.subject,
    params.reply,
    params.payloadHash,
    params.iat,
    params.requestId,
  );
  const digest = await sha256(input);
  const sig = await crypto.subtle.sign(
    { name: "Ed25519" },
    privateKey,
    toArrayBuffer(digest),
  );
  return base64urlEncode(new Uint8Array(sig));
}

export async function createEventProof(
  privateKey: CryptoKey,
  params: EventProofParams,
): Promise<string> {
  const input = buildEventProofInput(
    params.contextDigest,
    params.descriptorIdentity,
    params.subject,
    params.payloadHash,
    params.eventId,
    params.eventTime,
  );
  const digest = await sha256(input);
  const sig = await crypto.subtle.sign(
    { name: "Ed25519" },
    privateKey,
    toArrayBuffer(digest),
  );
  return base64urlEncode(new Uint8Array(sig));
}

export async function verifyProof(
  publicSessionKey: string,
  params: ProofParams,
  proofBase64url: string,
): Promise<boolean> {
  const result = await AsyncResult.try(async () => {
    const input = buildProofInput(
      params.contextDigest,
      params.subject,
      params.reply,
      params.payloadHash,
      params.iat,
      params.requestId,
    );
    const digest = await sha256(input);
    const signature = base64urlDecode(proofBase64url);
    const pub = await importEd25519PublicKeyFromBase64url(publicSessionKey);
    return crypto.subtle.verify(
      { name: "Ed25519" },
      pub,
      toArrayBuffer(signature),
      toArrayBuffer(digest),
    );
  });
  return result.unwrapOr(false);
}

export async function verifyEventProof(
  publicSessionKey: string,
  params: EventProofParams,
  proofBase64url: string,
): Promise<boolean> {
  const result = await AsyncResult.try(async () => {
    const input = buildEventProofInput(
      params.contextDigest,
      params.descriptorIdentity,
      params.subject,
      params.payloadHash,
      params.eventId,
      params.eventTime,
    );
    const digest = await sha256(input);
    const signature = base64urlDecode(proofBase64url);
    const pub = await importEd25519PublicKeyFromBase64url(publicSessionKey);
    return crypto.subtle.verify(
      { name: "Ed25519" },
      pub,
      toArrayBuffer(signature),
      toArrayBuffer(digest),
    );
  });
  return result.unwrapOr(false);
}
