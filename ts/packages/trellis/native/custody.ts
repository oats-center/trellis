import { isIP } from "node:net";
import type { webcrypto } from "node:crypto";
import { fromPublic } from "@nats-io/nkeys";
import { z } from "zod";
import type { RuntimeAuthorizationTrust } from "../auth/authority.ts";
import { base64urlDecode, base64urlEncode, sha256 } from "../auth/utils.ts";
import { codecs } from "../generated_support.ts";
import { AuthError } from "../errors/AuthError.ts";

/** Exact native credential identity, independent of the application's request descriptor. */
export type NativeCredentialScope = Readonly<{
  issuer: string;
  instanceId: string;
  account: string;
  clientId: string;
  credentialHandle: string;
}>;

/** Identity and rotating secret from a verified native OAuth response. Never log this value. */
export type NativeOAuthGrant = Readonly<{
  principalId: string;
  oauthGrantId: string;
  loginSessionId: string;
  refreshToken: string;
}>;

/**
 * Application-owned, exclusively acquired credential custody. The SDK consumes
 * each acquired lease, including release when opening or admission fails.
 *
 * Persistent implementations must hold exclusive ownership across read, exchange,
 * and commit; flush the spent marker before beginRotation resolves; and atomically
 * retain successor tokens and authenticated trust before their methods resolve.
 * Reject on storage failure: the SDK never retries an ambiguous refresh exchange.
 * Getters expose the current acquired snapshot, not uncoordinated storage reads.
 * Private signing material must never be returned by this interface.
 */
export interface NativeOAuthCustodyLease {
  /** Exact credential identity held by this lease. */
  readonly scope: NativeCredentialScope;
  /** Retained authenticated issuer pin and rotation floor. */
  readonly trust: RuntimeAuthorizationTrust;
  /** Public RFC 7517 P-256 signing key; never include private key material. */
  readonly publicJwk: Readonly<webcrypto.JsonWebKey>;
  /** Whether a known, unsubmitted refresh credential is available. */
  readonly refreshAvailable: boolean;
  /** Principal bound by a previous verified response, or undefined before first authorization. */
  readonly principalId: string | undefined;
  /** Sign with the bound private key; return the 64-byte ES256 P1363 signature (r || s), not DER. */
  sign(input: Uint8Array<ArrayBuffer>): Promise<ArrayBuffer>;
  /** Mark the current token spent before returning it to the HTTP exchange. */
  beginRotation(): string | Promise<string>;
  /** Clear the spent marker after the issuer refused the rotation before reading it, keeping the same unsubmitted token. */
  abandonRotation(): void | Promise<void>;
  /** Retain a returned, unsubmitted successor without promoting admission. */
  retainRotatedToken(refreshToken: string): void | Promise<void>;
  /** Atomically install a verified response without changing its owning principal. */
  install(grant: NativeOAuthGrant): void | Promise<void>;
  /** Retain only the SDK-authenticated same-installation issuer trust. */
  retainTrust(trust: RuntimeAuthorizationTrust): void | Promise<void>;
  /** Release exclusive ownership after all exchanges have settled. Must be idempotent. */
  close(): Promise<void>;
}

/** Application-owned acquisition of one credential family's exclusive custody. */
export interface NativeOAuthCustody {
  /** Restore or create this exact family's lease; retain existing trust rather than repinning from discovery. */
  acquire(
    scope: NativeCredentialScope,
    initialTrust: RuntimeAuthorizationTrust,
    signal?: AbortSignal,
  ): Promise<NativeOAuthCustodyLease>;
}

const grantSchema = z.object({
  principalId: z.string().min(1).max(4096),
  oauthGrantId: z.string().min(1).max(4096),
  loginSessionId: z.string().min(1).max(4096),
  refreshToken: z.string().min(1).max(16384),
});

