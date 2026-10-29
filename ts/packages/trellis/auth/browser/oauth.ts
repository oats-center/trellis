import { err, isErr, ok, type Result } from "@oatscenter/result";
import { z } from "zod";
import { AuthError } from "../../errors/AuthError.ts";
import { authenticateIssuerChain } from "../issuer_trust.ts";
import { createAuth, type TrellisAuth } from "../session_auth.ts";
import { base64urlEncode, sha256, utf8, toArrayBuffer } from "../utils.ts";
import { BrowserSessionStore, type BrowserOAuthScope } from "./storage.ts";
import { type SignedOAuthAdmission } from "../protocol_wasm.ts";
import {
  boundedJson,
  dpopThumbprint,
  postWithDpop,
  verifyAdmission,
} from "../oauth_core.ts";
import {
  SessionAuthorityOwner,
  type RuntimeAuthorizationTrust,
} from "../authority.ts";

const metadataSchema = z.object({
  issuer: z.string().url(),
  authorization_endpoint: z.string().url(),
  token_endpoint: z.string().url(),
  revocation_endpoint: z.string().url(),
  end_session_endpoint: z.string().url(),
  code_challenge_methods_supported: z.array(z.string()),
  trellis: z.object({
    trellisInstanceId: z.string().min(1),
    audienceNatsAccount: z.string().min(1),
    rotationSequence: z.string().regex(/^(0|[1-9][0-9]*)$/),
    natsEndpoints: z.array(z.string().min(1)).min(1),
    issuers: z
      .array(
        z.object({
          keyId: z.string().min(1),
          publicKey: z.string().min(1),
          state: z.enum(["active", "retired", "revoked"]),
        }),
      )
      .min(1),
  }),
});
const tokenSchema = z.object({
  access_token: z.string().min(1),
  token_type: z.string().refine((value) => value.toLowerCase() === "dpop"),
  refresh_token: z.string().min(1),
  expires_in: z.number().int().positive(),
  trellis_admission: z.string().min(1),
  authorization_details: z
    .array(
      z.object({
        type: z.literal("trellis_capabilities"),
        required: z.array(z.string()),
        optional: z.array(z.string()),
      }),
    )
    .optional(),
});

/** HTTPS-authenticated discovery scoped to the user's selected server. */
export type BrowserOAuthMetadata = z.infer<typeof metadataSchema>;
/** Fresh runtime material, never persisted alongside the durable DPoP key. */
export type BrowserOAuthAdmission = {
  readonly auth: TrellisAuth;
  readonly runtimeId: string;
  readonly admission: string;
  readonly verifiedAdmission: SignedOAuthAdmission;
  readonly trust: RuntimeAuthorizationTrust;
  readonly accessToken: string;
  readonly expiresIn: number;
  readonly authorizationDetails: z.infer<
    typeof tokenSchema
  >["authorization_details"];
  readonly metadata: BrowserOAuthMetadata;
};
/** Generated application selection, not uploaded participant IDL. */
export type BrowserCapabilityRequest = {
  readonly required: readonly string[];
  readonly optional: readonly string[];
};
const pendingSchema = z.object({
  issuer: z.string(),
  clientId: z.string(),
  redirectUri: z.string(),
  credentialHandle: z.string(),
  verifier: z.string(),
  state: z.string(),
  createdAt: z.number(),
});

/** Registered and CIMD public OAuth clients share this flow and durable custody. */
export class BrowserOAuthClient {
  readonly metadata: BrowserOAuthMetadata;
  readonly scope: BrowserOAuthScope;
  readonly store: BrowserSessionStore;
  #nonce?: string;
  #runtime?: Promise<{ auth: TrellisAuth; runtimeId: string }>;
  #owner?: SessionAuthorityOwner;

  private constructor(
    metadata: BrowserOAuthMetadata,
    scope: BrowserOAuthScope,
    store: BrowserSessionStore,
  ) {
    this.metadata = metadata;
    this.scope = scope;
    this.store = store;
  }

