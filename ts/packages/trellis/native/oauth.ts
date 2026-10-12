import { isIP } from "node:net";
import { setTimeout } from "node:timers/promises";
import { err, ok, type Result } from "@oatscenter/result";
import { z } from "zod";
import { fromPublic } from "@nats-io/nkeys";
import { createAuth, type TrellisAuth } from "../auth/session_auth.ts";
import {
  SessionAuthorityOwner,
  type RuntimeAuthorizationTrust,
} from "../auth/authority.ts";
import {
  verifyOAuthAdmissionWasm,
  type PlatformPrivilege,
} from "../auth/protocol_wasm.ts";
import {
  base64urlDecode,
  base64urlEncode,
  sha256,
  utf8,
  toArrayBuffer,
} from "../auth/utils.ts";
import { codecs, type AppRequestDescriptor } from "../generated_support.ts";
import { AuthError } from "../errors/AuthError.ts";
import { authenticateIssuerChain } from "../auth/issuer_trust.ts";
import {
  NativeCredentialLease,
  type NativeCredentialScope,
} from "./storage.ts";

const metadataSchema = z.object({
  issuer: z.string(),
  trellis: z.object({
    trellisInstanceId: z.string().min(1),
    audienceNatsAccount: z.string().min(1),
    natsEndpoints: z.array(z.string()).min(1),
    authorityFormat: z.literal("trellis.session-authority.v1"),
    admissionFormat: z.literal("trellis.oauth-admission.v1"),
    issuers: z
      .array(
        z.object({
          keyId: z.string().min(1),
          publicKey: z.string().min(1),
          state: z.enum(["active", "retired", "revoked"]),
        }),
      )
      .min(1),
    rotationSequence: z.string().regex(/^(0|[1-9][0-9]*)$/),
    requiredAuthorizationDetailsTypes: z
      .array(z.literal("trellis_capabilities"))
      .min(1),
  }),
});
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
  expires_in: z.number().int().positive().max(300),
  interval: z.number().int().positive().max(300).default(5),
});

/** Explicitly selected issuer, native transport addresses and initial authorization pin. */
export type NativeOAuthServer = Readonly<{
  issuer: string;
  natsServers: readonly string[];
  trust: RuntimeAuthorizationTrust;
}>;

