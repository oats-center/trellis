import {
  initializeProtocolWasm,
  initializeProtocolWasmSync,
} from "./protocol_wasm.ts";
import * as protocol from "./protocol_wasm/trellis_protocol_wasm.js";
import { base64urlDecode, base64urlEncode, toArrayBuffer } from "./utils.ts";

/** Strict signature envelope shared by auth, bootstrap, and context refresh. */
export const SESSION_PROOF_FORMAT_V1 = "trellis.session-proof.v1" as const;

/** One protocol-owned signature purpose. */
export type SessionProofPurpose =
  | "userAuthRequest"
  | "userAuthBind"
  | "serviceBootstrap"
  | "deviceBootstrap"
  | "deviceEnrollment"
  | "authorizationContextRefresh";

/** Inputs validated and encoded by the Rust session-proof protocol. */
export type SessionProofInput =
  | {
    purpose: "userAuthBind";
    origin: string;
    flowId: string;
    sessionPublicKey: string;
    unsignedRequest: Record<string, unknown> & { issuedAt: number };
  }
  | {
    purpose: "userAuthRequest";
    origin: string;
    unsignedRequest: Record<string, unknown> & { issuedAt: number };
  }
  | {
    purpose: "serviceBootstrap" | "deviceBootstrap" | "deviceEnrollment";
    origin: string;
    unsignedRequest: Record<string, unknown> & { iat: number };
  }
  | {
    purpose: "authorizationContextRefresh";
    origin: string;
    sessionPublicKey: string;
    unsignedRequest: Record<string, unknown> & { issuedAt: number };
  };

/** One strict session-proof signature envelope. */
export type SessionProof = {
  format: typeof SESSION_PROOF_FORMAT_V1;
  signature: string;
};

/** Freshness limits enforced by the Rust verifier. */
export type SessionProofPolicy = {
  maximumAgeMs: number;
  maximumFutureSkewMs: number;
};

/** Parse and validate one signature envelope through Rust. */
export function parseSessionProof(value: unknown): SessionProof {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocol.parse_session_proof(JSON.stringify(value)),
  ) as SessionProof;
}

/** Hash a browser proof-bearing request using its protocol-owned payload rules. */
export async function sessionProofRequestDigest(
  request: Record<string, unknown>,
): Promise<string> {
  await initializeProtocolWasm();
  return protocol.session_proof_request_digest(JSON.stringify(request));
}

/** Return the protocol-owned signing digest without implementing its framing in JS. */
export async function sessionProofSigningDigest(
  input: SessionProofInput,
): Promise<string> {
  await initializeProtocolWasm();
  return protocol.session_proof_signing_digest(JSON.stringify(input));
}

/** Sign the Rust-produced digest, then verify the declared key and signature in Rust. */
export async function signSessionProof(
  input: SessionProofInput,
  privateKey: CryptoKey,
  signerPublicKey: string,
): Promise<SessionProof> {
  const digest = base64urlDecode(await sessionProofSigningDigest(input));
  const signature = await crypto.subtle.sign(
    { name: "Ed25519" },
    privateKey,
    toArrayBuffer(digest),
  );
  const proof: SessionProof = {
    format: SESSION_PROOF_FORMAT_V1,
    signature: base64urlEncode(new Uint8Array(signature)),
  };
  await verifySessionProof(
    input,
    proof,
    signerPublicKey,
    Number(
      input.purpose === "serviceBootstrap" ||
        input.purpose === "deviceBootstrap" ||
        input.purpose === "deviceEnrollment"
        ? input.unsignedRequest.iat
        : input.unsignedRequest.issuedAt,
    ),
    { maximumAgeMs: 0, maximumFutureSkewMs: 0 },
  );
  return proof;
}

/** Verify signature, signer binding, input shape, and freshness through Rust. */
export async function verifySessionProof(
  input: SessionProofInput,
  proof: SessionProof,
  signerPublicKey: string,
  nowMs: number,
  policy: SessionProofPolicy = {
    maximumAgeMs: 30_000,
    maximumFutureSkewMs: 30_000,
  },
): Promise<void> {
  await initializeProtocolWasm();
  protocol.verify_session_proof(
    JSON.stringify(input),
    JSON.stringify(proof),
    signerPublicKey,
    nowMs,
    policy.maximumAgeMs,
    policy.maximumFutureSkewMs,
  );
}
