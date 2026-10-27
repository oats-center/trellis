import { setTimeout } from "node:timers/promises";
import { err, ok, type Result } from "@oatscenter/result";
import { z } from "zod";
import { createAuth, type TrellisAuth } from "../auth/session_auth.ts";
import {
  SessionAuthorityOwner,
  type RuntimeAuthorizationTrust,
} from "../auth/authority.ts";
import {
  dpopThumbprint,
  postWithDpop,
  verifyAdmission,
} from "../auth/oauth_core.ts";
import { base64urlEncode } from "../auth/utils.ts";
import type { AppRequestDescriptor } from "../generated_support.ts";
import { AuthError } from "../errors/AuthError.ts";
import { authenticateIssuerChain } from "../auth/issuer_trust.ts";
import {
  MemoryNativeOAuthCustody,
  validateNativeScope,
  validateNativeTrust,
  type NativeOAuthCustody,
  type NativeOAuthCustodyLease,
  type NativeCredentialScope,
} from "./custody.ts";
import { discoverNativeOAuth } from "./discovery.ts";

const tokenSchema = z.object({
  token_type: z.string(),
  refresh_token: z.string().min(1).max(16384),
  trellis_admission: z.string().min(1),
  authorization_details: z.array(z.unknown()),
});
const deviceSchema = z.object({
  device_code: z.string().min(1),
  user_code: z.string().min(1),
  verification_uri: z.string().url(),
  verification_uri_complete: z.string().url().optional(),
  expires_in: z
    .number()
    .int()
    .positive()
    .max(30 * 60),
  interval: z.number().int().positive().max(300).default(5),
});

/** Native OAuth custody and a stable in-memory runtime key, independent of the transport owner. */
export class NativeOAuthClient {
  readonly auth: TrellisAuth;
  readonly runtimeId: string;
  readonly #lease: NativeOAuthCustodyLease;
  #tail: Promise<unknown> = Promise.resolve();
  #closing = false;
  #connected = false;
  #owner?: SessionAuthorityOwner;
  #anchor: RuntimeAuthorizationTrust;

  private constructor(lease: NativeOAuthCustodyLease, auth: TrellisAuth) {
    this.#lease = lease;
    this.#anchor = lease.trust;
    this.auth = auth;
    this.runtimeId = base64urlEncode(
      crypto.getRandomValues(new Uint8Array(16)),
    );
  }

  /** Create process-local credentials, or acquire explicitly supplied application custody. Each opening creates a fresh transport key and runtime nonce. */
  static async open(options: {
    scope: NativeCredentialScope;
    trust: RuntimeAuthorizationTrust;
    custody?: NativeOAuthCustody;
    signal?: AbortSignal;
  }): Promise<Result<NativeOAuthClient, AuthError>> {
    let lease: NativeOAuthCustodyLease | undefined;
    try {
      options.signal?.throwIfAborted();
      validateNativeScope(options.scope);
      lease = await (options.custody ?? new MemoryNativeOAuthCustody()).acquire(
        options.scope,
        options.trust,
        options.signal,
      );
      if (
        Object.entries(options.scope).some(
          ([key, value]) =>
            lease?.scope[key as keyof NativeCredentialScope] !== value,
        )
      )
        throw new AuthError({ reason: "InvalidCredential" });
      await validateNativeTrust(options.scope, lease.trust);
      const jwk = lease.publicJwk;
      if (
        jwk.kty !== "EC" ||
        jwk.crv !== "P-256" ||
        jwk.d !== undefined ||
        !jwk.x ||
        !jwk.y
      )
        throw new AuthError({ reason: "InvalidCredential" });
      await crypto.subtle.importKey(
        "jwk",
        jwk,
        { name: "ECDSA", namedCurve: "P-256" },
        false,
        ["verify"],
      );
      const auth = await createAuth({
        sessionKeySeed: base64urlEncode(
          crypto.getRandomValues(new Uint8Array(32)),
        ),
      });
      options.signal?.throwIfAborted();
      return ok(new NativeOAuthClient(lease, auth));
    } catch (cause) {
      await lease?.close();
      return err(
        cause instanceof AuthError
          ? cause
          : new AuthError({
              reason: options.signal?.aborted
                ? "OAuthCancelled"
                : "StorageUnavailable",
            }),
      );
    }
  }

