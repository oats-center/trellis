import { type Authenticator, nkeyAuthenticator } from "@nats-io/nats-core";
import { fromSeed, Prefix } from "@nats-io/nkeys";

import type { SessionAuthorityOwner } from "./authority.ts";
import { trellisCrypto } from "./crypto.ts";
import { createProof, type RequestSignature } from "./proof.ts";
import {
  signAdmissionGetWasm,
  type AdmissionGet,
  type SessionRequest,
} from "./protocol_wasm.ts";
import {
  type SessionProof,
  type SessionProofInput,
  signSessionProof,
} from "./session_proof.ts";
import { correctedIatSeconds } from "./time.ts";
import { base64urlDecode } from "./utils.ts";

export type NatsConnectOptions = {
  authenticator: Authenticator | Authenticator[];
  inboxPrefix: string;
};

export type TrellisAuth = {
  sessionKey: string; // base64url raw public key
  sessionNkey: string;
  sign: (data: Uint8Array) => Promise<Uint8Array>;
  currentIat: () => number;
  setServerClockOffsetMs: (clockOffsetMs: number) => void;
  /** Current authorization-context digest bound into v1 request and event proofs. */
  contextDigest: () => string;
  /** Common owner of verified session identity and application authorization. */
  authorityOwner?: SessionAuthorityOwner;

  createProof: (
    request: SessionRequest,
    payload: Uint8Array,
  ) => Promise<RequestSignature>;
  signSessionProof: (input: SessionProofInput) => Promise<SessionProof>;
  /** Sign the native pending own-context lookup using this ephemeral key. */
  signAdmissionGet: (
    request: AdmissionGet,
    subject: string,
    reply: string,
  ) => Promise<string>;
  natsConnectOptions: (opts: {
    inboxPrefix: string;
    /** Fresh signed admission for this exact ephemeral key, never a refresh token. */
    admission: string | (() => string);
    authorizationUsable?: () => boolean;
    /**
     * The installation's sentinel user JWT. Presenting it hands the
     * connection to Trellis's auth callout, which decides who connected.
     */
    natsSentinel: string;
  }) => Promise<NatsConnectOptions>;
};

export async function createAuth(opts: {
  sessionKeySeed: string;
  contextDigest?: string | (() => string);
}): Promise<TrellisAuth> {
  const seed = base64urlDecode(opts.sessionKeySeed);
  const signer = await (await trellisCrypto()).signerFromSeed(seed);
  const sessionKey = signer.publicKey;
  const encodedSeed = encodeNatsSeed(Prefix.User, seed);
  const sessionNkey = fromSeed(encodedSeed).getPublicKey();
  let serverClockOffsetMs = 0;
  const resolveContextDigest = (): string => {
    const digest =
      typeof opts.contextDigest === "function"
        ? opts.contextDigest()
        : opts.contextDigest;
    if (digest === undefined) {
      throw new Error("contextDigest is required to sign v1 request proofs");
    }
    return digest;
  };

  const sign = async (data: Uint8Array): Promise<Uint8Array> => {
    return await signer.sign(data);
  };

  const currentIat = (): number =>
    correctedIatSeconds(Date.now(), serverClockOffsetMs);

  return {
    sessionKey,
    sessionNkey,
    sign,
    currentIat,
    setServerClockOffsetMs: (clockOffsetMs) => {
      serverClockOffsetMs = clockOffsetMs;
    },
    contextDigest: resolveContextDigest,
    createProof: (request, payload) => createProof(signer, request, payload),
    signSessionProof: (input) => signSessionProof(input, signer, sessionKey),
    signAdmissionGet: (request, subject, reply) =>
      signAdmissionGetWasm(request, subject, reply, seed),
    natsConnectOptions: (options) => {
      return Promise.resolve({
        authenticator: [
          nkeyAuthenticator(encodedSeed),
          () => ({ jwt: options.natsSentinel }),
          (nonce) => {
            if (options.authorizationUsable?.() === false) {
              throw new Error("authorization context is suspended");
            }
            if (!nonce) throw new Error("NATS server nonce is required");
            const admission =
              typeof options.admission === "function"
                ? options.admission()
                : options.admission;
            if (!admission) {
              throw new Error("signed admission is required for NATS connect");
            }
            return {
              auth_token: admission,
            };
          },
        ],
        inboxPrefix: options.inboxPrefix,
      });
    },
  };
}

/**
 * A raw 32-byte Ed25519 seed as a NATS seed string (`SU…`), the form nkeys
 * reads. nkeys does not export its encoder, so this follows its format: the
 * seed prefix and key type in two bytes, the seed, a little-endian CRC-16
 * (XMODEM) of those, all in unpadded RFC 4648 base32.
 */
function encodeNatsSeed(prefix: number, seed: Uint8Array): Uint8Array {
  const seedPrefix = 18 << 3;
  const raw = new Uint8Array(2 + seed.length + 2);
  raw[0] = seedPrefix | (prefix >> 5);
  raw[1] = (prefix & 31) << 3;
  raw.set(seed, 2);
  let crc = 0;
  for (const byte of raw.subarray(0, raw.length - 2)) {
    crc ^= byte << 8;
    for (let bit = 0; bit < 8; bit++)
      crc = crc & 0x8000 ? ((crc << 1) ^ 0x1021) & 0xffff : (crc << 1) & 0xffff;
  }
  raw[raw.length - 2] = crc & 0xff;
  raw[raw.length - 1] = crc >> 8;
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let text = "";
  let buffer = 0;
  let bits = 0;
  for (const byte of raw) {
    buffer = (buffer << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      text += alphabet[(buffer >> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  if (bits > 0) text += alphabet[(buffer << (5 - bits)) & 31];
  return new TextEncoder().encode(text);
}
