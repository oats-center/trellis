import { Kvm, type RoKV } from "@nats-io/kv";
import { createInbox, type NatsConnection } from "@nats-io/nats-core";
import { QueuedIteratorImpl } from "@nats-io/nats-core/internal";
import {
  AckPolicy,
  DeliverPolicy,
  jetstream,
  type ConsumerMessages,
} from "@nats-io/jetstream";
import {
  assertSessionAuthorityHandleCurrentWasm,
  verifyAdmissionContextWasm,
  type AdmissionGet,
  type AdmissionContext,
  authenticateCatalogWasm,
  authenticateProviderCertificateWasm,
  authorizeCatalogActionWasm,
  authorizeProviderEventPublicationWasm,
  type CatalogHandle,
  type ProviderCertificateHandle,
  createSessionAuthorityHandleWasm,
  type AuthorityIssuerKey,
  type AuthenticatedCaller,
  type CatalogActionIdentity,
  type MetadataVerification,
  type OriginalEventPublication,
  type SessionAuthority,
  type SessionAuthorityHandle,
  type SessionAuthorityVerificationPolicy,
  type SessionEvent,
  type SessionRequest,
  type SessionResponse,
  verifyAuthorityMetadataWasm,
  verifySessionEventWasm,
  verifySessionRequestWasm,
  verifySessionResponseWasm,
  type IssuerMetadataQuery,
  type IssuerMetadataResponse,
  authorizationSessionStatusSchema,
  signedBytes,
} from "./protocol_wasm.ts";
import { payloadSha256 } from "./message_digests.ts";
import { type JsonValue } from "./utils.ts";
import { requestIssuerMetadata } from "./issuer_metadata.ts";

const authorityLeaseToken = Symbol("verified authority lease");

type RecordObject = { [key: string]: JsonValue };
type CachedAuthority = {
  /** Private admission candidates cannot serve application frames before commit. */
  unpublished?: boolean;
  handle: SessionAuthorityHandle;
  authority: SessionAuthority;
  digest: string;
  references: number;
  retired: boolean;
  byteLength: number;
};
type VerifiedMaterial<H = undefined> = {
  bytes: Uint8Array;
  digest: string;
  record: RecordObject;
  /** Shared-protocol handle authenticated once at fetch, for per-use checks. */
  handle: H;
};
type CatalogMaterial = VerifiedMaterial<CatalogHandle>;
type ProviderMaterial = VerifiedMaterial<ProviderCertificateHandle>;

/** Signed bootstrap registry locations. These are runtime internals, never app authoring input. */
export type AuthorityRegistryBinding = {
  materialBucket: string;
  revocationBucket: string;
  issuer: AuthorityIssuerKey;
  rotationSequence: string;
};

/** Exact sent request retained until a verified reply or the ordinary timeout. */
export type PendingSessionRequest = {
  request: SessionRequest;
  proofDigest: string;
  action: CatalogActionIdentity;
  expectedDeploymentId: string | null;
};

/** Short-lived typed identity lease; cannot be constructed from unverified JSON. */
export class VerifiedAuthority {
  readonly #owner: SessionAuthorityOwner;
  readonly #entry: CachedAuthority;
  #released = false;

  private constructor(owner: SessionAuthorityOwner, entry: CachedAuthority) {
    this.#owner = owner;
    this.#entry = entry;
    entry.references++;
  }

  /** Only the common owner can lend authenticated state. */
  static acquire(
    token: typeof authorityLeaseToken,
    owner: SessionAuthorityOwner,
    entry: CachedAuthority,
  ): VerifiedAuthority {
    if (token !== authorityLeaseToken)
      throw new Error("unverified authority lease");
    return new VerifiedAuthority(owner, entry);
  }

