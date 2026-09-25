import { type Authenticator, jwtAuthenticator } from "@nats-io/nats-core";
import { fromSeed, Prefix } from "@nats-io/nkeys";
import { Codec } from "@nats-io/nkeys/lib/codec.js";
import { ulid } from "ulid";

import type { AuthorizationProviderCache } from "./authorization_context.ts";
import { trellisCrypto } from "./crypto.ts";
import { createProof } from "./proof.ts";
import {
  type SessionProof,
  type SessionProofInput,
  signSessionProof,
} from "./session_proof.ts";
import { correctedIatSeconds } from "./time.ts";
import {
  base64urlDecode,
  base64urlEncode,
  canonicalizeJsonValue,
  sha256,
  utf8,
} from "./utils.ts";

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
  /** Local verifier for received requests and events. */
  authorizationProviderCache?: AuthorizationProviderCache;

  createProof: (
    subject: string,
    payloadHash: Uint8Array,
    reply: string,
    requestId?: string,
    iat?: number,
  ) => Promise<string>;
  signSessionProof: (input: SessionProofInput) => Promise<SessionProof>;
  natsConnectOptions: (
    opts: {
      sessionId: string;
      contextDigest: string | (() => string);
      jwt: string | (() => string);
      authorizationUsable?: () => boolean;
    },
  ) => Promise<NatsConnectOptions>;
};

export async function createAuth(
  opts: { sessionKeySeed: string; contextDigest?: string | (() => string) },
): Promise<TrellisAuth> {
  const seed = base64urlDecode(opts.sessionKeySeed);
  const signer = await (await trellisCrypto()).signerFromSeed(seed);
  const sessionKey = signer.publicKey;
  const encodedSeed = Codec.encodeSeed(Prefix.User, seed);
  const sessionNkey = fromSeed(encodedSeed).getPublicKey();
  let serverClockOffsetMs = 0;
  const resolveContextDigest = (): string => {
    const digest = typeof opts.contextDigest === "function"
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
    createProof: (subject, payloadHash, reply, requestId, iat) =>
      createProof(signer, {
        contextDigest: resolveContextDigest(),
        subject,
        reply,
        payloadHash,
        iat: iat ?? currentIat(),
        requestId: requestId ?? ulid(),
      }),
    signSessionProof: (input) => signSessionProof(input, signer, sessionKey),
    natsConnectOptions: (options) => {
      return Promise.resolve({
        authenticator: [
          jwtAuthenticator(options.jwt, encodedSeed),
          (nonce) => {
            if (options.authorizationUsable?.() === false) {
              throw new Error("authorization context is suspended");
            }
            if (!nonce) throw new Error("NATS server nonce is required");
            const contextDigest = typeof options.contextDigest === "function"
              ? options.contextDigest()
              : options.contextDigest;
            if (!contextDigest) {
              throw new Error("contextDigest is required for NATS connect");
            }
            return {
              auth_token: JSON.stringify({
                format: "trellis.nats-connect-token.v1",
                contextDigest,
              }),
            };
          },
        ],
        inboxPrefix: `_INBOX.${options.sessionId}`,
      });
    },
  };
}
