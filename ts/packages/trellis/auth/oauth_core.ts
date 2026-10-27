import { AuthError } from "../errors/AuthError.ts";
import type { RuntimeAuthorizationTrust } from "./authority.ts";
import {
  verifyOAuthAdmissionWasm,
  type SignedOAuthAdmission,
} from "./protocol_wasm.ts";
import { base64urlEncode, sha256, toArrayBuffer, utf8 } from "./utils.ts";

/**
 * The OAuth steps the browser and native clients share: response bounds, DPoP
 * proofs, the token request with its nonce challenge, and admission
 * verification. Each client keeps its own custody and error mapping.
 */

/** Read at most 64 KiB of an OAuth response as JSON. */
export async function boundedJson(response: Response): Promise<unknown> {
  if (!response.body) throw new AuthError({ reason: "InvalidOAuthResponse" });
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const next = await reader.read();
      if (next.done) break;
      length += next.value.byteLength;
      if (length > 65536)
        throw new AuthError({ reason: "InvalidOAuthResponse" });
      chunks.push(next.value);
    }
  } finally {
    await reader.cancel();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
}

/** A P-256 DPoP key: its public JWK and an ES256 signer over raw bytes. */
export type DpopKey = Readonly<{
  publicJwk: JsonWebKey;
  sign(bytes: Uint8Array<ArrayBuffer>): Promise<ArrayBuffer | Uint8Array>;
}>;

/** RFC 7638 thumbprint of a P-256 public JWK. */
export async function dpopThumbprint(jwk: JsonWebKey): Promise<string> {
  const { crv, kty, x, y } = jwk;
  return base64urlEncode(
    await sha256(utf8(JSON.stringify({ crv, kty, x, y }))),
  );
}

/** RFC 9449 proof carrying the Trellis runtime ID and session NKey thumbprint. */
export async function createDpopProof(
  key: DpopKey,
  endpoint: string,
  runtimeId: string,
  sessionPublicKey: string,
  nonce?: string,
): Promise<string> {
  const htu = new URL(endpoint);
  htu.search = "";
  htu.hash = "";
  const header = base64urlEncode(
    utf8(JSON.stringify({ typ: "dpop+jwt", alg: "ES256", jwk: key.publicJwk })),
  );
  const sessionThumbprint = base64urlEncode(
    await sha256(
      utf8(JSON.stringify({ crv: "Ed25519", kty: "OKP", x: sessionPublicKey })),
    ),
  );
  const claims = base64urlEncode(
    utf8(
      JSON.stringify({
        jti: crypto.randomUUID(),
        htm: "POST",
        htu: htu.href,
        iat: Math.floor(Date.now() / 1000),
        trellis_runtime_id: runtimeId,
        trellis_nats_jkt: sessionThumbprint,
        ...(nonce === undefined ? {} : { nonce }),
      }),
    ),
  );
  const input = `${header}.${claims}`;
  const signature = new Uint8Array(
    await key.sign(new Uint8Array(toArrayBuffer(utf8(input)))),
  );
  return `${input}.${base64urlEncode(signature)}`;
}

/** A token-endpoint response after any DPoP nonce challenge was answered. */
export type TokenResponse = Readonly<{
  status: number;
  ok: boolean;
  data: unknown;
  /** The latest server nonce, for the next request to the same endpoint. */
  nonce?: string;
}>;

/** One bounded `DPoP-Nonce` value, or undefined. @internal */
export function usableNonce(value: string | null): string | undefined {
  return value && value.length <= 1024 && !value.includes(",")
    ? value
    : undefined;
}

/**
 * POST a DPoP-bound form, answering a `use_dpop_nonce` challenge. Each attempt
 * signs a fresh proof; a challenge precedes any artifact consumption, so the
 * retry never replays a spent credential.
 */
export async function postWithDpop(options: {
  endpoint: string;
  form: URLSearchParams;
  key: DpopKey;
  runtimeId: string;
  sessionPublicKey: string;
  nonce?: string;
  signal: AbortSignal;
}): Promise<TokenResponse> {
  let nonce = options.nonce;
  for (let attempt = 0; attempt < 3; attempt++) {
    const proof = await createDpopProof(
      options.key,
      options.endpoint,
      options.runtimeId,
      options.sessionPublicKey,
      nonce,
    );
    const response = await fetch(options.endpoint, {
      method: "POST",
      body: options.form,
      headers: { DPoP: proof },
      credentials: "omit",
      redirect: "error",
      signal: options.signal,
    });
    // A rate limiter, often a proxy, refuses before reading the request and
    // need not send a JSON body.
    if (response.status === 429)
      return { status: 429, ok: false, data: undefined };
    const data = await boundedJson(response);
    const challenge = usableNonce(response.headers.get("DPoP-Nonce"));
    const asksForNonce =
      response.status === 400 &&
      typeof data === "object" &&
      data !== null &&
      (data as { error?: unknown }).error === "use_dpop_nonce";
    if (asksForNonce && challenge && challenge !== nonce) {
      nonce = challenge;
      continue;
    }
    return { status: response.status, ok: response.ok, data, nonce: challenge };
  }
  throw new AuthError({ reason: "OAuthRecoveryRequired" });
}

/**
 * Verify a signed OAuth admission against current trust. If it does not
 * verify, refresh issuer trust once (the issuer may have rotated while the
 * request was in flight) and verify again.
 */
export async function verifyAdmission(options: {
  admission: string;
  ephemeralNkey: string;
  nowUnixSeconds: () => number;
  trust: () => RuntimeAuthorizationTrust;
  refreshTrust: () => Promise<void>;
}): Promise<
  | {
      ok: true;
      admission: SignedOAuthAdmission;
      trust: RuntimeAuthorizationTrust;
    }
  | { ok: false }
> {
  const verify = async (trust: RuntimeAuthorizationTrust) =>
    await verifyOAuthAdmissionWasm(utf8(options.admission), {
      issuer: trust.issuer,
      trellisInstanceId: trust.instanceId,
      audienceNatsAccount: trust.account,
      ephemeralNkey: options.ephemeralNkey,
      nowUnixSeconds: options.nowUnixSeconds(),
      maximumLifetimeSeconds: 300,
    });
  let trust = options.trust();
  let verified = await verify(trust);
  if (!verified.ok) {
    await options.refreshTrust();
    trust = options.trust();
    verified = await verify(trust);
  }
  return verified.ok
    ? { ok: true, admission: verified.admission, trust }
    : { ok: false };
}