/** Validate a selected credential identity before creating or accepting custody. @internal */
export function validateNativeScope(scope: NativeCredentialScope): void {
  if (
    Object.values(scope).some(
      (value) => typeof value !== "string" || !value || value.length > 4096,
    )
  )
    throw new AuthError({ reason: "InvalidCredential" });
  const origin = new URL(scope.issuer);
  const host = origin.hostname.replace(/^\[|\]$/g, "");
  const loopback =
    host === "localhost" ||
    host === "::1" ||
    (isIP(host) === 4 && host.startsWith("127."));
  // The issuer is an origin, or in development mode an origin plus the path
  // sign-in is served under, in exactly one spelling.
  if (
    `${origin.origin}${origin.pathname.replace(/\/+$/, "")}` !== scope.issuer ||
    (origin.protocol !== "https:" && !(origin.protocol === "http:" && loopback))
  )
    throw new AuthError({ reason: "InvalidCredential" });
  codecs.ulid.decode(scope.instanceId);
  codecs.ulid.decode(scope.credentialHandle);
  if (
    !scope.account.startsWith("A") ||
    fromPublic(scope.account).getPublicKey() !== scope.account
  )
    throw new AuthError({ reason: "InvalidCredential" });
}

/** Validate retained trust and monotonic authenticated replacement. @internal */
export async function validateNativeTrust(
  scope: NativeCredentialScope,
  trust: RuntimeAuthorizationTrust,
  previous?: RuntimeAuthorizationTrust,
): Promise<void> {
  const bytes = base64urlDecode(trust.issuer.publicKey);
  if (
    trust.instanceId !== scope.instanceId ||
    trust.account !== scope.account ||
    trust.issuer.state !== "active" ||
    !/^(0|[1-9][0-9]*)$/.test(trust.rotationSequence) ||
    bytes.length !== 32 ||
    base64urlEncode(bytes) !== trust.issuer.publicKey ||
    base64urlEncode(await sha256(bytes)) !== trust.issuer.keyId ||
    (previous &&
      (BigInt(trust.rotationSequence) < BigInt(previous.rotationSequence) ||
        (trust.rotationSequence === previous.rotationSequence &&
          (trust.issuer.keyId !== previous.issuer.keyId ||
            trust.issuer.publicKey !== previous.issuer.publicKey))))
  )
    throw new AuthError({ reason: "InvalidCredential" });
  await crypto.subtle.importKey(
    "raw",
    new Uint8Array(bytes),
    { name: "Ed25519" },
    false,
    ["verify"],
  );
}

type MemoryCredential = {
  scope: NativeCredentialScope;
  key: CryptoKey;
  publicJwk: webcrypto.JsonWebKey;
  trust: RuntimeAuthorizationTrust;
  grant?: NativeOAuthGrant;
  spent: boolean;
  leased: boolean;
};

/**
 * One process-local credential family. Retaining this object allows fresh client
 * connections to resume the family after the previous connection closes. Nothing
 * is written to disk or shared across processes. Concurrent acquisition is denied.
 */
export class MemoryNativeOAuthCustody implements NativeOAuthCustody {
  #credential?: Promise<MemoryCredential>;

  /** Acquire a new lease, preserving this object's key, trust, and unspent successor. */
  async acquire(
    scope: NativeCredentialScope,
    trust: RuntimeAuthorizationTrust,
    signal?: AbortSignal,
  ): Promise<NativeOAuthCustodyLease> {
    signal?.throwIfAborted();
    validateNativeScope(scope);
    this.#credential ??= (async () => {
      await validateNativeTrust(scope, trust);
      const pair = await crypto.subtle.generateKey(
        { name: "ECDSA", namedCurve: "P-256" },
        false,
        ["sign", "verify"],
      );
      return {
        scope: Object.freeze({ ...scope }),
        trust: structuredClone(trust),
        key: pair.privateKey,
        publicJwk: await crypto.subtle.exportKey("jwk", pair.publicKey),
        spent: false,
        leased: false,
      };
    })();
    const credential = await this.#credential;
    signal?.throwIfAborted();
    if (
      Object.entries(scope).some(
        ([key, value]) =>
          credential.scope[key as keyof NativeCredentialScope] !== value,
      )
    )
      throw new AuthError({ reason: "InvalidCredential" });
    if (credential.leased)
      throw new AuthError({ reason: "CredentialAlreadyConnected" });
    credential.leased = true;
    return new MemoryNativeOAuthLease(credential);
  }
}