async function boundedJson(response: Response): Promise<unknown> {
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

/** Discover only the selected HTTPS origin (literal loopback HTTP is allowed), without redirects. Retained custody never repins from discovery. */
export async function discoverNativeOAuth(
  origin: string,
  options: { signal?: AbortSignal } = {},
): Promise<Result<NativeOAuthServer, AuthError>> {
  try {
    const url = new URL(origin);
    const host = url.hostname.replace(/^\[|\]$/g, "");
    const loopback =
      host === "localhost" ||
      host === "::1" ||
      (isIP(host) === 4 && host.startsWith("127."));
    if (
      url.pathname !== "/" ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      (url.protocol !== "https:" && !(url.protocol === "http:" && loopback))
    )
      throw new AuthError({ reason: "InvalidIssuer" });
    const response = await fetch(
      `${url.origin}/.well-known/oauth-authorization-server`,
      {
        redirect: "error",
        credentials: "omit",
        signal: AbortSignal.any([
          AbortSignal.timeout(30000),
          ...(options.signal ? [options.signal] : []),
        ]),
      },
    );
    if (!response.ok) throw new AuthError({ reason: "OAuthUnavailable" });
    const metadata = metadataSchema.parse(await boundedJson(response));
    if (metadata.issuer !== url.origin)
      throw new AuthError({ reason: "InvalidIssuer" });
    codecs.ulid.decode(metadata.trellis.trellisInstanceId);
    if (
      !metadata.trellis.audienceNatsAccount.startsWith("A") ||
      fromPublic(metadata.trellis.audienceNatsAccount).getPublicKey() !==
        metadata.trellis.audienceNatsAccount
    )
      throw new AuthError({ reason: "InvalidIssuer" });
    for (const issuer of metadata.trellis.issuers) {
      const bytes = base64urlDecode(issuer.publicKey);
      if (
        bytes.length !== 32 ||
        base64urlEncode(bytes) !== issuer.publicKey ||
        base64urlEncode(await sha256(bytes)) !== issuer.keyId
      )
        throw new AuthError({ reason: "InvalidIssuer" });
    }
    const active = metadata.trellis.issuers.filter(
      (key) => key.state === "active",
    );
    if (
      active.length !== 1 ||
      new Set(metadata.trellis.issuers.map((key) => key.keyId)).size !==
        metadata.trellis.issuers.length
    )
      throw new AuthError({ reason: "InvalidIssuer" });
    for (const server of metadata.trellis.natsEndpoints) {
      const endpoint = new URL(server);
      if (
        !["nats:", "tls:", "ws:", "wss:"].includes(endpoint.protocol) ||
        !endpoint.hostname ||
        endpoint.username ||
        endpoint.password ||
        (endpoint.pathname && endpoint.pathname !== "/") ||
        endpoint.search ||
        endpoint.hash
      )
        throw new AuthError({ reason: "InvalidIssuer" });
    }
    const nativeServers = metadata.trellis.natsEndpoints.filter((server) =>
      /^(nats|tls):/.test(server),
    );
    if (nativeServers.length === 0)
      throw new AuthError({ reason: "InvalidIssuer" });
    return ok({
      issuer: url.origin,
      natsServers: nativeServers,
      trust: {
        instanceId: metadata.trellis.trellisInstanceId,
        account: metadata.trellis.audienceNatsAccount,
        issuer: active[0],
        rotationSequence: metadata.trellis.rotationSequence,
      },
    });
  } catch (cause) {
    return err(
      cause instanceof AuthError
        ? cause
        : new AuthError({
            reason: options.signal?.aborted
              ? "OAuthCancelled"
              : "OAuthUnavailable",
          }),
    );
  }
}

/** Native OAuth custody and a stable in-memory runtime key, independent of the transport owner. */
export class NativeOAuthClient {
  readonly auth: TrellisAuth;
  readonly runtimeId: string;
  readonly #lease: NativeCredentialLease;
  #tail: Promise<unknown> = Promise.resolve();
  #closing = false;
  #connected = false;
  #owner?: SessionAuthorityOwner;
  #anchor: RuntimeAuthorizationTrust;

  private constructor(lease: NativeCredentialLease, auth: TrellisAuth) {
    this.#lease = lease;
    this.#anchor = lease.trust;
    this.auth = auth;
    this.runtimeId = base64urlEncode(
      crypto.getRandomValues(new Uint8Array(16)),
    );
  }

  /** Acquire exclusive POSIX custody. Reopening generates a fresh Ed25519 key and 128-bit runtime nonce. */
  static async open(options: {
    directory: string;
    scope: NativeCredentialScope;
    trust: RuntimeAuthorizationTrust;
    signal?: AbortSignal;
  }): Promise<Result<NativeOAuthClient, AuthError>> {
    let lease: NativeCredentialLease | undefined;
    try {
      lease = await NativeCredentialLease.open(
        options.directory,
        options.scope,
        options.trust,
        options.signal,
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
    options: {
      signal?: AbortSignal;
      platformPrivileges?: {
        required: readonly PlatformPrivilege[];
        optional?: readonly PlatformPrivilege[];
      };
    } = {},
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
        ...(options.platformPrivileges
          ? [
              {
                type: "trellis_platform_privileges",
                required: options.platformPrivileges.required,
                optional: options.platformPrivileges.optional ?? [],
              },
            ]
          : []),
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
      const uri = new URL(data.verification_uri);
      if (uri.origin !== this.scope.issuer || uri.username || uri.password)
        throw new AuthError({ reason: "InvalidOAuthResponse" });
      return new NativeDeviceAuthorization(
        this,
        data.device_code,
        data.user_code,
        data.verification_uri,
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
      const token = this.#lease.beginRotation();
      try {
        return await this.#install(
          await this.#exchange(
            "/token",
            { grant_type: "refresh_token", refresh_token: token },
            options.signal,
          ),
          options.signal,
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
      if (this.#owner) this.#lease.retainTrust(this.#owner);
      else await this.#prepareTrust(options.signal);
      const token = this.#lease.beginRotation();
      try {
        return await this.#install(
          await this.#exchange(
            "/token",
            { grant_type: "refresh_token", refresh_token: token },
            options.signal,
          ),
          options.signal,
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
    const uri = `${this.scope.issuer}${endpoint}`;
    let nonce: string | undefined;
    const boundedSignal = AbortSignal.any([
      AbortSignal.timeout(30000),
      ...(signal ? [signal] : []),
    ]);
    const form = new URLSearchParams({
      ...fields,
      client_id: this.scope.clientId,
      trellis_runtime_id: this.runtimeId,
      trellis_session_nkey: this.auth.sessionNkey,
    });
    for (let attempt = 0; attempt < 2; attempt++) {
      const header = base64urlEncode(
        utf8(
          JSON.stringify({
            typ: "dpop+jwt",
            alg: "ES256",
            jwk: this.#lease.publicJwk,
          }),
        ),
      );
      const thumbprint = base64urlEncode(
        await sha256(
          utf8(
            JSON.stringify({
              crv: "Ed25519",
              kty: "OKP",
              x: this.auth.sessionKey,
            }),
          ),
        ),
      );
      const claims = base64urlEncode(
        utf8(
          JSON.stringify({
            htm: "POST",
            htu: uri,
            iat: Math.floor(Date.now() / 1000),
            jti: crypto.randomUUID(),
            trellis_runtime_id: this.runtimeId,
            trellis_nats_jkt: thumbprint,
            ...(nonce ? { nonce } : {}),
          }),
        ),
      );
      const input = `${header}.${claims}`;
      const proof = `${input}.${base64urlEncode(new Uint8Array(await this.#lease.sign(new Uint8Array(toArrayBuffer(utf8(input))))))}`;
      const response = await fetch(uri, {
        method: "POST",
        body: form,
        headers: { DPoP: proof },
        credentials: "omit",
        redirect: "error",
        signal: boundedSignal,
      });
      const data = await boundedJson(response);
      if (response.ok) return data;
      const failure = z.object({ error: z.string() }).parse(data);
      const challenge = response.headers.get("DPoP-Nonce");
      if (
        response.status === 400 &&
        failure.error === "use_dpop_nonce" &&
        attempt === 0 &&
        challenge &&
        challenge.length <= 1024 &&
        !challenge.includes(",")
      ) {
        nonce = challenge;
        continue;
      }
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
    throw new AuthError({ reason: "OAuthRecoveryRequired" });
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
      this.#lease.retainTrust(owner);
      if (!this.#connected) this.#anchor = this.trust;
    } finally {
      if (owner !== this.#owner) owner.stop();
    }
  }

  async #install(
    data: unknown,
    signal?: AbortSignal,
  ): Promise<NativeOAuthSession> {
    const response = tokenSchema.parse(data);
    if (response.token_type.toLowerCase() === "dpop")
      this.#lease.retainRotatedToken(response.refresh_token);
    let trust = this.trust;
    let verified = await verifyOAuthAdmissionWasm(
      utf8(response.trellis_admission),
      {
        issuer: trust.issuer,
        trellisInstanceId: trust.instanceId,
        audienceNatsAccount: trust.account,
        ephemeralNkey: this.auth.sessionNkey,
        nowUnixSeconds: Math.floor(Date.now() / 1000),
        maximumLifetimeSeconds: 300,
      },
    );
    if (!verified.ok) {
      // The old token was spent, but this freshly returned successor has never
      // been sent. Retain it without promoting unverified admission authority.
      await this.#prepareTrust(signal);
      trust = this.trust;
      verified = await verifyOAuthAdmissionWasm(
        utf8(response.trellis_admission),
        {
          issuer: trust.issuer,
          trellisInstanceId: trust.instanceId,
          audienceNatsAccount: trust.account,
          ephemeralNkey: this.auth.sessionNkey,
          nowUnixSeconds: Math.floor(Date.now() / 1000),
          maximumLifetimeSeconds: 300,
        },
      );
      if (!verified.ok) throw new AuthError({ reason: "InvalidOAuthResponse" });
    }
    const body = verified.admission.credential;
    const jwk = this.#lease.publicJwk;
    const jkt = base64urlEncode(
      await sha256(
        utf8(JSON.stringify({ crv: "P-256", kty: "EC", x: jwk.x, y: jwk.y })),
      ),
    );
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
    this.#lease.install({
      principalId: body.principalId,
      oauthGrantId: body.oauthGrantId,
      loginSessionId: body.loginSessionId,
      refreshToken: response.refresh_token,
    });
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
    interval: number,
    deadline: number,
  ) {
    this.#client = client;
    this.#code = code;
    this.userCode = userCode;
    this.verificationUri = verificationUri;
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
      while (performance.now() < this.#deadline) {
        await setTimeout(
          Math.min(this.#interval * 1000, this.#deadline - performance.now()),
          undefined,
          { signal: options.signal },
        );
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