  /** Authenticated fields; no authority is inferred merely from identity. */
  get authority(): Readonly<SessionAuthority> {
    return this.#entry.authority;
  }
  /** Content address of the exact immutable signed context. */
  get digest(): string {
    return this.#entry.digest;
  }
  /** Whether checked transport publication has released this private candidate. @internal */
  get published(): boolean {
    return !this.#entry.unpublished;
  }
  /** Internal verifier access requires the module-private lease token. */
  entry(token: typeof authorityLeaseToken): CachedAuthority {
    if (token !== authorityLeaseToken || this.#released)
      throw new Error("unverified authority lease");
    return this.#entry;
  }
  /** Verified logical caller identity usable by all SDK adapters. */
  get caller(): AuthenticatedCaller {
    const authority = this.#entry.authority;
    // Auth signs the user's own profile into a user principal's authority.
    const profile =
      authority.principalKind === "user"
        ? authority.extensions["trellis.user.v1"]
        : undefined;
    const text = (value: JsonValue | undefined) =>
      typeof value === "string" ? value : undefined;
    const user =
      profile !== null &&
      typeof profile === "object" &&
      !Array.isArray(profile) &&
      typeof profile.displayName === "string"
        ? {
            displayName: profile.displayName,
            ...(text(profile.givenName) && {
              givenName: text(profile.givenName),
            }),
            ...(text(profile.familyName) && {
              familyName: text(profile.familyName),
            }),
            ...(text(profile.email) && { email: text(profile.email) }),
            emailVerified: profile.emailVerified === true,
          }
        : undefined;
    return {
      authorityDigest: this.digest,
      principalId: authority.principalId,
      principalKind: authority.principalKind,
      binding: authority.binding,
      authorizationSessionId: authority.authorizationSessionId,
      runtimeId: authority.runtimeId,
      sessionPublicKey: authority.sessionPublicKey,
      inboxPrefix: authority.inboxPrefix,
      loginSessionId: authority.loginSessionId,
      oauthGrantId: authority.oauthGrantId,
      capabilities: authority.capabilities,
      apis: authority.apis,
      ...(user && { user }),
    };
  }
  /** Time and observed retirement are checked again immediately before authorized work. */
  assertCurrent(): void {
    if (this.#released) throw new Error("authority lease released");
    this.#owner.assertCurrent(this.#entry);
  }
  /** Authorize a dispatcher action through its actual signed whole-capability membership. */
  async authorize(apiId: string, action: CatalogActionIdentity): Promise<void> {
    this.assertCurrent();
    await this.#owner.authorize(this.#entry, apiId, action);
    this.assertCurrent();
  }
  /** Check one action against the verified authority and its exact accepted catalog. */
  async allows(apiId: string, action: CatalogActionIdentity): Promise<boolean> {
    this.assertCurrent();
    const allowed = await this.#owner.allows(this.#entry, apiId, action);
    this.assertCurrent();
    return allowed;
  }

  /** Return authenticated catalog evidence bound to this exact live authority. @internal */
  async authenticatedCatalog(
    apiId: string,
  ): Promise<Readonly<RecordObject> | undefined> {
    return await this.#owner.authenticatedCatalog(
      this.entry(authorityLeaseToken),
      apiId,
    );
  }

  /** Check the Auth-certified implementation, not consumer capability authority. */
  async authorizeProvider(
    apiId: string,
    generation: string,
    action: CatalogActionIdentity,
  ): Promise<void> {
    this.assertCurrent();
    await this.#owner.authorizeProvider(this.#entry, apiId, generation, action);
    this.assertCurrent();
  }
  /** Release local material only; accepted domain work is not rolled back. */
  release(): void {
    if (this.#released) return;
    this.#released = true;
    this.#entry.references--;
    if (this.#entry.retired && this.#entry.references === 0)
      this.#entry.handle.free();
  }
}

/** Selected installation and current active issuer authenticated by the common authority owner's rotation chain. */
export type RuntimeAuthorizationTrust = Readonly<{
  instanceId: string;
  account: string;
  issuer: Readonly<AuthorityIssuerKey>;
  rotationSequence: string;
}>;

/** One authority owner per runtime: pinned trust, bounded peer material, one revocation mirror. */
export class SessionAuthorityOwner {
  readonly instanceId: string;
  readonly account: string;
  readonly #keys = new Map<string, AuthorityIssuerKey>();
  readonly #peers = new Map<string, CachedAuthority>();
  readonly #materials = new Map<string, CatalogMaterial | ProviderMaterial>();
  readonly #pending = new Map<string, Promise<CachedAuthority>>();
  readonly #coldMaterials = new Map<
    string,
    Promise<CatalogMaterial | ProviderMaterial>
  >();
  readonly #revocations = new Map<
    string,
    { cutoff: number; expiresAt: number }
  >();
  readonly #now: () => number;
  readonly #listeners = new Set<() => void>();
  #metadataTransport?: {
    nats: NatsConnection;
    inboxPrefix: string;
    signal?: AbortSignal;
  };
  #metadataPending = 0;
  #material?: RoKV;
  #watch?: ConsumerMessages;
  #mirrorEpoch = 0;
  #mirrorCovered = false;
  #mirrorUpdates: Promise<void> = Promise.resolve();
  #mirrorFailed = false;
  #mirrorStop?: () => void;
  #mirrorPreparationStop?: () => void;
  #ownPreparationEpoch = 0;
  #rotationSequence: bigint;
  readonly #initialIssuers: readonly AuthorityIssuerKey[];
  readonly #initialSequence: string;
  readonly #authenticatedIssuerSequences = new Map<string, string>();
  #rotation?: Promise<void>;
  #own?: CachedAuthority;
  #ownAvailable = false;
  #stopped = false;
  #overloaded = false;

  /** Supply issuer pins from trusted provisioning or selected HTTPS discovery, not peer material. */
  constructor(options: {
    instanceId: string;
    account: string;
    issuers: readonly AuthorityIssuerKey[];
    rotationSequence?: string;
    now?: () => number;
  }) {
    if (!options.issuers.length || options.issuers.length > 128)
      throw new Error("issuer pin budget exceeded");
    this.instanceId = options.instanceId;
    this.account = options.account;
    this.#rotationSequence = BigInt(options.rotationSequence ?? "0");
    this.#initialSequence = this.#rotationSequence.toString();
    this.#initialIssuers = options.issuers.map((issuer) => ({ ...issuer }));
    if (options.issuers.length === 1)
      this.#authenticatedIssuerSequences.set(
        options.issuers[0].keyId,
        this.#initialSequence,
      );
    this.#now = options.now ?? (() => Math.floor(Date.now() / 1000));
    for (const key of options.issuers) {
      if (this.#keys.has(key.keyId)) throw new Error("duplicate issuer pin");
      this.#keys.set(key.keyId, Object.freeze({ ...key }));
    }
  }

  /** Snapshot current authenticated issuer trust and its durable rotation floor; does not discover or advance trust. */
  get trustSnapshot(): RuntimeAuthorizationTrust {
    return this.#currentTrust();
  }

  /** Obtain successor trust only from this owner's verified chain rooted at the exact retained credential pin. */
  trustForCredential(
    anchor: RuntimeAuthorizationTrust,
  ): RuntimeAuthorizationTrust {
    const initial = this.#keys.get(anchor.issuer.keyId);
    if (
      anchor.instanceId !== this.instanceId ||
      anchor.account !== this.account ||
      this.#initialIssuers.length !== 1 ||
      this.#authenticatedIssuerSequences.get(anchor.issuer.keyId) !==
        anchor.rotationSequence ||
      !initial ||
      initial.publicKey !== anchor.issuer.publicKey ||
      initial.state === "revoked"
    )
      throw new Error("credential trust anchor mismatch");
    return this.#currentTrust();
  }

  #currentTrust(): RuntimeAuthorizationTrust {
    if (this.#stopped) throw new Error("authority owner stopped");
    const active = [...this.#keys.values()].filter(
      (key) => key.state === "active",
    );
    if (active.length !== 1) throw new Error("active issuer unavailable");
    return Object.freeze({
      instanceId: this.instanceId,
      account: this.account,
      issuer: Object.freeze({ ...active[0] }),
      rotationSequence: this.#rotationSequence.toString(),
    });
  }

  /** Attach read-only public material and one snapshot-and-update mirror for all logical sessions. */
  async attach(
    nats: NatsConnection,
    binding: AuthorityRegistryBinding,
    options: {
      signal?: AbortSignal;
      timeoutMs?: number;
      inboxPrefix?: string;
    } = {},
  ): Promise<void> {
    if (this.#stopped) throw new Error("authority owner stopped");
    for (const name of [binding.materialBucket, binding.revocationBucket]) {
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(name))
        throw new Error("invalid authority registry");
    }
    this.#mirrorPreparationStop?.();
    const epoch = this.#mirrorEpoch + 1;
    let promoted = false;
    const metadataTransport = {
      nats,
      inboxPrefix: options.inboxPrefix ?? "_INBOX",
      signal: options.signal,
    };
    if (!this.#mirrorCovered) this.#metadataTransport = metadataTransport;
    const ready = Promise.withResolvers<void>();
    void ready.promise.catch(() => undefined);
    let watch: ConsumerMessages | undefined;
    let ended = false;
    let received = 0;
    let applied = 0;
    let revision = 0;
    let boundary: number | undefined;
    let prepared = false;
    let ordered = Promise.resolve();
    const current = () =>
      (promoted
        ? epoch === this.#mirrorEpoch
        : this.#mirrorPreparationStop === abort) &&
      !this.#stopped &&
      !ended;
    const fail = (error: Error, transportLost = false) => {
      if (ended) return;
      ended = true;
      if (promoted && epoch === this.#mirrorEpoch) {
        this.#mirrorCovered = false;
        this.#mirrorFailed = !transportLost && !nats.isClosed();
        for (const listener of this.#listeners) {
          try {
            listener();
          } catch {
            /* coverage is already fenced */
          }
        }
      }
      ready.reject(error);
      watch?.stop();
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", abort);
      if (statuses instanceof QueuedIteratorImpl) statuses.stop();
    };
    const abort = () => fail(new Error("authority mirror cancelled"), true);
    const timer = setTimeout(
      () => fail(new Error("authority mirror snapshot timeout")),
      options.timeoutMs ?? 30_000,
    );
    const check = () => {
      if (!current() || nats.isClosed() || options.signal?.aborted)
        throw new Error("authority mirror superseded");
    };
    const cover = () => {
      check();
      if (
        (promoted || prepared) &&
        boundary !== undefined &&
        applied >= boundary &&
        applied === received
      ) {
        const changed = !this.#mirrorCovered;
        if (!promoted) {
          const previousStop = this.#mirrorStop;
          // The current mirror remains authoritative until the candidate has
          // applied its complete authenticated snapshot and queued updates.
          promoted = true;
          this.#mirrorEpoch = epoch;
          this.#mirrorStop = abort;
          this.#mirrorPreparationStop = undefined;
          this.#watch = watch;
          this.#mirrorUpdates = ordered;
          this.#metadataTransport = metadataTransport;
          this.#material = material;
          this.#mirrorFailed = false;
          previousStop?.();
        }
        this.#mirrorCovered = true;
        clearTimeout(timer);
        ready.resolve();
        if (changed)
          for (const listener of this.#listeners) {
            try {
              listener();
            } catch {
              /* coverage is already restored */
            }
          }
      }
    };
    options.signal?.addEventListener("abort", abort, { once: true });
    const statuses = nats.status();
    this.#mirrorPreparationStop = abort;
    void (async () => {
      try {
        for await (const status of statuses) {
          if (!current()) break;
          if (status.type === "disconnect" || status.type === "close") {
            fail(new Error("authority mirror disconnected"), true);
            break;
          }
        }
      } catch {
        fail(new Error("authority mirror status failed"));
      }
    })();
    void nats
      .closed()
      .then(() => fail(new Error("authority mirror connection closed")));
    let material: RoKV;
    const setup = (async () => {
      check();
      material = await new Kvm(nats).open(binding.materialBucket, {
        bindOnly: true,
      });
      check();
      if (!this.#mirrorCovered) this.#material = material;
      await this.#advanceTrust(BigInt(binding.rotationSequence));
      check();
      const inbox = createInbox(options.inboxPrefix);
      const prefix = `$KV.${binding.revocationBucket}.`;
      const stream = `KV_${binding.revocationBucket}`;
      const name = `TrellisAuthority${inbox.split(".").at(-1)}`;
      const js = jetstream(nats, { checkAPI: false });
      const manager = await js.jetstreamManager(false);
      check();
      await manager.consumers.add(stream, {
        name,
        deliver_subject: inbox,
        deliver_policy: DeliverPolicy.LastPerSubject,
        ack_policy: AckPolicy.None,
        filter_subject: `${prefix}>`,
        flow_control: true,
        idle_heartbeat: 5_000_000_000,
        inactive_threshold: 10_000_000_000,
        mem_storage: true,
        num_replicas: 1,
      });
      check();
      const consumer = await js.consumers.getPushConsumer(stream, name);
      check();
      watch = await consumer.consume({
        callback: (message) => {
          if (!current()) return;
          const sequence = message.info.deliverySequence;
          if (
            !message.subject.startsWith(prefix) ||
            sequence !== received + 1 ||
            message.info.streamSequence <= revision
          ) {
            fail(new Error("authority mirror sequence gap"));
            return;
          }
          received = sequence;
          revision = message.info.streamSequence;
          // Fence authority synchronously at ingress until this update is applied.
          if (promoted) this.#mirrorCovered = false;
          ordered = ordered
            .then(async () => {
              check();
              const operation = message.headers?.get("KV-Operation") || "PUT";
              if (operation === "PUT") {
                try {
                  await this.observeRevocation(
                    message.data,
                    message.subject.slice(prefix.length),
                    current,
                  );
                } catch {
                  /* forged records cannot create or erase a cutoff */
                }
              } else if (operation !== "DEL" && operation !== "PURGE") {
                throw new Error("authority mirror operation invalid");
              }
              check();
              applied = sequence;
              cover();
            })
            .catch((error) =>
              fail(error instanceof Error ? error : new Error(String(error))),
            );
          if (promoted) this.#mirrorUpdates = ordered;
        },
        abort_on_missing_resource: true,
      });
      check();
      const events = watch.status();
      void (async () => {
        try {
          for await (const event of events) {
            if (!current()) return;
            if (
              (event.type === "heartbeat" &&
                event.lastConsumerSequence > received) ||
              event.type === "heartbeats_missed" ||
              event.type === "consumer_deleted" ||
              event.type === "stream_not_found" ||
              event.type === "consumer_not_found" ||
              event.type === "no_responders" ||
              event.type === "reset" ||
              event.type === "ordered_consumer_recreated"
            ) {
              fail(new Error(`authority mirror lost coverage: ${event.type}`));
              return;
            }
          }
        } catch {
          fail(new Error("authority mirror heartbeat status failed"));
        } finally {
          fail(new Error("authority mirror status ended"));
        }
      })();
      void watch.closed().then(() => fail(new Error("authority mirror ended")));
      const info = await consumer.info();
      check();
      boundary = info.delivered.consumer_seq + info.num_pending;
      await ordered;
      // Do not clear an ingress fence belonging to the predecessor while its
      // already-received revocations are still being authenticated.
      await this.#settleMirrorUpdates();
      check();
      prepared = true;
      cover();
      await ready.promise;
      check();
    })().catch((error) => {
      watch?.stop();
      throw error;
    });
    try {
      await Promise.race([setup, ready.promise]);
      check();
    } catch (error) {
      fail(error instanceof Error ? error : new Error(String(error)));
      watch?.stop();
      throw error;
    }
  }

  /** Verify an issuer-signed lookup reply against the actual pending ephemeral request. */
  async verifyAdmissionContext(
    bytes: Uint8Array,
    request: AdmissionGet,
    subject: string,
    reply: string,
  ): Promise<AdmissionContext> {
    if (this.#stopped || bytes.length > 2 * 1024 * 1024)
      throw new Error("Unavailable");
    const envelope: unknown = JSON.parse(new TextDecoder().decode(bytes));
    const response: unknown =
      envelope && typeof envelope === "object"
        ? Reflect.get(envelope, "response")
        : undefined;
    const keyId: unknown =
      response && typeof response === "object"
        ? Reflect.get(response, "issuerKeyId")
        : undefined;
    if (typeof keyId !== "string") throw new Error("Unavailable");
    const issuer = await this.#issuer(keyId);
    const verified = await verifyAdmissionContextWasm(
      bytes,
      issuer,
      request,
      subject,
      reply,
      this.account,
      this.#now(),
    );
    if (
      this.#stopped ||
      this.#keys.get(issuer.keyId)?.state !== "active" ||
      verified.expiresAt <= this.#now()
    )
      throw new Error("Unavailable");
    return verified;
  }

  /** Clock and bounds shared by all request/event/provider verification. */
  policy(): SessionAuthorityVerificationPolicy {
    return {
      nowUnixSeconds: this.#now(),
      allowedClockSkewSeconds: 5,
      maximumAuthorityLifetimeSeconds: 300,
      maximumAuthorityBytes: 512 * 1024,
      maximumEntries: 4096,
    };
  }

  /** Install non-evictable own authority only after complete signature, scope, key and lifecycle checks. */
  get ownDigest(): string | undefined {
    return this.#own?.digest;
  }

  /** Whether a verified sticky retirement affects the installed own logical session, even while its live lease is unavailable. */
  get ownRevoked(): boolean {
    return (
      this.#own !== undefined &&
      this.isRevoked(this.#own.authority.authorizationSessionId)
    );
  }

  /** Loss of current revocation coverage requiring private snapshot recovery. */
  get coverageLost(): boolean {
    return this.#mirrorFailed;
  }

  /** Verified retirement observed by the runtime's single mirror. */
  isRevoked(sessionId: string): boolean {
    return this.#revocations.has(sessionId);
  }

  /** Observe promotion and sticky lifecycle changes, without a per-context subscription. */
  subscribeChanges(callback: () => void): () => void {
    this.#listeners.add(callback);
    return () => this.#listeners.delete(callback);
  }

  /** Install non-evictable own authority only after complete signature, scope, key and lifecycle checks. */
  async installOwn(
    bytes: Uint8Array,
    expected: {
      runtimeId: string;
      sessionPublicKey: string;
      binding?: SessionAuthority["binding"];
    },
  ): Promise<VerifiedAuthority> {
    const candidate = await this.prepareOwn(bytes, expected);
    candidate.commit();
    return this.own();
  }

  /** Verify a private candidate; commit only at checked transport publication. */
  async prepareOwn(
    bytes: Uint8Array,
    expected: {
      runtimeId: string;
      sessionPublicKey: string;
      binding?: SessionAuthority["binding"];
    },
  ): Promise<{
    readonly digest: string;
    readonly authority: Readonly<SessionAuthority>;
    assertPreparedCurrent(): void;
    retain(): VerifiedAuthority;
    commit(): void;
    dispose(): void;
  }> {
    const epoch = ++this.#ownPreparationEpoch;
    const mirrorEpoch = this.#mirrorEpoch;
    const candidate = await this.#verify(bytes, "live");
    candidate.unpublished = true;
    try {
      if (
        candidate.authority.runtimeId !== expected.runtimeId ||
        candidate.authority.sessionPublicKey !== expected.sessionPublicKey ||
        (expected.binding &&
          Object.entries(expected.binding).some(
            ([key, value]) =>
              Reflect.get(candidate.authority.binding, key) !== value,
          ))
      )
        throw new Error("own authority binding mismatch");
      this.assertCurrent(candidate);
      let finished = false;
      const assertPreparedCurrent = () => {
        if (
          finished ||
          epoch !== this.#ownPreparationEpoch ||
          mirrorEpoch !== this.#mirrorEpoch
        )
          throw new Error("own authority candidate superseded");
        this.assertCurrent(candidate);
      };
      return {
        digest: candidate.digest,
        authority: candidate.authority,
        assertPreparedCurrent,
        retain: () => {
          assertPreparedCurrent();
          return VerifiedAuthority.acquire(
            authorityLeaseToken,
            this,
            candidate,
          );
        },
        commit: () => {
          if (finished) return;
          assertPreparedCurrent();
          finished = true;
          const previous = this.#own;
          this.#own = candidate;
          candidate.unpublished = false;
          this.#ownAvailable = true;
          if (previous) {
            previous.retired = true;
            if (!previous.references) previous.handle.free();
          }
          for (const listener of this.#listeners) {
            try {
              listener();
            } catch {
              /* promotion already committed */
            }
          }
        },
        dispose: () => {
          if (finished) return;
          finished = true;
          candidate.retired = true;
          if (!candidate.references) candidate.handle.free();
        },
      };
    } catch (error) {
      candidate.handle.free();
      throw error;
    }
  }

  /** Borrow the current own authority, which peer-cache pressure can never evict. */
  own(): VerifiedAuthority {
    if (!this.#own || !this.#ownAvailable)
      throw new Error("own authority unavailable");
    this.assertCurrent(this.#own);
    return VerifiedAuthority.acquire(authorityLeaseToken, this, this.#own);
  }

  /** Suspend application intake when the logical transport has no current generation. */
  setTransportAvailable(available: boolean): void {
    this.#ownAvailable = available;
    for (const listener of this.#listeners) listener();
  }

  /** Resolve immutable peer material once per digest, bounded independently from active local identity. */
  async peer(digest: string): Promise<VerifiedAuthority> {
    await this.#settleMirrorUpdates();
    if (this.#own?.digest === digest) return this.own();
    let entry = this.#peers.get(digest);
    if (!entry) {
      let pending = this.#pending.get(digest);
      if (!pending) {
        if (this.#pending.size >= 32)
          throw new Error("authority resolution budget exceeded");
        pending = (async () => {
          const bytes = await this.#read("authority", digest);
          const verified = await this.#verify(bytes, "live");
          try {
            if (verified.digest !== digest)
              throw new Error("authority digest mismatch");
            await this.#settleMirrorUpdates();
            this.assertCurrent(verified);
            this.#prunePeers(verified.byteLength);
            if (
              this.#peers.size >= 256 ||
              [...this.#peers.values()].reduce(
                (size, entry) => size + entry.byteLength,
                verified.byteLength,
              ) >
                16 * 1024 * 1024
            )
              throw new Error("peer authority budget exceeded");
            this.#peers.set(digest, verified);
            return verified;
          } catch (error) {
            verified.handle.free();
            throw error;
          }
        })();
        this.#pending.set(digest, pending);
        void pending
          .finally(() => this.#pending.delete(digest))
          .catch(() => undefined);
      }
      entry = await pending;
    }
    await this.#settleMirrorUpdates();
    this.assertCurrent(entry);
    this.#peers.delete(digest);
    this.#peers.set(digest, entry);
    return VerifiedAuthority.acquire(authorityLeaseToken, this, entry);
  }

  /** Authenticate and authorize an exact incoming request without exposing a raw decoded caller. */
  async verifyRequest(
    request: SessionRequest,
    payload: Uint8Array,
    proof: string,
    action: CatalogActionIdentity,
    role: "consumer" | "provider" = "consumer",
  ): Promise<{ caller: VerifiedAuthority; requestProofDigest: string }> {
    const caller = await this.peer(request.authorityDigest);
    try {
      if (role === "provider")
        await caller.authorizeProvider(
          request.apiId,
          request.apiGeneration,
          action,
        );
      else await caller.authorize(request.apiId, action);
      const entry = caller.entry(authorityLeaseToken);
      const verified = await verifySessionRequestWasm({
        authorityHandle: entry.handle,
        request,
        payloadSha256: await payloadSha256(payload),
        proof,
        policy: this.policy(),
        knownRevoked: this.#revocations.has(
          entry.authority.authorizationSessionId,
        ),
      });
      if (!verified.ok) throw new Error(verified.error.code);
      caller.assertCurrent();
      return { caller, requestProofDigest: verified.requestProofDigest };
    } catch (error) {
      caller.release();
      throw error;
    }
  }

  /** Verify actual provider result bytes against the still-pending opening, not response-supplied correlation. */
  async verifyResponse(
    response: SessionResponse,
    payload: Uint8Array,
    proof: string,
    receivedSubject: string,
    pending: PendingSessionRequest,
  ): Promise<
    Extract<AuthenticatedCaller["binding"], { kind: "service" | "device" }>
  > {
    const provider = await this.peer(response.providerAuthorityDigest);
    try {
      const certificate = await this.#metadata(
        "provider",
        response.providerCertificateDigest,
      );
      const issuer = await this.#issuer(String(certificate.record.issuerKeyId));
      await this.#settleMirrorUpdates();
      const verified = await verifySessionResponseWasm({
        providerHandle: provider.entry(authorityLeaseToken).handle,
        response,
        payloadSha256: await payloadSha256(payload),
        proof,
        certificate: certificate.handle,
        certificateIssuer: issuer,
        pendingRequest: pending.request,
        pendingRequestDigest: pending.proofDigest,
        action: pending.action,
        expectedDeploymentId: pending.expectedDeploymentId,
        receivedSubject,
        policy: this.policy(),
        knownRevoked: this.#revocations.has(
          provider.authority.authorizationSessionId,
        ),
      });
      if (!verified.ok) throw new Error(verified.error.code);
      await this.#settleMirrorUpdates();
      provider.assertCurrent();
      if (
        provider.authority.binding.kind !== "service" &&
        provider.authority.binding.kind !== "device"
      )
        throw new Error("response provider is not provisioned");
      if (
        response.kind === "success" &&
        pending.action.kind === "operation" &&
        pending.action.direction === "invoke"
      ) {
        const accepted = JSON.parse(new TextDecoder().decode(payload)) as {
          kind?: unknown;
          ref?: { deploymentId?: unknown };
        };
        if (
          accepted.kind !== "accepted" ||
          accepted.ref?.deploymentId !== provider.authority.binding.deploymentId
        ) {
          throw new Error(
            "operation reference does not belong to its certified accepting deployment",
          );
        }
      }
      return provider.authority.binding;
    } finally {
      provider.release();
    }
  }

  /** Verify an event live, or at the original trusted JetStream publication rather than replay time. */
  async verifyEvent(
    event: SessionEvent,
    payload: Uint8Array,
    proof: string,
    action: CatalogActionIdentity,
    originalPublication: OriginalEventPublication | null = null,
  ): Promise<AuthenticatedCaller> {
    let historical: CachedAuthority | undefined;
    let live: VerifiedAuthority | undefined;
    let historicalCutoff: number | undefined;
    try {
      if (originalPublication) {
        const bytes = await this.#read("authority", event.authorityDigest);
        const hints = this.#hints(bytes);
        if (typeof hints.authorizationSessionId !== "string")
          throw new Error("invalid historical session");
        const metadata = await this.readIssuerMetadata({
          kind: "authorityStatus",
          authorizationSessionId: hints.authorizationSessionId,
        });
        if (metadata.result.kind !== "value")
          throw new Error("historical revocation resolution unavailable");
        const status = authorizationSessionStatusSchema.parse(
          JSON.parse(metadata.result.payload),
        );
        if (status.authorizationSessionId !== hints.authorizationSessionId)
          throw new Error("historical status mismatch");
        if (status.revocation) {
          const revocation = await this.#verifyMetadata(
            "revocation",
            signedBytes(status.revocation),
          );
          if (
            revocation.record.authorizationSessionId !==
              hints.authorizationSessionId ||
            typeof revocation.record.effectiveCutoff !== "number"
          )
            throw new Error("historical cutoff mismatch");
          historicalCutoff = revocation.record.effectiveCutoff;
        } else if (status.state !== "active")
          throw new Error("historical cutoff unavailable");
        const known = this.#revocations.get(hints.authorizationSessionId);
        if (known)
          historicalCutoff = Math.min(
            historicalCutoff ?? known.cutoff,
            known.cutoff,
          );
        historical = await this.#verify(
          bytes,
          "historicalEvent",
          historicalCutoff,
        );
      }
      const entry = originalPublication
        ? historical!
        : (live = await this.peer(event.authorityDigest)).entry(
            authorityLeaseToken,
          );
      if (entry.digest !== event.authorityDigest)
        throw new Error("event authority digest mismatch");
      await this.authorize(
        entry,
        event.apiId,
        action,
        originalPublication !== null,
        {
          at: Math.floor(Date.parse(event.eventTime) / 1000),
          ...(originalPublication
            ? {
                publishedAt: Math.floor(
                  Date.parse(originalPublication.publishedAt) / 1000,
                ),
              }
            : {}),
        },
      );
      const verified = await verifySessionEventWasm({
        authorityHandle: entry.handle,
        event,
        payloadSha256: await payloadSha256(payload),
        proof,
        policy: this.policy(),
        originalPublication,
        knownRevoked: this.#revocations.has(
          entry.authority.authorizationSessionId,
        ),
      });
      if (!verified.ok) throw new Error(verified.error.code);
      if (!originalPublication) this.assertCurrent(entry);
      else {
        const observed = this.#revocations.get(
          entry.authority.authorizationSessionId,
        )?.cutoff;
        const cutoff = Math.min(
          historicalCutoff ?? Infinity,
          observed ?? Infinity,
        );
        if (Date.parse(originalPublication.publishedAt) / 1000 >= cutoff)
          throw new Error("EventRevoked");
      }
      const lease = VerifiedAuthority.acquire(authorityLeaseToken, this, entry);
      try {
        return lease.caller;
      } finally {
        lease.release();
      }
    } finally {
      historical?.handle.free();
      live?.release();
    }
  }

  /** Apply a verified sticky logical retirement; false mirror records are discarded before any mutation. */
  async observeRevocation(
    bytes: Uint8Array,
    expectedSession?: string,
    originCurrent?: () => boolean,
  ): Promise<void> {
    const verified = await this.#verifyMetadata("revocation", bytes);
    if (originCurrent && !originCurrent())
      throw new Error("revocation mirror superseded");
    const { authorizationSessionId, effectiveCutoff, latestContextExpiry } =
      verified.record;
    if (
      typeof authorizationSessionId !== "string" ||
      typeof effectiveCutoff !== "number" ||
      typeof latestContextExpiry !== "number" ||
      (expectedSession !== undefined &&
        expectedSession !== authorizationSessionId)
    )
      throw new Error("invalid revocation record");
    for (const [id, record] of this.#revocations)
      if (record.expiresAt <= this.#now()) this.#revocations.delete(id);
    const current = this.#revocations.get(authorizationSessionId);
    if (
      !current &&
      latestContextExpiry > this.#now() &&
      this.#revocations.size >= 4096
    ) {
      this.#overloaded = true;
      throw new Error("revocation budget exceeded");
    }
    if (latestContextExpiry > this.#now()) {
      this.#revocations.set(authorizationSessionId, {
        cutoff: Math.min(current?.cutoff ?? effectiveCutoff, effectiveCutoff),
        expiresAt: Math.max(
          current?.expiresAt ?? latestContextExpiry,
          latestContextExpiry,
        ),
      });
      for (const listener of this.#listeners) {
        try {
          listener();
        } catch {
          /* retirement remains sticky */
        }
      }
    }
  }

  async #settleMirrorUpdates(): Promise<void> {
    const epoch = this.#mirrorEpoch;
    let updates: Promise<void>;
    do {
      updates = this.#mirrorUpdates;
      await updates;
      if (epoch !== this.#mirrorEpoch)
        throw new Error("authority mirror superseded");
    } while (updates !== this.#mirrorUpdates);
    // This waits only for updates already received, never for lost coverage or
    // a replacement mirror. Callers still perform their normal live checks.
  }

  /** Recheck observed revocation and expiry after every cold asynchronous boundary. */
  assertCurrent(entry: CachedAuthority): void {
    if (this.#stopped || this.#overloaded || !this.#mirrorCovered)
      throw new Error("authority owner unavailable");
    const issuer = this.#keys.get(entry.authority.issuerKeyId);
    if (issuer?.state !== "active")
      throw new Error("issuer unavailable for live authority");
    assertSessionAuthorityHandleCurrentWasm(
      entry.handle,
      this.policy(),
      this.#revocations.has(entry.authority.authorizationSessionId),
    );
  }

  /** Shared authorization used by RPC, events and long-lived adapters. */
  async authorize(
    entry: CachedAuthority,
    apiId: string,
    action: CatalogActionIdentity,
    historical = false,
    publication?: { at: number; publishedAt?: number },
  ): Promise<void> {
    if (!(await this.allows(entry, apiId, action, historical, publication)))
      throw new Error("PermissionDenied");
  }

  /** Check an advisory action without treating verification failure as ordinary denial. */
  async allows(
    entry: CachedAuthority,
    apiId: string,
    action: CatalogActionIdentity,
    historical = false,
    publication?: { at: number; publishedAt?: number },
  ): Promise<boolean> {
    if (!historical) this.assertCurrent(entry);
    const api = entry.authority.apis.find((api) => api.apiId === apiId);
    if (!api) return false;
    const catalog = await this.#metadata("catalog", api.catalogSnapshotDigest);
    const issuer = await this.#issuer(String(catalog.record.issuerKeyId));
    let allowed = await authorizeCatalogActionWasm(
      entry.handle,
      catalog.handle,
      this.#metadataPolicy(issuer),
      action,
    );
    if (!allowed && action.kind === "event" && action.direction === "publish") {
      const binding = entry.authority.providerBindings.find(
        (binding) => binding.apiId === apiId,
      );
      if (
        binding &&
        (entry.authority.binding.kind === "service" ||
          entry.authority.binding.kind === "device")
      ) {
        const at = publication?.at ?? this.#now();
        const certificate = await this.#metadata(
          "provider",
          binding.providerCertificateDigest,
          at,
        );
        const providerIssuer = await this.#issuer(
          String(certificate.record.issuerKeyId),
        );
        allowed = await authorizeProviderEventPublicationWasm(
          entry.handle,
          catalog.handle,
          certificate.handle,
          {
            catalogIssuer: issuer,
            certificateIssuer: providerIssuer,
            trellisInstanceId: this.instanceId,
            audienceNatsAccount: this.account,
            at,
            ...(publication?.publishedAt !== undefined
              ? { publishedAt: publication.publishedAt }
              : {}),
          },
          action,
        );
      }
    }
    if (!historical) this.assertCurrent(entry);
    return allowed;
  }

  /** Resolve immutable catalog evidence for one API bound to a live verified authority. @internal */
  async authenticatedCatalog(
    entry: CachedAuthority,
    apiId: string,
  ): Promise<Readonly<RecordObject> | undefined> {
    this.assertCurrent(entry);
    const api = entry.authority.apis.find((api) => api.apiId === apiId);
    if (!api) return undefined;
    const catalog = await this.#metadata("catalog", api.catalogSnapshotDigest);
    const record = catalog.record;
    await this.#issuer(String(record.issuerKeyId));
    if (
      record.apiId !== apiId ||
      record.generation !== api.generation ||
      typeof record.acceptedRevision !== "string" ||
      BigInt(record.acceptedRevision) < BigInt(api.acceptedRevision)
    )
      throw new Error("catalog does not match authority binding");
    this.assertCurrent(entry);
    return structuredClone(record);
  }

  /** Authenticate the provider certificate and exact advertised action for a retained endpoint. */
  async authorizeProvider(
    entry: CachedAuthority,
    apiId: string,
    generation: string,
    action: CatalogActionIdentity,
  ): Promise<void> {
    this.assertCurrent(entry);
    const authority = entry.authority;
    const binding = authority.providerBindings.find(
      (binding) => binding.apiId === apiId && binding.generation === generation,
    );
    if (
      !binding ||
      (authority.binding.kind !== "service" &&
        authority.binding.kind !== "device")
    )
      throw new Error("PermissionDenied");
    const certificate = (
      await this.#metadata("provider", binding.providerCertificateDigest)
    ).record;
    if (
      certificate.principalId !== authority.principalId ||
      certificate.sessionPublicKey !== authority.sessionPublicKey ||
      certificate.deploymentId !== authority.binding.deploymentId ||
      certificate.instanceId !== authority.binding.instanceId ||
      certificate.apiId !== apiId ||
      certificate.generation !== generation ||
      certificate.implementationDigest !== binding.implementationDigest ||
      certificate.deploymentId !== binding.deploymentId ||
      typeof certificate.notBefore !== "number" ||
      typeof certificate.expiresAt !== "number" ||
      certificate.notBefore > this.#now() ||
      certificate.expiresAt <= this.#now() ||
      !Array.isArray(certificate.implementedActions) ||
      !certificate.implementedActions.some(
        (member) =>
          member &&
          typeof member === "object" &&
          Reflect.get(member, "kind") === action.kind &&
          Reflect.get(member, "name") === action.name &&
          Reflect.get(member, "direction") === action.direction,
      )
    )
      throw new Error("PermissionDenied");
    this.assertCurrent(entry);
  }

  /** Stop owned local work without erasing durable OAuth credentials or published historical material. */
  stop(): void {
    if (this.#stopped) return;
    this.#stopped = true;
    ++this.#mirrorEpoch;
    ++this.#ownPreparationEpoch;
    this.#mirrorCovered = false;
    this.#mirrorStop?.();
    this.#mirrorStop = undefined;
    this.#mirrorPreparationStop?.();
    this.#mirrorPreparationStop = undefined;
    this.#watch?.stop();
    this.#watch = undefined;
    for (const entry of [
      ...this.#peers.values(),
      ...(this.#own ? [this.#own] : []),
    ]) {
      entry.retired = true;
      if (!entry.references) entry.handle.free();
    }
    this.#peers.clear();
    this.#materials.clear();
    this.#own = undefined;
    for (const listener of this.#listeners) {
      try {
        listener();
      } catch {
        /* owner is already stopped */
      }
    }
    this.#listeners.clear();
  }

  /** Read public verification material using a fresh, pinned-issuer authenticated exchange. */
  async readIssuerMetadata(
    query: IssuerMetadataQuery,
  ): Promise<IssuerMetadataResponse> {
    return this.#readIssuerMetadata(query, true);
  }

  async #readIssuerMetadata(
    query: IssuerMetadataQuery,
    requireCoverage: boolean,
  ): Promise<IssuerMetadataResponse> {
    const transport = this.#metadataTransport;
    const epoch = this.#mirrorEpoch;
    const assertCurrent = () => {
      if (
        !transport ||
        this.#stopped ||
        (requireCoverage && !this.#mirrorCovered) ||
        this.#mirrorEpoch !== epoch ||
        transport.nats.isClosed() ||
        transport.nats.isDraining()
      )
        throw new Error("issuer metadata transport unavailable");
    };
    assertCurrent();
    if (!transport || this.#metadataPending >= 16)
      throw new Error("issuer metadata concurrency budget exceeded");
    this.#metadataPending++;
    try {
      const response = await requestIssuerMetadata({
        ...transport,
        instanceId: this.instanceId,
        account: this.account,
        query,
        issuer: async (keyId) => {
          // Never resolve a metadata envelope through another metadata lookup.
          const issuer = await this.#forwardIssuer(keyId);
          if (issuer?.state !== "active")
            throw new Error("untrusted issuer metadata signer");
          return issuer;
        },
        assertCurrent,
      });
      if (query.kind === "issuerKey" && response.result.kind === "value") {
        // The shared verifier checked the exact requested key and non-live state.
        const historical: AuthorityIssuerKey = JSON.parse(
          response.result.payload,
        );
        const previous = this.#keys.get(historical.keyId);
        if (previous && previous.publicKey !== historical.publicKey)
          throw new Error("historical issuer conflict");
        if (previous?.state === "active")
          throw new Error("historical issuer is a live pin");
        if (!previous && this.#keys.size >= 128)
          throw new Error("issuer history budget exceeded");
        if (previous?.state !== "revoked")
          this.#keys.set(historical.keyId, historical);
      }
      if (
        query.kind === "authorityStatus" &&
        response.result.kind === "value"
      ) {
        const status = authorizationSessionStatusSchema.parse(
          JSON.parse(response.result.payload),
        );
        if (status.authorizationSessionId !== query.authorizationSessionId)
          throw new Error("issuer metadata status identity mismatch");
        if (status.revocation)
          await this.observeRevocation(
            signedBytes(status.revocation),
            query.authorizationSessionId,
            () =>
              epoch === this.#mirrorEpoch &&
              this.#mirrorCovered &&
              !this.#stopped,
          );
        assertCurrent();
      }
      return response;
    } finally {
      this.#metadataPending--;
    }
  }

  #metadataPolicy(issuer: AuthorityIssuerKey): MetadataVerification {
    return {
      issuer,
      trellisInstanceId: this.instanceId,
      audienceNatsAccount: this.account,
      nowUnixSeconds: this.#now(),
      installedSequence: this.#rotationSequence.toString(),
    };
  }

  async #read(
    kind: "authority" | "catalog" | "provider" | "rotation",
    key: string,
  ): Promise<Uint8Array> {
    if (!this.#material || this.#stopped)
      throw new Error("authority material unavailable");
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(key))
      throw new Error("invalid material lookup key");
    const prefix =
      kind === "authority" ? "context" : kind === "catalog" ? "api" : kind;
    const entry = await this.#material.get(`${prefix}.${key}`);
    if (!entry || entry.operation !== "PUT" || entry.value.length > 1024 * 1024)
      throw new Error("authority material unavailable");
    return entry.value;
  }

  async #issuer(keyId: string): Promise<AuthorityIssuerKey> {
    let issuer = this.#keys.get(keyId);
    if (!issuer) {
      issuer = await this.#forwardIssuer(keyId);
    }
    if (!issuer) {
      const response = await this.#readIssuerMetadata(
        { kind: "issuerKey", keyId },
        false,
      );
      if (response.result.kind !== "value")
        throw new Error("historical authority issuer unavailable");
      issuer = this.#keys.get(keyId);
    }
    if (!issuer || issuer.state === "revoked")
      throw new Error("untrusted authority issuer");
    return issuer;
  }

  async #forwardIssuer(keyId: string): Promise<AuthorityIssuerKey | undefined> {
    if (!this.#keys.has(keyId)) {
      // Only consecutive predecessor signatures can extend live issuer trust.
      await this.#advanceTrust(this.#rotationSequence + 32n);
    }
    return this.#keys.get(keyId);
  }

  /** Authenticate one signed successor from this owner's existing durable pin. */
  async acceptIssuerRotation(bytes: Uint8Array): Promise<void> {
    while (this.#rotation) await this.#rotation;
    const rotation = this.#applyIssuerRotation(bytes);
    this.#rotation = rotation;
    try {
      await rotation;
    } finally {
      if (this.#rotation === rotation) this.#rotation = undefined;
    }
  }

  async #applyIssuerRotation(bytes: Uint8Array): Promise<void> {
    if (this.#stopped) throw new Error("authority owner stopped");
    const hints = this.#hints(bytes);
    const previous =
      typeof hints.previousKeyId === "string"
        ? this.#keys.get(hints.previousKeyId)
        : undefined;
    if (
      !previous ||
      previous.state !== "active" ||
      previous.keyId !== this.#currentTrust().issuer.keyId
    )
      throw new Error("untrusted rotation predecessor");
    const verified = await verifyAuthorityMetadataWasm(
      "rotation",
      bytes,
      this.#metadataPolicy(previous),
    );
    if (this.#stopped) throw new Error("authority owner stopped");
    if (
      this.#keys.get(previous.keyId)?.state !== "active" ||
      previous.keyId !== this.#currentTrust().issuer.keyId
    )
      throw new Error("rotation predecessor changed during verification");
    if (!verified.successor || this.#keys.size >= 128)
      throw new Error("issuer rotation budget exceeded");
    const existing = this.#keys.get(verified.successor.keyId);
    if (existing) throw new Error("issuer rotation conflict");
    this.#keys.set(previous.keyId, { ...previous, state: "retired" });
    this.#keys.set(verified.successor.keyId, verified.successor);
    this.#rotationSequence++;
    this.#authenticatedIssuerSequences.set(
      verified.successor.keyId,
      this.#rotationSequence.toString(),
    );
  }

  async #advanceTrust(target: bigint): Promise<void> {
    while (this.#rotation) {
      await this.#rotation;
      if (this.#rotationSequence >= target) return;
    }
    const rotation = (async () => {
      for (
        let count = 0;
        count < 32 && this.#rotationSequence < target;
        count++
      ) {
        let bytes: Uint8Array;
        try {
          bytes = await this.#read(
            "rotation",
            (this.#rotationSequence + 1n).toString(),
          );
        } catch {
          break;
        }
        await this.#applyIssuerRotation(bytes);
      }
    })();
    this.#rotation = rotation;
    try {
      await rotation;
    } finally {
      if (this.#rotation === rotation) this.#rotation = undefined;
    }
  }

  #hints(bytes: Uint8Array): RecordObject {
    if (!bytes.length || bytes.length > 1024 * 1024)
      throw new Error("metadata byte budget exceeded");
    // Lookup hints are never authority. Only the shared verifier's output enters trusted state.
    const value: JsonValue = JSON.parse(
      new TextDecoder("utf-8", { fatal: true }).decode(bytes),
    );
    if (!value || Array.isArray(value) || typeof value !== "object")
      throw new Error("invalid material");
    return value;
  }

  async #verify(
    bytes: Uint8Array,
    purpose: "live" | "historicalEvent",
    historicalCutoff?: number,
  ): Promise<CachedAuthority> {
    const mirrorEpoch = this.#mirrorEpoch;
    const hints = this.#hints(bytes);
    if (typeof hints.issuerKeyId !== "string")
      throw new Error("missing issuer lookup identity");
    const issuer = await this.#issuer(hints.issuerKeyId);
    const revoked =
      typeof hints.authorizationSessionId === "string"
        ? this.#revocations.get(hints.authorizationSessionId)
        : undefined;
    const verified = await createSessionAuthorityHandleWasm({
      authorityBytes: bytes,
      verification: {
        issuer,
        trellisInstanceId: this.instanceId,
        audienceNatsAccount: this.account,
        policy: this.policy(),
        purpose,
        revocationCutoff:
          historicalCutoff === undefined
            ? (revoked?.cutoff ?? null)
            : Math.min(historicalCutoff, revoked?.cutoff ?? Infinity),
      },
    });
    const entry: CachedAuthority = {
      handle: verified.handle,
      authority: verified.verified.authority,
      digest: verified.verified.authorityDigest,
      references: 0,
      retired: false,
      byteLength: bytes.length,
    };
    const pending: object[] = [entry.authority];
    while (pending.length) {
      const value = pending.pop()!;
      for (const child of Object.values(value))
        if (child && typeof child === "object") pending.push(child);
      Object.freeze(value);
    }
    try {
      if (purpose === "live") {
        await this.#settleMirrorUpdates();
        if (mirrorEpoch !== this.#mirrorEpoch)
          throw new Error("authority verification coverage superseded");
        this.assertCurrent(entry);
      }
      return entry;
    } catch (error) {
      entry.handle.free();
      throw error;
    }
  }

  async #verifyMetadata(
    kind: "revocation",
    bytes: Uint8Array,
    at?: number,
  ): Promise<VerifiedMaterial> {
    const hints = this.#hints(bytes);
    if (typeof hints.issuerKeyId !== "string")
      throw new Error("missing issuer lookup identity");
    const issuer = await this.#issuer(hints.issuerKeyId);
    const verified = await verifyAuthorityMetadataWasm(kind, bytes, {
      ...this.#metadataPolicy(issuer),
      ...(at !== undefined ? { nowUnixSeconds: at } : {}),
    });
    return {
      bytes: bytes.slice(),
      digest: verified.digest,
      record: verified.record,
      handle: undefined,
    };
  }

  /** Authenticate fetched catalog or certificate bytes once into a handle. */
  async #authenticateMaterial(
    kind: "catalog" | "provider",
    bytes: Uint8Array,
    at?: number,
  ): Promise<CatalogMaterial | ProviderMaterial> {
    const hints = this.#hints(bytes);
    if (typeof hints.issuerKeyId !== "string")
      throw new Error("missing issuer lookup identity");
    const issuer = await this.#issuer(hints.issuerKeyId);
    const policy = {
      ...this.#metadataPolicy(issuer),
      ...(at !== undefined ? { nowUnixSeconds: at } : {}),
    };
    const authenticated =
      kind === "catalog"
        ? await authenticateCatalogWasm(bytes, policy)
        : await authenticateProviderCertificateWasm(bytes, policy);
    return { bytes: bytes.slice(), ...authenticated };
  }

  async #metadata(
    kind: "catalog",
    digest: string,
    at?: number,
  ): Promise<CatalogMaterial>;
  async #metadata(
    kind: "provider",
    digest: string,
    at?: number,
  ): Promise<ProviderMaterial>;
  async #metadata(
    kind: "catalog" | "provider",
    digest: string,
    at?: number,
  ): Promise<CatalogMaterial | ProviderMaterial> {
    const key = `${kind}.${digest}`;
    const cached = this.#materials.get(key);
    if (cached) {
      this.#materials.delete(key);
      this.#materials.set(key, cached);
      return cached;
    }
    let pending = this.#coldMaterials.get(key);
    if (!pending) {
      if (this.#coldMaterials.size >= 32)
        throw new Error("metadata resolution budget exceeded");
      pending = (async () => {
        const verified = await this.#authenticateMaterial(
          kind,
          await this.#read(kind, digest),
          at,
        );
        if (verified.digest !== digest)
          throw new Error("metadata digest mismatch");
        if (this.#stopped) throw new Error("authority owner stopped");
        let total = [...this.#materials.values()].reduce(
          (size, entry) => size + entry.bytes.length,
          verified.bytes.length,
        );
        while (this.#materials.size >= 256 || total > 16 * 1024 * 1024) {
          const oldest = this.#materials.keys().next().value;
          if (oldest === undefined)
            throw new Error("metadata byte budget exceeded");
          total -= this.#materials.get(oldest)!.bytes.length;
          this.#materials.delete(oldest);
        }
        this.#materials.set(key, verified);
        return verified;
      })();
      this.#coldMaterials.set(key, pending);
      void pending
        .finally(() => this.#coldMaterials.delete(key))
        .catch(() => undefined);
    }
    return await pending;
  }

  #prunePeers(incomingBytes: number): void {
    let total = [...this.#peers.values()].reduce(
      (size, entry) => size + entry.byteLength,
      incomingBytes,
    );
    for (const [digest, entry] of this.#peers) {
      if (entry.references) continue;
      if (
        entry.authority.expiresAt > this.#now() &&
        this.#peers.size < 256 &&
        total <= 16 * 1024 * 1024
      )
        continue;
      this.#peers.delete(digest);
      entry.retired = true;
      entry.handle.free();
      total -= entry.byteLength;
    }
  }
}