/** An exclusive view of one process-local family. @internal */
class MemoryNativeOAuthLease implements NativeOAuthCustodyLease {
  readonly scope: NativeCredentialScope;
  readonly #record: MemoryCredential;
  #closed = false;

  constructor(record: MemoryCredential) {
    this.scope = record.scope;
    this.#record = record;
  }

  /** Current process-local trust, returned without mutable aliases. */
  get trust(): RuntimeAuthorizationTrust {
    this.#assertOpen();
    return structuredClone(this.#record.trust);
  }
  /** Public DPoP key only. */
  get publicJwk(): Readonly<webcrypto.JsonWebKey> {
    this.#assertOpen();
    return { ...this.#record.publicJwk };
  }
  /** Whether this process can submit its current refresh token once. */
  get refreshAvailable(): boolean {
    this.#assertOpen();
    return this.#record.grant !== undefined && !this.#record.spent;
  }
  /** Preserve the account binding independently of adapter commit behavior. */
  get principalId(): string | undefined {
    this.#assertOpen();
    return this.#record.grant?.principalId;
  }
  /** Sign without exporting the private key. */
  async sign(input: Uint8Array<ArrayBuffer>): Promise<ArrayBuffer> {
    this.#assertOpen();
    return await crypto.subtle.sign(
      { name: "ECDSA", hash: "SHA-256" },
      this.#record.key,
      input,
    );
  }
  /** Fence replay before the caller can submit the token. */
  beginRotation(): string {
    this.#assertOpen();
    if (!this.#record.grant || this.#record.spent)
      throw new AuthError({ reason: "OAuthRecoveryRequired" });
    this.#record.spent = true;
    return this.#record.grant.refreshToken;
  }
  /** Restore the token a refused rotation never submitted. */
  abandonRotation(): void {
    this.#assertOpen();
    if (!this.#record.grant || !this.#record.spent)
      throw new AuthError({ reason: "OAuthRecoveryRequired" });
    this.#record.spent = false;
  }
  /** Retain an unsubmitted successor even when admission verification has not completed. */
  retainRotatedToken(refreshToken: string): void {
    this.#assertOpen();
    if (!this.#record.grant) return;
    if (!this.#record.spent || this.#record.grant.refreshToken === refreshToken)
      throw new AuthError({ reason: "OAuthRecoveryRequired" });
    this.#record.grant = grantSchema.parse({
      ...this.#record.grant,
      refreshToken,
    });
    this.#record.spent = false;
  }
  /** Install verified identity without letting reauthorization switch accounts. */
  install(grant: NativeOAuthGrant): void {
    this.#assertOpen();
    const selected = grantSchema.parse(grant);
    if (
      this.#record.grant &&
      this.#record.grant.principalId !== selected.principalId
    )
      throw new AuthError({ reason: "InvalidCredential" });
    this.#record.grant = selected;
    this.#record.spent = false;
  }
  /** Retain a same-installation authenticated successor without lowering its trust floor. */
  async retainTrust(trust: RuntimeAuthorizationTrust): Promise<void> {
    this.#assertOpen();
    await validateNativeTrust(this.scope, trust, this.#record.trust);
    this.#assertOpen();
    this.#record.trust = structuredClone(trust);
  }
  /** Release the lease; only the application-retained custody object can reopen it. */
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#record.leased = false;
  }
  #assertOpen(): void {
    if (this.#closed) throw new AuthError({ reason: "OAuthRecoveryRequired" });
  }
}
