/**
 * @module
 * Internal Trellis cryptography boundary.
 *
 * One process/document implementation performs every Trellis hash, signature,
 * HMAC, HKDF, and random-bytes operation. WebCrypto is preferred when the
 * operations Trellis needs are usable; otherwise a portable implementation
 * built on the dependencies Trellis already ships is selected. The selected
 * backend is cached, and the wire formats are identical either way.
 *
 * This module is internal to the Trellis runtime and is not part of the public
 * SDK surface.
 */

import { hkdf } from "@noble/hashes/hkdf";
import { hmac } from "@noble/hashes/hmac";
import { sha256 as nobleSha256 } from "@noble/hashes/sha256";
import nacl from "tweetnacl";

/** One Ed25519 signer bound to a Trellis seed. */
export type Ed25519Signer = {
  /** Raw 32-byte public key encoded as unpadded base64url. */
  readonly publicKey: string;
  /** Signs the exact bytes with the Ed25519 private key. */
  sign(data: Uint8Array): Promise<Uint8Array>;
};

/** The semantic cryptographic operations Trellis requires. */
export type TrellisCrypto = {
  /** SHA-256 digest of `data`. */
  sha256(data: Uint8Array): Promise<Uint8Array>;
  /** Derives the Ed25519 signer for a 32-byte seed. */
  signerFromSeed(seed: Uint8Array): Promise<Ed25519Signer>;
  /** Verifies one Ed25519 signature over `data`. */
  verifyEd25519(
    publicKey: Uint8Array,
    data: Uint8Array,
    signature: Uint8Array,
  ): Promise<boolean>;
  /** HKDF-SHA-256 derive-bits. */
  hkdfSha256(
    inputKeyingMaterial: Uint8Array,
    salt: Uint8Array,
    info: Uint8Array,
    length: number,
  ): Promise<Uint8Array>;
  /** HMAC-SHA-256 of `data` under `key`. */
  hmacSha256(key: Uint8Array, data: Uint8Array): Promise<Uint8Array>;
  /** Cryptographically secure random bytes. */
  randomBytes(length: number): Uint8Array;
  /** RFC 4122/9562 version-4 UUID. */
  randomUuid(): string;
};

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

function validateSeed(seed: Uint8Array): Uint8Array {
  if (seed.length !== 32) {
    throw new Error(
      `Invalid Ed25519 seed length: ${seed.length} (expected 32)`,
    );
  }
  return seed;
}

function pkcs8FromSeed(seed: Uint8Array): Uint8Array {
  const pkcs8 = new Uint8Array(ED25519_PKCS8_PREFIX.length + seed.length);
  pkcs8.set(ED25519_PKCS8_PREFIX, 0);
  pkcs8.set(seed, ED25519_PKCS8_PREFIX.length);
  return pkcs8;
}

function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  const buffer = data.buffer;
  if (buffer instanceof ArrayBuffer) {
    return buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
  }
  const copy = new Uint8Array(data.byteLength);
  copy.set(data);
  return copy.buffer;
}

function base64urlEncode(data: Uint8Array): string {
  let binary = "";
  for (const byte of data) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(
    /=+$/g,
    "",
  );
}

/** Cryptographic random bytes; never falls back to `Math.random()`. */
export function randomBytes(length: number): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(length));
}