  /** Discover the selected issuer and reopen the exact app's credential custody. */
  static async open(options: {
    issuer: string;
    clientId: string;
    credentialHandle: string;
  }): Promise<Result<BrowserOAuthClient, AuthError>> {
    try {
      const issuer = new URL(options.issuer);
      // Plain HTTP only reaches this machine, which browsers already trust.
      const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(
        issuer.hostname,
      );
      if (
        issuer.username ||
        issuer.password ||
        issuer.hash ||
        issuer.search ||
        (issuer.protocol !== "https:" &&
          !(loopback && issuer.protocol === "http:"))
      )
        return err(new AuthError({ reason: "InvalidIssuer" }));
      const response = await fetch(
        `${issuer.href.replace(/\/+$/, "")}/.well-known/oauth-authorization-server`,
        {
          redirect: "error",
          credentials: "omit",
          signal: AbortSignal.timeout(30_000),
        },
      );
      if (!response.ok)
        return err(new AuthError({ reason: "OAuthUnavailable" }));
      const metadata = metadataSchema.parse(await boundedJson(response));
      if (
        new URL(metadata.issuer).href !== issuer.href ||
        !metadata.code_challenge_methods_supported.includes("S256") ||
        [
          metadata.authorization_endpoint,
          metadata.token_endpoint,
          metadata.revocation_endpoint,
          metadata.end_session_endpoint,
        ].some((endpoint) => new URL(endpoint).origin !== issuer.origin)
      )
        return err(new AuthError({ reason: "InvalidIssuer" }));
      const scope: BrowserOAuthScope = {
        issuer: metadata.issuer,
        instanceId: metadata.trellis.trellisInstanceId,
        account: metadata.trellis.audienceNatsAccount,
        browserOrigin: location.origin,
        clientId: options.clientId,
        credentialHandle: options.credentialHandle,
      };
      const active = metadata.trellis.issuers.filter(
        (key) => key.state === "active",
      );
      if (active.length !== 1)
        return err(new AuthError({ reason: "InvalidIssuerMetadata" }));
      const opened = (
        await BrowserSessionStore.open(scope, {
          instanceId: scope.instanceId,
          account: scope.account,
          issuer: active[0],
          rotationSequence: metadata.trellis.rotationSequence,
        })
      ).take();
      if (isErr(opened)) return err(opened.error);
      return ok(new BrowserOAuthClient(metadata, scope, opened));
    } catch {
      return err(new AuthError({ reason: "OAuthUnavailable" }));
    }
  }

  /** Revoke this app's refresh family and clear custody; does not log out of the Portal. */
  async revoke(): Promise<Result<void, AuthError>> {
    return this.store.clear(async (token) => {
      try {
        const response = await fetch(this.metadata.revocation_endpoint, {
          method: "POST",
          credentials: "omit",
          redirect: "error",
          signal: AbortSignal.timeout(30_000),
          body: new URLSearchParams({
            client_id: this.scope.clientId,
            token,
            token_type_hint: "refresh_token",
          }),
        });
        return response.ok
          ? ok(undefined)
          : err(new AuthError({ reason: "OAuthUnavailable" }));
      } catch {
        return err(new AuthError({ reason: "OAuthUnavailable" }));
      }
    });
  }

  /** Build a PKCE authorization URL and retain only this tab's continuation. */
  async authorize(
    redirectUri: string,
    capabilities: BrowserCapabilityRequest,
  ): Promise<Result<string, AuthError>> {
    try {
      const redirect = new URL(redirectUri);
      if (
        redirect.origin !== location.origin ||
        redirect.hash ||
        redirect.username ||
        redirect.password
      )
        return err(new AuthError({ reason: "InvalidRedirect" }));
      const verifier = base64urlEncode(
        crypto.getRandomValues(new Uint8Array(32)),
      );
      const state = base64urlEncode(crypto.getRandomValues(new Uint8Array(32)));
      const { crv, kty, x, y } = await crypto.subtle.exportKey(
        "jwk",
        this.store.dpop.publicKey,
      );
      const publicJwk = { crv, kty, x, y };
      const jkt = await dpopThumbprint(publicJwk);
      sessionStorage.setItem(
        this.#pendingKey(),
        JSON.stringify({
          issuer: this.scope.issuer,
          clientId: this.scope.clientId,
          credentialHandle: this.scope.credentialHandle,
          redirectUri,
          verifier,
          state,
          createdAt: Date.now(),
        }),
      );
      const url = new URL(this.metadata.authorization_endpoint);
      url.search = new URLSearchParams({
        response_type: "code",
        client_id: this.scope.clientId,
        redirect_uri: redirectUri,
        code_challenge: base64urlEncode(await sha256(utf8(verifier))),
        code_challenge_method: "S256",
        dpop_jkt: jkt,
        trellis_dpop_jwk: JSON.stringify(publicJwk),
        state,
        authorization_details: JSON.stringify([
          {
            type: "trellis_capabilities",
            required: capabilities.required,
            optional: capabilities.optional,
          },
        ]),
      }).toString();
      return ok(url.href);
    } catch {
      return err(new AuthError({ reason: "StorageUnavailable" }));
    }
  }

