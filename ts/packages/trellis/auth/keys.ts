import nacl from "tweetnacl";

import { base64urlDecode, base64urlEncode, toArrayBuffer } from "./utils.ts";

const ED25519_PKCS8_PREFIX = Uint8Array.from([
  0x30,
  0x2e,
  0x02,
  0x01,
  0x00,
  0x30,
  0x05,
  0x06,
  0x03,
  0x2b,
  0x65,
  0x70,
  0x04,
  0x22,
  0x04,
  0x20,
]);

export function pkcs8FromEd25519Seed(seed32: Uint8Array): Uint8Array {
  if (seed32.length !== 32) {
    throw new Error(
      `Invalid Ed25519 seed length: ${seed32.length} (expected 32)`,
    );
  }
  const pkcs8 = new Uint8Array(ED25519_PKCS8_PREFIX.length + seed32.length);
  pkcs8.set(ED25519_PKCS8_PREFIX, 0);
  pkcs8.set(seed32, ED25519_PKCS8_PREFIX.length);
  return pkcs8;
}

function validateEd25519Seed(seed32: Uint8Array): Uint8Array {
  if (seed32.length !== 32) {
    throw new Error(
      `Invalid Ed25519 seed length: ${seed32.length} (expected 32)`,
    );
  }
  return seed32;
}

function ed25519KeyPairFromSeed(seed32: Uint8Array): nacl.SignKeyPair {
  return nacl.sign.keyPair.fromSeed(validateEd25519Seed(seed32));
}

export function publicKeyBase64urlFromSeed(seed32: Uint8Array): string {
  return base64urlEncode(ed25519KeyPairFromSeed(seed32).publicKey);
}

export async function importEd25519PrivateKeyFromSeedBase64url(
  seedBase64url: string,
): Promise<CryptoKey> {
  const seed = base64urlDecode(seedBase64url);
  if (seed.length !== 32) {
    throw new Error(
      `Invalid Ed25519 seed length: ${seed.length} (expected 32)`,
    );
  }
  const pkcs8 = pkcs8FromEd25519Seed(seed);
  return await crypto.subtle.importKey(
    "pkcs8",
    toArrayBuffer(pkcs8),
    { name: "Ed25519" },
    true,
    ["sign"],
  );
}

export async function publicKeyBase64urlFromPrivateKey(
  privateKey: CryptoKey,
): Promise<string> {
  const jwk = await crypto.subtle.exportKey("jwk", privateKey) as JsonWebKey;
  if (typeof jwk.x !== "string" || jwk.x.length === 0) {
    throw new Error("Failed to derive Ed25519 public key (missing JWK.x)");
  }
  return jwk.x;
}

export async function importEd25519PublicKeyFromBase64url(
  publicKeyBase64url: string,
): Promise<CryptoKey> {
  const raw = base64urlDecode(publicKeyBase64url);
  if (raw.length !== 32) {
    throw new Error(
      `Invalid Ed25519 public key length: ${raw.length} (expected 32)`,
    );
  }
  return await crypto.subtle.importKey(
    "raw",
    toArrayBuffer(raw),
    { name: "Ed25519" },
    true,
    ["verify"],
  );
}
