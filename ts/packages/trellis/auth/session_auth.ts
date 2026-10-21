import { type Authenticator, nkeyAuthenticator } from "@nats-io/nats-core";
import { fromSeed, Prefix } from "@nats-io/nkeys";
import { Codec } from "@nats-io/nkeys/lib/codec.js";

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
  }) => Promise<NatsConnectOptions>;
};

export async function createAuth(opts: {
  sessionKeySeed: string;
  contextDigest?: string | (() => string);
}): Promise<TrellisAuth> {
  const seed = base64urlDecode(opts.sessionKeySeed);
  const signer = await (await trellisCrypto()).signerFromSeed(seed);
  const sessionKey = signer.publicKey;
  const encodedSeed = Codec.encodeSeed(Prefix.User, seed);
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
