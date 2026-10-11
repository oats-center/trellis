import type { Ed25519Signer } from "./crypto.ts";
import {
  sessionRequestSigningDigestWasm,
  sessionResponseSigningDigestWasm,
  sessionEventSigningDigestWasm,
  type SessionEvent,
  type SessionRequest,
  type SessionResponse,
} from "./protocol_wasm.ts";
import { base64urlEncode } from "./utils.ts";

/** Request proof and shared transcript digest retained until authenticated completion. */
export type RequestSignature = {
  readonly proof: string;
  readonly requestProofDigest: Uint8Array;
};

/** Sign the shared Rust transcript over the exact request publication buffer. */
export async function createProof(
  signer: Ed25519Signer,
  request: SessionRequest,
  payload: Uint8Array,
): Promise<RequestSignature> {
  const requestProofDigest = await sessionRequestSigningDigestWasm(
    request,
    payload,
  );
  return {
    proof: base64urlEncode(await signer.sign(requestProofDigest)),
    requestProofDigest,
  };
}

/** Sign catalog-stamped event identity; no subject parsing or independent framing. */
export async function createEventProof(
  signer: Ed25519Signer,
  event: SessionEvent,
  payload: Uint8Array,
): Promise<string> {
  return base64urlEncode(
    await signer.sign(await sessionEventSigningDigestWasm(event, payload)),
  );
}

/** Sign actual result bytes and the retained opening request as a provider. */
export async function createResponseProof(
  signer: Ed25519Signer,
  response: SessionResponse,
  payload: Uint8Array,
): Promise<string> {
  return base64urlEncode(
    await signer.sign(
      await sessionResponseSigningDigestWasm(response, payload),
    ),
  );
}