/** A version-4 UUID derived from cryptographic random bytes. */
export function randomUuid(): string {
  const bytes = randomBytes(16);
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${
    hex.slice(16, 20)
  }-${hex.slice(20)}`;
}

function webCryptoBackend(subtle: SubtleCrypto): TrellisCrypto {
  return {
    sha256: async (data) =>
      new Uint8Array(await subtle.digest("SHA-256", toArrayBuffer(data))),
    signerFromSeed: async (seed) => {
      const normalized = validateSeed(seed);
      const key = await subtle.importKey(
        "pkcs8",
        toArrayBuffer(pkcs8FromSeed(normalized)),
        { name: "Ed25519" },
        false,
        ["sign"],
      );
      const publicKey = base64urlEncode(
        nacl.sign.keyPair.fromSeed(normalized).publicKey,
      );
      return {
        publicKey,
        sign: async (data) =>
          new Uint8Array(
            await subtle.sign({ name: "Ed25519" }, key, toArrayBuffer(data)),
          ),
      };
    },
    verifyEd25519: async (publicKey, data, signature) => {
      try {
        const key = await subtle.importKey(
          "raw",
          toArrayBuffer(publicKey),
          { name: "Ed25519" },
          false,
          ["verify"],
        );
        return await subtle.verify(
          { name: "Ed25519" },
          key,
          toArrayBuffer(signature),
          toArrayBuffer(data),
        );
      } catch {
        return false;
      }
    },
    hkdfSha256: async (inputKeyingMaterial, salt, info, length) => {
      const key = await subtle.importKey(
        "raw",
        toArrayBuffer(inputKeyingMaterial),
        "HKDF",
        false,
        ["deriveBits"],
      );
      return new Uint8Array(
        await subtle.deriveBits(
          {
            name: "HKDF",
            hash: "SHA-256",
            salt: toArrayBuffer(salt),
            info: toArrayBuffer(info),
          },
          key,
          length * 8,
        ),
      );
    },
    hmacSha256: async (key, data) => {
      const hmacKey = await subtle.importKey(
        "raw",
        toArrayBuffer(key),
        { name: "HMAC", hash: "SHA-256" },
        false,
        ["sign"],
      );
      return new Uint8Array(
        await subtle.sign("HMAC", hmacKey, toArrayBuffer(data)),
      );
    },
    randomBytes,
    randomUuid,
  };
}

/**
 * Builds the portable cryptography implementation.
 *
 * Exported for Trellis-internal interoperability tests only; not part of the
 * public SDK surface.
 */
export function portableBackend(): TrellisCrypto {
  return {
    sha256: async (data) => nobleSha256(data),
    signerFromSeed: async (seed) => {
      const keyPair = nacl.sign.keyPair.fromSeed(validateSeed(seed));
      return {
        publicKey: base64urlEncode(keyPair.publicKey),
        sign: async (data) => nacl.sign.detached(data, keyPair.secretKey),
      };
    },
    verifyEd25519: async (publicKey, data, signature) =>
      nacl.sign.detached.verify(data, signature, publicKey),
    hkdfSha256: async (inputKeyingMaterial, salt, info, length) =>
      hkdf(nobleSha256, inputKeyingMaterial, salt, info, length),
    hmacSha256: async (key, data) => hmac(nobleSha256, key, data),
    randomBytes,
    randomUuid,
  };
}

/**
 * Probes whether the WebCrypto operations Trellis relies on are usable here.
 *
 * Presence of `crypto.subtle` is not sufficient: an insecure context can still
 * expose the object while an algorithm is unavailable, so each required
 * operation is exercised once.
 */
async function webCryptoUsable(): Promise<boolean> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return false;
  try {
    const signKey = await subtle.importKey(
      "pkcs8",
      toArrayBuffer(pkcs8FromSeed(new Uint8Array(32))),
      { name: "Ed25519" },
      false,
      ["sign"],
    );
    await subtle.sign(
      { name: "Ed25519" },
      signKey,
      toArrayBuffer(new Uint8Array(1)),
    );

    const hkdfKey = await subtle.importKey(
      "raw",
      toArrayBuffer(new Uint8Array(32)),
      "HKDF",
      false,
      ["deriveBits"],
    );
    await subtle.deriveBits(
      {
        name: "HKDF",
        hash: "SHA-256",
        salt: toArrayBuffer(new Uint8Array(0)),
        info: toArrayBuffer(new Uint8Array(0)),
      },
      hkdfKey,
      256,
    );

    const hmacKey = await subtle.importKey(
      "raw",
      toArrayBuffer(new Uint8Array(32)),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"],
    );
    await subtle.sign("HMAC", hmacKey, toArrayBuffer(new Uint8Array(1)));

    await subtle.digest("SHA-256", toArrayBuffer(new Uint8Array(1)));
    return true;
  } catch {
    return false;
  }
}

let cachedBackend: Promise<TrellisCrypto> | undefined;

/**
 * Resolves the process/document Trellis cryptography implementation.
 *
 * The choice is made once from actual operation usability and cached for the
 * life of the module instance.
 */
export function trellisCrypto(): Promise<TrellisCrypto> {
  cachedBackend ??=
    (async () =>
      await webCryptoUsable()
        ? webCryptoBackend(globalThis.crypto.subtle)
        : portableBackend())();
  return cachedBackend;
}