  /** Consume this tab's exact callback once; install signed authority before remembering its grant. */
  async complete(
    callback: string = location.href,
  ): Promise<
    Result<{ value: BrowserOAuthAdmission; refreshToken: string }, AuthError>
  > {
    try {
      const raw = sessionStorage.getItem(this.#pendingKey());
      if (!raw) return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
      const pending = pendingSchema.parse(JSON.parse(raw));
      const url = new URL(callback);
      const redirect = new URL(pending.redirectUri);
      if (
        pending.issuer !== this.scope.issuer ||
        pending.clientId !== this.scope.clientId ||
        pending.credentialHandle !== this.scope.credentialHandle ||
        Date.now() - pending.createdAt > 600_000 ||
        pending.createdAt > Date.now() ||
        url.origin !== redirect.origin ||
        url.pathname !== redirect.pathname ||
        url.searchParams.getAll("state").length !== 1 ||
        url.searchParams.get("state") !== pending.state ||
        url.searchParams.getAll("code").length > 1 ||
        url.searchParams.getAll("iss").length > 1 ||
        (url.searchParams.has("iss") &&
          url.searchParams.get("iss") !== this.metadata.issuer)
      )
        return err(new AuthError({ reason: "InvalidOAuthCallback" }));
      sessionStorage.removeItem(this.#pendingKey());
      if (url.searchParams.has("error"))
        return err(new AuthError({ reason: "OAuthAuthorizationDenied" }));
      const code = url.searchParams.get("code");
      if (!code) return err(new AuthError({ reason: "InvalidOAuthCallback" }));
      return await this.#exchange(
        new URLSearchParams({
          grant_type: "authorization_code",
          code,
          redirect_uri: pending.redirectUri,
          code_verifier: pending.verifier,
        }),
      );
    } catch {
      return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
    }
  }

  /** Serialize refresh across tabs and atomically commit rotation; reload makes a fresh runtime key. */
  async resume(
    install?: (admission: BrowserOAuthAdmission) => Promise<void>,
  ): Promise<Result<BrowserOAuthAdmission, AuthError>> {
    if (!install && !this.#owner)
      return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
    return await this.store.rotate(
      async (token) =>
        await this.#exchange(
          new URLSearchParams({
            grant_type: "refresh_token",
            refresh_token: token,
          }),
        ),
      async (admission) => {
        if (install) await install(admission);
        else if (!this.#owner)
          throw new AuthError({ reason: "OAuthRecoveryRequired" });
      },
      async () => {
        await this.#prepareTrust();
      },
    );
  }

  /** Bind the authenticated connection to the retained credential trust root. */
  async bindOwner(owner: SessionAuthorityOwner): Promise<void> {
    await this.store.retainTrust(owner);
    this.#owner = owner;
  }

  #pendingKey(): string {
    return `trellis.oauth.pending:${JSON.stringify([this.scope.issuer, this.scope.clientId, this.scope.credentialHandle])}`;
  }

  async #prepareTrust(metadata?: BrowserOAuthMetadata): Promise<void> {
    if (!metadata) {
      const response = await fetch(
        `${this.scope.issuer.replace(/\/+$/, "")}/.well-known/oauth-authorization-server`,
        {
          redirect: "error",
          credentials: "omit",
          signal: AbortSignal.timeout(30000),
        },
      );
      if (!response.ok)
        throw new AuthError({ reason: "OAuthTrustUnavailable" });
      metadata = metadataSchema.parse(await boundedJson(response));
    }
    const active = metadata.trellis.issuers.filter(
      (key) => key.state === "active",
    );
    if (metadata.issuer !== this.scope.issuer || active.length !== 1)
      throw new AuthError({ reason: "OAuthTrustUnavailable" });
    const owner = await authenticateIssuerChain(
      this.scope.issuer,
      this.#owner?.trustSnapshot ?? this.store.trust,
      {
        instanceId: metadata.trellis.trellisInstanceId,
        account: metadata.trellis.audienceNatsAccount,
        issuer: active[0],
        rotationSequence: metadata.trellis.rotationSequence,
      },
      undefined,
      this.#owner,
    );
    try {
      await this.store.retainTrust(owner);
    } finally {
      if (owner !== this.#owner) owner.stop();
    }
  }

  async #exchange(
    form: URLSearchParams,
  ): Promise<
    Result<{ value: BrowserOAuthAdmission; refreshToken: string }, AuthError>
  > {
    try {
      if (this.#owner) await this.store.retainTrust(this.#owner);
      // Rotation retains this document's ephemeral identity. Reload creates a new
      // client/runtime while the IndexedDB grant and non-extractable key survive.
      const { auth, runtimeId } = await (this.#runtime ??= createAuth({
        sessionKeySeed: base64urlEncode(
          crypto.getRandomValues(new Uint8Array(32)),
        ),
      }).then((auth) => ({
        auth,
        runtimeId: base64urlEncode(crypto.getRandomValues(new Uint8Array(16))),
      })));
      form.set("client_id", this.scope.clientId);
      form.set("trellis_runtime_id", runtimeId);
      form.set("trellis_session_nkey", auth.sessionNkey);
      const dpop = this.store.dpop;
      const publicJwk = await crypto.subtle.exportKey("jwk", dpop.publicKey);
      const response = await postWithDpop({
        endpoint: this.metadata.token_endpoint,
        form,
        key: {
          publicJwk,
          sign: async (bytes) =>
            await crypto.subtle.sign(
              { name: "ECDSA", hash: "SHA-256" },
              dpop.privateKey,
              toArrayBuffer(bytes),
            ),
        },
        runtimeId,
        sessionPublicKey: auth.sessionKey,
        nonce: this.#nonce,
        signal: AbortSignal.timeout(30_000),
      });
      if (response.nonce) this.#nonce = response.nonce;
      // Neither unsigned NATS errors nor transient HTTP failures delete the durable grant/key.
      if (!response.ok)
        return err(
          new AuthError({
            reason:
              response.status >= 500 || response.status === 429
                ? "OAuthUnavailable"
                : "OAuthRecoveryRequired",
          }),
        );
      const token = tokenSchema.parse(response.data);
      if (form.get("grant_type") === "refresh_token") {
        const saved = await this.store.retainRotatedToken(
          token.refresh_token,
          form.get("refresh_token") ?? "",
        );
        if (saved.isErr()) return err(saved.error);
      }
      // Recheck the authenticated chain after network I/O; an issuer retired
      // while the request was in flight must not mint a newly installed grant.
      if (this.#owner) await this.store.retainTrust(this.#owner);
      const verified = await verifyAdmission({
        admission: token.trellis_admission,
        ephemeralNkey: auth.sessionNkey,
        nowUnixSeconds: () => auth.currentIat(),
        trust: () => this.store.trust,
        refreshTrust: async () => await this.#prepareTrust(),
      });
      if (!verified.ok)
        return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
      const trust = verified.trust;
      const body = verified.admission.credential;
      const jkt = await dpopThumbprint(publicJwk);
      const grant = this.store.grant;
      if (
        (this.#owner &&
          this.#owner.trustSnapshot.issuer.keyId !== trust.issuer.keyId) ||
        body.runtimeId !== runtimeId ||
        body.clientId !== this.scope.clientId ||
        body.persistentDpopJkt !== jkt ||
        body.binding.bindingKind !== "browser" ||
        body.binding.browserOrigin !== location.origin ||
        body.binding.browserOrigin !== this.scope.browserOrigin ||
        (grant &&
          (body.principalId !== grant.principalId ||
            (form.get("grant_type") === "refresh_token" &&
              (body.oauthGrantId !== grant.oauthGrantId ||
                body.loginSessionId !== grant.loginSessionId))))
      )
        return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
      return ok({
        value: {
          auth,
          runtimeId,
          admission: token.trellis_admission,
          verifiedAdmission: verified.admission,
          trust,
          accessToken: token.access_token,
          expiresIn: token.expires_in,
          authorizationDetails: token.authorization_details,
          metadata: this.metadata,
        },
        refreshToken: token.refresh_token,
      });
    } catch (cause) {
      return err(
        cause instanceof AuthError
          ? cause
          : new AuthError({ reason: "OAuthRecoveryRequired" }),
      );
    }
  }
}