  /** Restored exact scope, containing no secret. */
  get scope(): NativeCredentialScope {
    return this.#lease.scope;
  }
  /** Restored or owner-authenticated trust, never token-response discovery. */
  get trust(): RuntimeAuthorizationTrust {
    return this.#lease.trust;
  }

  /** Begin RFC 8628 using the generated native application's finite RAR selections. */
  startDevice(
    request: AppRequestDescriptor,
    options: { signal?: AbortSignal } = {},
  ): Promise<Result<NativeDeviceAuthorization, AuthError>> {
    return this.#serial(async () => {
      if (request.kind !== "native")
        throw new AuthError({ reason: "InvalidCredential" });
      const details = JSON.stringify([
        {
          type: "trellis_capabilities",
          required: request.requiredCapabilities,
          optional: request.optionalCapabilities,
        },
      ]);
      if (Buffer.byteLength(details) > 32768)
        throw new AuthError({ reason: "InvalidCredential" });
      const data = deviceSchema.parse(
        await this.#exchange(
          "/device_authorization",
          { authorization_details: details },
          options.signal,
        ),
      );
      // Both links must lead to the selected issuer; the complete link must
      // carry exactly the code the user compares on the page.
      const onIssuer = (value: string) => {
        const uri = new URL(value);
        return uri.origin === this.scope.issuer &&
          !uri.username &&
          !uri.password
          ? uri
          : undefined;
      };
      const complete =
        data.verification_uri_complete === undefined
          ? undefined
          : onIssuer(data.verification_uri_complete);
      if (
        !onIssuer(data.verification_uri) ||
        (data.verification_uri_complete !== undefined &&
          complete?.searchParams.get("user_code") !== data.user_code)
      )
        throw new AuthError({ reason: "InvalidOAuthResponse" });
      return new NativeDeviceAuthorization(
        this,
        data.device_code,
        data.user_code,
        data.verification_uri,
        data.verification_uri_complete,
        data.interval,
        performance.now() + data.expires_in * 1000,
      );
    });
  }

  /** Rotate a stored family. Ambiguous exchange or cancellation preserves the durable spent marker. */
  resume(
    options: { signal?: AbortSignal } = {},
  ): Promise<Result<NativeOAuthSession, AuthError>> {
    return this.#serial(async () => {
      options.signal?.throwIfAborted();
      if (!this.#lease.refreshAvailable)
        throw new AuthError({ reason: "OAuthRecoveryRequired" });
      await this.#prepareTrust(options.signal);
      const token = await this.#lease.beginRotation();
      try {
        return await this.#install(
          await this.#exchange(
            "/token",
            { grant_type: "refresh_token", refresh_token: token },
            options.signal,
          ),
          options.signal,
          true,
        );
      } catch (cause) {
        if (
          cause instanceof AuthError &&
          cause.reason === "OAuthTrustUnavailable"
        )
          throw cause;
        throw new AuthError({ reason: "OAuthRecoveryRequired" });
      }
    });
  }

  /** Refresh under owner-authenticated successor trust, retaining this runtime/key and exclusive custody. */
  refresh(
    options: { signal?: AbortSignal } = {},
  ): Promise<Result<NativeOAuthSession, AuthError>> {
    return this.#serial(async () => {
      options.signal?.throwIfAborted();
      if (!this.#lease.refreshAvailable)
        throw new AuthError({ reason: "OAuthRecoveryRequired" });
      if (this.#owner) await this.#retainTrust(this.#owner);
      // Authenticate the selected origin before spending the one-use secret.
      // An unavailable origin leaves custody intact; an ambiguous token POST
      // still retains the strict spent marker and requires explicit recovery.
      await this.#prepareTrust(options.signal);
      const token = await this.#lease.beginRotation();
      try {
        return await this.#install(
          await this.#exchange(
            "/token",
            { grant_type: "refresh_token", refresh_token: token },
            options.signal,
          ),
          options.signal,
          true,
        );
      } catch (cause) {
        if (
          cause instanceof AuthError &&
          cause.reason === "OAuthTrustUnavailable"
        )
          throw cause;
        throw new AuthError({ reason: "OAuthRecoveryRequired" });
      }
    });
  }

  /** Poll one standard Device Code exchange; only pending/slow_down permit subsequent polls. @internal */
  pollDevice(
    code: string,
    signal?: AbortSignal,
  ): Promise<Result<NativeOAuthSession, AuthError>> {
    return this.#serial(
      async () =>
        await this.#install(
          await this.#exchange(
            "/token",
            {
              grant_type: "urn:ietf:params:oauth:grant-type:device_code",
              device_code: code,
            },
            signal,
          ),
          signal,
        ),
    );
  }

  /** Transfer this family's one connection claim to the common transport owner. @internal */
  claimConnection(): {
    bindOwner(owner: SessionAuthorityOwner): void;
    close(): Promise<void>;
  } {
    if (this.#connected || this.#closing)
      throw new AuthError({ reason: "CredentialAlreadyConnected" });
    this.#connected = true;
    return {
      bindOwner: (owner) => {
        if (this.#owner)
          throw new AuthError({ reason: "CredentialAlreadyConnected" });
        SessionAuthorityOwner.prototype.trustForCredential.call(
          owner,
          this.#anchor,
        );
        this.#owner = owner;
      },
      close: async () => {
        this.#closing = true;
        await this.#tail;
        await this.#lease.close();
      },
    };
  }

  /** Before connection transfer, settle exchanges and release custody. After transfer this reference cannot close the transport-owned lease. */
  async close(): Promise<void> {
    if (this.#connected) return;
    this.#closing = true;
    await this.#tail;
    await this.#lease.close();
  }

  #serial<T>(operation: () => Promise<T>): Promise<Result<T, AuthError>> {
    if (this.#closing)
      return Promise.resolve(
        err(new AuthError({ reason: "OAuthRecoveryRequired" })),
      );
    const result = this.#tail.then(async () => {
      try {
        return ok(await operation());
      } catch (cause) {
        return err(
          cause instanceof AuthError
            ? cause
            : new AuthError({ reason: "OAuthUnavailable" }),
        );
      }
    });
    this.#tail = result;
    return result;
  }

  async #exchange(
    endpoint: string,
    fields: Record<string, string>,
    signal?: AbortSignal,
  ): Promise<unknown> {
    const form = new URLSearchParams({
      ...fields,
      client_id: this.scope.clientId,
      trellis_runtime_id: this.runtimeId,
      trellis_session_nkey: this.auth.sessionNkey,
    });
    const lease = this.#lease;
    const response = await postWithDpop({
      endpoint: `${this.scope.issuer}${endpoint}`,
      form,
      key: {
        publicJwk: lease.publicJwk,
        sign: async (bytes) => await lease.sign(bytes),
      },
      runtimeId: this.runtimeId,
      sessionPublicKey: this.auth.sessionKey,
      signal: AbortSignal.any([
        AbortSignal.timeout(30000),
        ...(signal ? [signal] : []),
      ]),
    });
    if (response.ok) return response.data;
    const failure = z.object({ error: z.string() }).parse(response.data);
    throw new AuthError({
      reason:
        failure.error === "access_denied"
          ? "OAuthAuthorizationDenied"
          : failure.error === "expired_token"
            ? "OAuthDeviceExpired"
            : failure.error === "authorization_pending" ||
                failure.error === "slow_down"
              ? failure.error
              : "OAuthRecoveryRequired",
    });
  }

  async #retainTrust(owner: SessionAuthorityOwner): Promise<void> {
    const previous = this.trust;
    const trust = SessionAuthorityOwner.prototype.trustForCredential.call(
      owner,
      previous,
    );
    await validateNativeTrust(this.scope, trust, previous);
    await this.#lease.retainTrust(trust);
  }

  async #prepareTrust(signal?: AbortSignal): Promise<void> {
    const discovered = await discoverNativeOAuth(this.scope.issuer, { signal });
    if (discovered.isErr())
      throw new AuthError({ reason: "OAuthTrustUnavailable" });
    const owner = await authenticateIssuerChain(
      this.scope.issuer,
      this.trust,
      discovered.orThrow().trust,
      signal,
      this.#owner,
    );
    try {
      await this.#retainTrust(owner);
      if (!this.#connected) this.#anchor = this.trust;
    } finally {
      if (owner !== this.#owner) owner.stop();
    }
  }

  async #install(
    data: unknown,
    signal?: AbortSignal,
    rotating = false,
  ): Promise<NativeOAuthSession> {
    const response = tokenSchema.parse(data);
    if (rotating && response.token_type.toLowerCase() === "dpop")
      await this.#lease.retainRotatedToken(response.refresh_token);
    // If the old token was spent but this freshly returned successor fails to
    // verify, it is retained without promoting unverified admission authority.
    const verified = await verifyAdmission({
      admission: response.trellis_admission,
      ephemeralNkey: this.auth.sessionNkey,
      nowUnixSeconds: () => Math.floor(Date.now() / 1000),
      trust: () => this.trust,
      refreshTrust: async () => await this.#prepareTrust(signal),
    });
    if (!verified.ok) throw new AuthError({ reason: "InvalidOAuthResponse" });
    const body = verified.admission.credential;
    if (
      this.#lease.principalId !== undefined &&
      this.#lease.principalId !== body.principalId
    )
      throw new AuthError({ reason: "InvalidCredential" });
    const jkt = await dpopThumbprint(this.#lease.publicJwk);
    if (
      response.token_type.toLowerCase() !== "dpop" ||
      body.clientId !== this.scope.clientId ||
      body.runtimeId !== this.runtimeId ||
      body.persistentDpopJkt !== jkt ||
      body.binding.bindingKind !== "native" ||
      body.binding.nativeDpopJkt !== jkt
    )
      throw new AuthError({ reason: "InvalidOAuthResponse" });
    signal?.throwIfAborted();
    await this.#lease.install({
      principalId: body.principalId,
      oauthGrantId: body.oauthGrantId,
      loginSessionId: body.loginSessionId,
      refreshToken: response.refresh_token,
    });
    signal?.throwIfAborted();
    return new NativeOAuthSession(
      this,
      response.trellis_admission,
      response.authorization_details,
      body.loginSessionId,
      body.oauthGrantId,
      body.principalId,
      body.persistentDpopJkt,
    );
  }
}

