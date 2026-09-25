import nacl from "tweetnacl";

import { base64urlEncode } from "./utils.ts";

function validateEd25519Seed(seed32: Uint8Array): Uint8Array {
  if (seed32.length !== 32) {
    throw new Error(
      `Invalid Ed25519 seed length: ${seed32.length} (expected 32)`,
    );
  }
  return seed32;
}

/**
 * Derives the raw Ed25519 public key for a 32-byte seed as unpadded base64url.
 *
 * Public-key derivation is implementation-independent because it runs on the
 * shared Ed25519 key schedule rather than a platform crypto handle.
 */
export function publicKeyBase64urlFromSeed(seed32: Uint8Array): string {
  return base64urlEncode(
    nacl.sign.keyPair.fromSeed(validateEd25519Seed(seed32)).publicKey,
  );
}