/** User-facing Device Code metadata; device credential and private key remain inaccessible. */
export class NativeDeviceAuthorization {
  readonly userCode: string;
  readonly verificationUri: string;
  /**
   * The issuer's portal URI with the code already filled in, when offered.
   * Show it as a link or QR code; the page still displays the code to compare.
   */
  readonly verificationUriComplete: string | undefined;
  readonly #client: NativeOAuthClient;
  readonly #code: string;
  readonly #deadline: number;
  #interval: number;
  #completed = false;
  /** Created by the ordinary OAuth client. @internal */
  constructor(
    client: NativeOAuthClient,
    code: string,
    userCode: string,
    verificationUri: string,
    verificationUriComplete: string | undefined,
    interval: number,
    deadline: number,
  ) {
    this.#client = client;
    this.#code = code;
    this.userCode = userCode;
    this.verificationUri = verificationUri;
    this.verificationUriComplete = verificationUriComplete;
    this.#interval = interval;
    this.#deadline = deadline;
  }
  /** Poll at the issuer's interval, honor slow_down and the original deadline; cancellation does not erase custody. */
  async complete(
    options: { signal?: AbortSignal } = {},
  ): Promise<Result<NativeOAuthSession, AuthError>> {
    if (this.#completed)
      return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
    this.#completed = true;
    try {
      // Poll once at once: a code approved before polling starts needs no
      // wait. The interval separates later polls.
      let first = true;
      while (performance.now() < this.#deadline) {
        if (!first)
          await setTimeout(
            Math.min(this.#interval * 1000, this.#deadline - performance.now()),
            undefined,
            { signal: options.signal },
          );
        first = false;
        const remaining = this.#deadline - performance.now();
        if (remaining <= 0) break;
        const result = await this.#client.pollDevice(
          this.#code,
          AbortSignal.any([
            AbortSignal.timeout(Math.ceil(remaining)),
            ...(options.signal ? [options.signal] : []),
          ]),
        );
        if (result.isOk()) return result;
        if (result.error.reason === "slow_down") this.#interval += 5;
        else if (result.error.reason !== "authorization_pending") return result;
      }
      return err(new AuthError({ reason: "OAuthDeviceExpired" }));
    } catch {
      return err(
        new AuthError({
          reason: options.signal?.aborted
            ? "OAuthCancelled"
            : "OAuthDeviceExpired",
        }),
      );
    }
  }
}

/** Verified OAuth session owns custody and the ephemeral runtime key, not a transport or second authorization cache. */
export class NativeOAuthSession {
  readonly #client: NativeOAuthClient;
  readonly #admission: string;
  readonly authorizationDetails: readonly unknown[];
  readonly loginSessionId: string;
  readonly oauthGrantId: string;
  readonly principalId: string;
  /** RFC 7638 thumbprint of this credential's durable P-256 key, certified by the issuer. */
  readonly durableDpopJkt: string;
  /** Created only after shared protocol verification. @internal */
  constructor(
    client: NativeOAuthClient,
    admission: string,
    details: readonly unknown[],
    loginSessionId: string,
    oauthGrantId: string,
    principalId: string,
    durableDpopJkt: string,
  ) {
    this.#client = client;
    this.#admission = admission;
    this.authorizationDetails = details;
    this.loginSessionId = loginSessionId;
    this.oauthGrantId = oauthGrantId;
    this.principalId = principalId;
    this.durableDpopJkt = durableDpopJkt;
  }
  /** Issuer-verified short-lived admission, kept only in memory and omitted from ordinary object inspection. */
  get admission(): string {
    return this.#admission;
  }

  /** Stable runtime signer retained across refresh. */
  get auth(): TrellisAuth {
    return this.#client.auth;
  }
  /** Canonical base64url 128-bit runtime nonce retained across refresh. */
  get runtimeId(): string {
    return this.#client.runtimeId;
  }
  /** Exact non-secret credential identity. */
  get scope(): NativeCredentialScope {
    return this.#client.scope;
  }
  /** Current retained trust floor. */
  get trust(): RuntimeAuthorizationTrust {
    return this.#client.trust;
  }
  /** Rotate using the bound common owner's verified successor chain, never caller-supplied issuer metadata. */
  refresh(
    options: { signal?: AbortSignal } = {},
  ): Promise<Result<NativeOAuthSession, AuthError>> {
    return this.#client.refresh(options);
  }
  /** Transfer this family's one connection claim to the common transport owner. @internal */
  claimConnection(): ReturnType<NativeOAuthClient["claimConnection"]> {
    return this.#client.claimConnection();
  }

  /** Release untransferred custody. Once connected, only closing the returned transport releases its lease; this old reference is inert. */
  close(): Promise<void> {
    return this.#client.close();
  }
}
