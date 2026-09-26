import type { NatsConnection } from "@nats-io/nats-core";

import type {
  AuthorizationContextHandle,
  AuthorizationIssuerKey,
  AuthorizationVerificationErrorCode,
  VerifiedAuthorizationContextTokenProjection,
  VerifiedAuthorizationEventPublisher,
} from "../protocol_wasm.ts";
import { canonicalizeJsonValue } from "../utils.ts";
import { trackCoverage } from "../../telemetry/lifecycle.ts";
import type { AuthorizationContextCache } from "./client_context.ts";
import {
  type AuthorizationRegistryIoCounters,
  AuthorizationRegistryReader,
  type RegistryWatchEntry,
} from "./nats_registry.ts";
import type {
  AuthorizationProviderEvent,
  AuthorizationProviderRequest,
  AuthorizationRegistryBinding,
} from "./types.ts";

const MAX_CACHED_CONTEXTS = 256;

type VerificationFailure = {
  ok: false;
  error: { code: AuthorizationVerificationErrorCode; path: string };
};

type CachedRequestVerificationResult =
  | {
    ok: true;
    contextDigest: string;
    context: VerifiedAuthorizationContextTokenProjection["context"];
  }
  | VerificationFailure;

type CachedEventVerificationResult =
  | {
    ok: true;
    contextDigest: string;
    context: VerifiedAuthorizationContextTokenProjection["context"];
    publisher: VerifiedAuthorizationEventPublisher;
  }
  | VerificationFailure;

/** Observable provider registry health. */
export type AuthorizationProviderCacheHealth = {
  revocationRevision: number;
  lastUpdateAt: number;
  healthy: boolean;
};

/** Provider I/O counters used by local hot-path tests and diagnostics. */
export type AuthorizationProviderIoCounters =
  & AuthorizationRegistryIoCounters
  & {
    contextResolves: number;
    contextVerifications: number;
  };

/** Internal marker for retryable provider registry or readiness failure. */
export class AuthorizationProviderUnavailableError extends Error {
  constructor(message: string, cause?: unknown) {
    super(message, { cause });
    this.name = "AuthorizationProviderUnavailableError";
  }
}

class InvalidIssuerResponseError extends Error {}

/** Provider attach options. */
export type AuthorizationProviderCacheOptions = { now?: () => number };

export type ProviderContextEntry = {
  contextDigest: string;
  context: Record<string, unknown>;
  issuer: AuthorizationIssuerKey;
  generation: number;
  covered: boolean;
  disposed: boolean;
  resourcesDisposed: boolean;
  revokedAt?: number;
  leases: number;
  watch?: AsyncIterator<RegistryWatchEntry>;
  closeWatch?: () => Promise<void>;
  live?: Promise<{
    handle: AuthorizationContextHandle;
    verified: VerifiedAuthorizationContextTokenProjection;
  }>;
  historical?: Promise<{
    handle: AuthorizationContextHandle;
    verified: VerifiedAuthorizationContextTokenProjection;
  }>;
};

/** One retained covered lease for a live observation session. */
export type LiveLeaseView = {
  readonly entry: ProviderContextEntry;
  readonly verified: VerifiedAuthorizationContextTokenProjection;
};

/** Typed coverage result for one retained live lease. */
export type LiveLeaseCoverage = "covered" | "revoked" | "lost" | "epoch";

type PendingContextEntry = {
  generation: number;
  owner: symbol;
  promise: Promise<ProviderContextEntry>;
};

/** Connected provider-side authorization verifier. */
export class AuthorizationProviderCache {
  readonly #registry: AuthorizationRegistryReader;
  readonly #cache: AuthorizationContextCache;
  readonly #now: () => number;
  readonly #contexts = new Map<string, ProviderContextEntry>();
  readonly #inFlight = new Map<string, PendingContextEntry>();
  readonly #ownIssuer: AuthorizationIssuerKey;
  #contextResolves = 0;
  #contextVerifications = 0;
  #revocationRevision = 0;
  #lastUpdateAt = 0;
  #stopped = false;
  #connected = true;
  #started = false;
  #generation = 0;
  #rotationOpen = false;
  #plannedGeneration: number | undefined;
  #ownEntry?: ProviderContextEntry;
  #onOwnInvalidated?: () => void;
  #onOwnResumed?: () => void;
  #ownRevokedDigest?: string;
  #ownUsable = true;
  #stopCoverage?: () => void;
  readonly #connectedWaiters = new Set<() => void>();
  readonly #liveChangeListeners = new Set<() => void>();

  private constructor(
    registry: AuthorizationRegistryReader,
    cache: AuthorizationContextCache,
    options: AuthorizationProviderCacheOptions,
  ) {
    this.#registry = registry;
    this.#cache = cache;
    this.#now = options.now ?? cache.correctedNowSeconds.bind(cache);
    this.#ownIssuer = structuredClone(cache.bundle().issuer);
  }

  /** Attach to the bootstrap-selected NATS authorization registry. */
  static async attach(
    nats: NatsConnection,
    binding: AuthorizationRegistryBinding,
    inboxPrefix: string,
    cache: AuthorizationContextCache,
    options: AuthorizationProviderCacheOptions = {},
  ): Promise<AuthorizationProviderCache> {
    if (
      canonicalizeJsonValue(binding) !==
        canonicalizeJsonValue(cache.bundle().authorizationRegistry)
    ) {
      throw new Error("authorization registry binding does not match");
    }
    return new AuthorizationProviderCache(
      await AuthorizationRegistryReader.open(
        nats,
        binding,
        inboxPrefix,
      ),
      cache,
      options,
    );
  }

  /** Enable provider verification. */
  start(): void {
    if (this.#started && !this.#stopped) return;
    this.#generation += 1;
    this.#stopped = false;
    this.#started = true;
    this.#stopCoverage = trackCoverage(() => {
      const healthy = this.#started && !this.#stopped && this.#connected;
      const now = this.#now();
      const current = (entry: ProviderContextEntry) =>
        healthy && entry.generation === this.#generation && entry.covered &&
        !entry.disposed && entry.revokedAt === undefined &&
        this.#contexts.get(entry.contextDigest) === entry &&
        typeof entry.context.notBefore === "number" &&
        entry.context.notBefore <= now &&
        typeof entry.context.expiresAt === "number" &&
        entry.context.expiresAt > now && entry.issuer.state !== "revoked";
      const installedDigest = this.#cache.storedContextDigest();
      const own = installedDigest === undefined
        ? undefined
        : this.#contexts.get(installedDigest);
      const ownCovered = !!own && this.#ownUsable && current(own) &&
        (this.#ownEntry === own || this.#cache.hasCandidate());
      let peerCovered = 0;
      let peerUnavailable = 0;
      for (const entry of this.#contexts.values()) {
        if (entry === own || entry === this.#ownEntry) {
          continue;
        }
        if (current(entry)) {
          peerCovered += 1;
        } else {
          peerUnavailable += 1;
        }
      }
      return { own: ownCovered, peerCovered, peerUnavailable };
    });
  }

  /** Stop verification without closing the caller-owned NATS connection. */
  stop(): void {
    this.#stopCoverage?.();
    this.#stopCoverage = undefined;
    this.#generation += 1;
    this.#rotationOpen = false;
    this.#plannedGeneration = undefined;
    this.#stopped = true;
    for (const entry of this.#contexts.values()) this.#invalidate(entry);
    this.#contexts.clear();
    this.#inFlight.clear();
    if (this.#ownEntry) this.#release(this.#ownEntry);
    this.#ownEntry = undefined;
    this.#notifyLiveChanges();
  }

  /**
   * Subscribe to local coverage, revocation and epoch changes.
   *
   * Retained live guards use this to fence quiet sessions without waiting for
   * the next frame. Notifications are advisory: the guard re-runs its own
   * synchronous check.
   */
  subscribeLiveChanges(callback: () => void): () => void {
    this.#liveChangeListeners.add(callback);
    return () => {
      this.#liveChangeListeners.delete(callback);
    };
  }

  #notifyLiveChanges(): void {
    for (const listener of [...this.#liveChangeListeners]) {
      try {
        listener();
      } catch {
        // A guard callback must never break cache bookkeeping.
      }
    }
  }

  /** Return the cache's corrected wall-clock seconds for signed validity. */
  liveNowSeconds(): number {
    return this.#now();
  }

  /**
   * Resolve and retain one covered lease for a live guard.
   *
   * The lease keeps the exact revocation watch alive; the caller must release
   * it when the guard is replaced or the session settles.
   */
  async retainLiveLease(
    contextDigest: string,
    generation: number,
  ): Promise<LiveLeaseView> {
    const entry = await this.#lease(contextDigest, false);
    try {
      this.#requireEntry(entry);
      if (entry.generation !== generation || generation !== this.#generation) {
        throw new AuthorizationProviderUnavailableError(
          "authorization context epoch changed before retention",
        );
      }
      if (entry.revokedAt !== undefined) {
        throw new AuthorizationProviderUnavailableError(
          "authorization context is revoked",
        );
      }
      const state = await this.#verified(entry, false);
      this.#requireEntry(entry);
      if (entry.revokedAt !== undefined) {
        throw new AuthorizationProviderUnavailableError(
          "authorization context is revoked",
        );
      }
      return { entry, verified: structuredClone(state.verified) };
    } catch (error) {
      this.#release(entry);
      throw error;
    }
  }

  /** Classify one retained live lease against current cache state. */
  liveLeaseCoverage(lease: LiveLeaseView): LiveLeaseCoverage {
    const entry = lease.entry;
    if (entry.revokedAt !== undefined) return "revoked";
    if (
      entry.generation !== this.#generation || !this.#started ||
      this.#stopped ||
      !this.#connected
    ) {
      return "epoch";
    }
    if (
      !entry.covered || entry.disposed ||
      this.#contexts.get(entry.contextDigest) !== entry
    ) {
      return "lost";
    }
    return "covered";
  }

  /** Release one retained live lease. */
  releaseLiveLease(lease: LiveLeaseView): void {
    this.#release(lease.entry);
  }

  /** Retain exact revocation coverage for the currently installed own context. */
  async retainOwnContext(): Promise<void> {
    const digest = this.#cache.storedContextDigest();
    if (digest === undefined) {
      throw new Error("authorization context is unavailable");
    }
    const generation = this.#generation;
    await this.#retainOwnContext(digest, generation);
    if (!this.#finalizeOwnInstallation("resume", digest, generation)) {
      this.#cache.requestRefresh();
      throw new Error(
        "authorization context coverage changed during resumption",
      );
    }
    this.#notifyLiveChanges();
  }

  /** Retain candidate coverage on the exact admitted connection generation. */
  async retainOwnCandidate(
    digest: string,
    generation: number,
  ): Promise<void> {
    await this.#retainOwnContext(digest, generation);
  }

  /** Promote the candidate only while its admitted coverage remains current. */
  promoteOwnCandidate(digest: string, generation: number): void {
    if (!this.#finalizeOwnInstallation("promote", digest, generation)) {
      throw new Error(
        "authorization candidate coverage changed before promotion",
      );
    }
  }

  /**
   * One synchronous final own-installation publication boundary.
   *
   * Rechecks the exact retained entry identity, generation, coverage,
   * revocation, current active/candidate digest, and validity immediately
   * before publishing, so invalidation consumed during an awaited retention
   * can never be overwritten by a later continuation.
   */
  #finalizeOwnInstallation(
    mode: "promote" | "resume",
    digest: string,
    generation: number,
  ): boolean {
    if (!this.#started || this.#stopped || !this.#connected) return false;
    if (generation !== this.#generation) return false;
    const entry = this.#ownEntry;
    if (
      !entry || entry.contextDigest !== digest || !entry.covered ||
      entry.disposed || entry.revokedAt !== undefined ||
      entry.generation !== generation || this.#contexts.get(digest) !== entry ||
      this.#ownRevokedDigest === digest
    ) {
      return false;
    }
    let verified: ReturnType<AuthorizationContextCache["transportCurrent"]>;
    if (mode === "promote") {
      if (
        !this.#cache.hasCandidate() ||
        this.#cache.transportCurrent().contextDigest !== digest
      ) return false;
      verified = this.#cache.transportCurrent();
    } else {
      if (this.#cache.hasCandidate()) return false;
      if (this.#cache.storedContextDigest() !== digest) return false;
      verified = this.#cache.current();
    }
    const now = this.#now();
    if (verified.context.notBefore > now || verified.context.expiresAt <= now) {
      return false;
    }
    if (mode === "promote") {
      this.#cache.promote(digest);
    }
    this.#rotationOpen = false;
    this.#ownUsable = true;
    this.#onOwnResumed?.();
    this.#notifyLiveChanges();
    return true;
  }

  connectionGeneration(): number {
    return this.#generation;
  }

  /** Return whether a planned rotation is awaiting coverage promotion. @internal */
  rotationOpen(): boolean {
    return this.#rotationOpen;
  }

  /**
   * Return whether one retained generation was opened by a planned rotation
   * and can still be rebound onto the replacement physical attachment. @internal
   */
  maintenanceFor(epoch: number): boolean {
    return this.#plannedGeneration !== undefined &&
      epoch < this.#plannedGeneration;
  }

  /** Return the installed own-context digest, if any. @internal */
  currentLocalContextDigest(): string | undefined {
    return this.#cache.storedContextDigest();
  }

  /** Abandon an unfinished planned rotation so retained guards fail closed. @internal */
  abandonRotation(): void {
    this.#rotationOpen = false;
    this.#plannedGeneration = undefined;
    this.#notifyLiveChanges();
  }

  /**
   * Wait, within a bounded budget, for a planned rotation to settle: either its
   * coverage is promoted onto the replacement attachment or it is abandoned.
   * @internal
   */
  waitRotationSettled(timeoutMs: number): Promise<boolean> {
    if (!this.#rotationOpen) return Promise.resolve(true);
    return new Promise<boolean>((resolve) => {
      let settled = false;
      const finish = (value: boolean): void => {
        if (settled) return;
        settled = true;
        unsubscribe();
        clearTimeout(timer);
        resolve(value);
      };
      const unsubscribe = this.subscribeLiveChanges(() => {
        if (!this.#rotationOpen) finish(true);
      });
      const timer = setTimeout(() => finish(!this.#rotationOpen), timeoutMs);
      if (!this.#rotationOpen) finish(true);
    });
  }

  async #retainOwnContext(digest: string, generation: number): Promise<void> {
    const entry = await this.#lease(digest, false);
    if (
      entry.revokedAt !== undefined || !entry.covered ||
      entry.generation !== generation || generation !== this.#generation ||
      !this.#connected
    ) {
      this.#release(entry);
      throw new Error(
        "authorization context coverage changed during installation",
      );
    }
    if (this.#ownEntry) this.#release(this.#ownEntry);
    this.#ownEntry = entry;
  }

  /** Returns whether the current own authorization may authenticate transport. */
  ownUsable(): boolean {
    return this.#ownUsable;
  }

  /** A verified private candidate or a retained unrevoked installation may authenticate transport. */
  transportUsable(): boolean {
    if (!this.#started || this.#stopped) return false;
    if (this.#ownUsable) return true;
    const digest = this.#cache.hasCandidate()
      ? this.#cache.transportCurrent().contextDigest
      : this.#cache.storedContextDigest();
    return digest !== undefined && digest !== this.#ownRevokedDigest;
  }

  /** Suspend stale transport authentication and request one current context. */
  refreshOwnAuthorization(): void {
    const revoked = this.#cache.storedContextDigest();
    if (revoked !== undefined) this.#ownRevokedDigest = revoked;
    if (this.#ownUsable) {
      this.#ownUsable = false;
      this.#onOwnInvalidated?.();
    }
    this.#cache.requestRefresh();
    this.#notifyLiveChanges();
  }

  /**
   * Apply one raw framework transport event to provider readiness.
   *
   * Provider verification coverage is tied to the physical NATS attachment,
   * so a physical loss always invalidates retained coverage and forces exact
   * coverage to be rebuilt on the replacement attachment. `planned` marks a
   * loss that is part of an in-progress authorization-credential rotation:
   * coverage is still rebuilt, but the owner's logical application
   * availability is not withdrawn.
   */
  observeTransportEvent(event: unknown, planned = false): void {
    if (!event || typeof event !== "object") return;
    const status = event as { type?: unknown; data?: unknown };
    switch (status.type) {
      case "disconnect":
      case "disconnected":
      case "reconnecting":
      case "forceReconnect":
        this.#observePhysicalConnected(false, planned);
        break;
      case "reconnect":
        this.#observePhysicalConnected(true, planned);
        break;
    }
    if (
      status.type === "error" &&
      String(status.data).toLowerCase().includes("authorization")
    ) {
      this.refreshOwnAuthorization();
    }
  }

  #observePhysicalConnected(connected: boolean, planned: boolean): void {
    const wasConnected = this.#connected;
    this.#connected = connected;
    if (wasConnected && !this.#connected) {
      // A planned rotation keeps the owner's logical availability installed:
      // the predecessor remains application-current until the candidate is
      // admitted and promoted on the replacement attachment.
      if (!planned) {
        this.#ownUsable = false;
        this.#onOwnInvalidated?.();
      }
      if (this.#ownEntry) this.#release(this.#ownEntry);
      this.#ownEntry = undefined;
      this.#generation += 1;
      if (planned) {
        this.#rotationOpen = true;
        this.#plannedGeneration = this.#generation;
      } else {
        this.#rotationOpen = false;
        this.#plannedGeneration = undefined;
      }
      for (const entry of this.#contexts.values()) this.#invalidate(entry);
      this.#contexts.clear();
      this.#inFlight.clear();
      this.#notifyLiveChanges();
    }
    if (!wasConnected && this.#connected) {
      for (const ready of this.#connectedWaiters) ready();
      this.#connectedWaiters.clear();
      this.#notifyLiveChanges();
      if (!this.#cache.hasCandidate()) {
        const generation = this.#generation;
        void this.#restoreOwnContext(generation);
      }
    }
  }

  /** Register the connection-owned usability withdrawal for own revocation. */
  onOwnInvalidated(callback: () => void): void {
    this.#onOwnInvalidated = callback;
  }

  /** Register the connection-owned resumption after same-context coverage initialization. */
  onOwnResumed(callback: () => void): void {
    this.#onOwnResumed = callback;
  }

  /** Wait until the connected registry is available. */
  waitReady(
    options: { signal?: AbortSignal; timeoutMs?: number } = {},
  ): Promise<void> {
    if (!this.#started || this.#stopped) {
      return Promise.reject(
        new AuthorizationProviderUnavailableError(
          "authorization provider is unavailable",
        ),
      );
    }
    if (options.signal?.aborted) return Promise.reject(options.signal.reason);
    if (this.#connected) return Promise.resolve();
    return new Promise<void>((resolve, reject) => {
      const ready = () => {
        clearTimeout(timer);
        options.signal?.removeEventListener("abort", aborted);
        resolve();
      };
      const aborted = () => {
        this.#connectedWaiters.delete(ready);
        clearTimeout(timer);
        reject(options.signal?.reason);
      };
      const timer = setTimeout(() => {
        this.#connectedWaiters.delete(ready);
        options.signal?.removeEventListener("abort", aborted);
        reject(
          new AuthorizationProviderUnavailableError(
            "authorization provider readiness timed out",
          ),
        );
      }, options.timeoutMs ?? 30_000);
      this.#connectedWaiters.add(ready);
      options.signal?.addEventListener("abort", aborted, { once: true });
    });
  }

  /** Return current provider health. */
  health(): AuthorizationProviderCacheHealth {
    return {
      revocationRevision: this.#revocationRevision,
      lastUpdateAt: this.#lastUpdateAt,
      healthy: this.#started && !this.#stopped && this.#connected,
    };
  }

  /** Return provider and registry I/O counters. */
  ioCounters(): AuthorizationProviderIoCounters {
    return {
      ...this.#registry.ioCounters(),
      contextResolves: this.#contextResolves,
      contextVerifications: this.#contextVerifications,
    };
  }

  async #restoreOwnContext(generation: number): Promise<void> {
    while (
      !this.#stopped && this.#connected && this.#generation === generation
    ) {
      try {
        await this.retainOwnContext();
        return;
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 1_000));
      }
    }
  }

  /** Resolve one context digest through the connected registry. */
  async resolveContext(
    contextDigest: string,
  ): Promise<VerifiedAuthorizationContextTokenProjection> {
    const entry = await this.#lease(contextDigest, false);
    try {
      const state = await this.#verified(entry, false);
      this.#requireEntry(entry);
      const { assertAuthorizationContextHandleCurrentWasm } = await import(
        "../protocol_wasm.ts"
      );
      assertAuthorizationContextHandleCurrentWasm(
        state.handle,
        this.#policy(this.#now()),
      );
      this.#requireEntry(entry);
      if (entry.revokedAt !== undefined) {
        throw new Error("authorization context is revoked");
      }
      return structuredClone(state.verified);
    } finally {
      this.#release(entry);
    }
  }

  /** Verify a presented request proof with exact route permissions. */
  async verifyRequest(
    request: AuthorizationProviderRequest,
  ): Promise<CachedRequestVerificationResult> {
    try {
      const entry = await this.#lease(request.contextDigest, false);
      try {
        this.#requireEntry(entry);
        if (entry.revokedAt !== undefined) {
          return requestFailure("PermissionDenied", "/authorization-context");
        }
        const state = await this.#verified(entry, false);
        this.#requireEntry(entry);
        if (request.sessionKey !== state.verified.context.sessionKey) {
          return requestFailure("InvalidInput", "/session-key");
        }
        const { verifyAuthorizationRequestWasm } = await import(
          "../protocol_wasm.ts"
        );
        const result = await verifyAuthorizationRequestWasm({
          contextHandle: state.handle,
          subject: request.subject,
          reply: request.reply,
          payload: request.payload,
          iat: request.iat,
          requestId: request.requestId,
          proof: request.proof,
          requiredPermissions: request.requiredPermissions,
          policy: this.#policy(this.#now()),
        });
        this.#requireEntry(entry);
        if (!result.ok) return result;
        if (
          result.contextDigest !== request.contextDigest ||
          state.verified.contextDigest !== request.contextDigest ||
          entry.revokedAt !== undefined
        ) {
          return requestFailure("PermissionDenied", "/authorization-context");
        }
        return { ...result, context: state.verified.context };
      } finally {
        this.#release(entry);
      }
    } catch (error) {
      if (error instanceof AuthorizationProviderUnavailableError) throw error;
      return requestFailure("InvalidInput", "/authorization-context");
    }
  }

  /** Verify a presented event proof, including historical issuer evidence. */
  async verifyEvent(
    event: AuthorizationProviderEvent,
  ): Promise<CachedEventVerificationResult> {
    try {
      const entry = await this.#lease(event.contextDigest, true);
      try {
        this.#requireEntry(entry);
        const state = await this.#verified(entry, true);
        this.#requireEntry(entry);
        if (event.sessionKey !== state.verified.context.sessionKey) {
          return eventFailure("InvalidInput", "/session-key");
        }
        const { verifyAuthorizationEventWasm } = await import(
          "../protocol_wasm.ts"
        );
        const result = await verifyAuthorizationEventWasm({
          contextHandle: state.handle,
          descriptorIdentity: event.descriptorIdentity,
          subject: event.subject,
          payload: event.payload,
          eventId: event.eventId,
          eventTime: event.eventTime,
          proof: event.proof,
          policy: this.#policy(this.#now()),
          revokedAt: entry.revokedAt ?? null,
        });
        this.#requireEntry(entry);
        if (!result.ok) return result;
        if (
          result.contextDigest !== event.contextDigest ||
          state.verified.contextDigest !== event.contextDigest ||
          entry.revokedAt !== undefined
        ) {
          return eventFailure("EventRevoked", "/authorization-context");
        }
        return { ...result, context: state.verified.context };
      } finally {
        this.#release(entry);
      }
    } catch (error) {
      if (error instanceof AuthorizationProviderUnavailableError) throw error;
      return eventFailure("InvalidInput", "/authorization-context");
    }
  }

  async #entry(
    contextDigest: string,
    historical: boolean,
  ): Promise<ProviderContextEntry> {
    assertDigest(contextDigest);
    this.#requireAvailable();
    const existing = this.#contexts.get(contextDigest);
    if (existing) {
      this.#requireEntry(existing);
      this.#contexts.delete(contextDigest);
      this.#contexts.set(contextDigest, existing);
      return existing;
    }
    let pending = this.#inFlight.get(contextDigest);
    if (!pending) {
      this.#makeRoom();
      const generation = this.#generation;
      const owner = Symbol(contextDigest);
      pending = {
        generation,
        owner,
        promise: this.#load(contextDigest, historical, generation, owner),
      };
      this.#inFlight.set(contextDigest, pending);
    }
    try {
      return await pending.promise;
    } finally {
      if (this.#inFlight.get(contextDigest) === pending) {
        this.#inFlight.delete(contextDigest);
      }
    }
  }

  async #lease(
    contextDigest: string,
    historical: boolean,
  ): Promise<ProviderContextEntry> {
    const entry = await this.#entry(contextDigest, historical);
    this.#requireEntry(entry);
    entry.leases += 1;
    this.#contexts.delete(contextDigest);
    this.#contexts.set(contextDigest, entry);
    return entry;
  }

  #makeRoom(): void {
    if (this.#contexts.size + this.#inFlight.size < MAX_CACHED_CONTEXTS) return;
    const oldest = [...this.#contexts].find(([, entry]) => entry.leases === 0);
    if (!oldest) {
      throw new AuthorizationProviderUnavailableError(
        "provider context cache capacity reached",
      );
    }
    this.#invalidate(oldest[1]);
  }

  async #load(
    contextDigest: string,
    historical: boolean,
    generation: number,
    owner: symbol,
  ): Promise<ProviderContextEntry> {
    assertDigest(contextDigest);
    this.#contextResolves += 1;
    const watch = await this.#registryIo(
      "authorization revocation watch is unavailable",
      () => this.#registry.watchRevocation(contextDigest),
    );
    let entry: ProviderContextEntry | undefined;
    let watchOwned = false;
    try {
      const contextEntry = await this.#registryIo(
        "authorization context registry is unavailable",
        () => this.#registry.getContext(contextDigest),
      );
      if (!contextEntry) {
        throw new AuthorizationProviderUnavailableError(
          "authorization context is missing",
        );
      }
      let context: Record<string, unknown>;
      try {
        context = parseJsonRecord(contextEntry.value);
      } catch (error) {
        throw new AuthorizationProviderUnavailableError(
          "authorization context registry entry is malformed",
          error,
        );
      }
      const issuerKeyId = String(context.issuerKeyId ?? "");
      if (!issuerKeyId) {
        throw new AuthorizationProviderUnavailableError(
          "authorization context issuer is missing",
        );
      }
      const issuer = await this.#issuer(issuerKeyId);
      entry = {
        contextDigest,
        context,
        issuer,
        generation,
        covered: false,
        disposed: false,
        resourcesDisposed: false,
        leases: 0,
        watch: watch.iterator,
        closeWatch: watch.close,
      };
      watchOwned = true;
      while (true) {
        const result = await this.#registryIo(
          "authorization revocation watch is unavailable",
          () => watch.iterator.next(),
        );
        if (result.done) {
          throw new AuthorizationProviderUnavailableError(
            "authorization revocation watch ended during initialization",
          );
        }
        if (result.value.operation === "initialized") break;
        this.#applyRevocation(entry, result.value);
      }
      entry.covered = true;
      void this.#watchRevocation(entry, watch.iterator);
      const verified = await this.#verified(entry, historical);
      if (verified.verified.contextDigest !== contextDigest) {
        throw new Error(
          "authorization context digest does not match its registry key",
        );
      }
      this.#requirePendingOwner(contextDigest, generation, owner);
      if (entry.disposed || !entry.covered) {
        throw new AuthorizationProviderUnavailableError(
          "authorization context lost revocation coverage",
        );
      }
      this.#contexts.set(contextDigest, entry);
      this.#notifyLiveChanges();
      return entry;
    } catch (error) {
      if (entry) {
        this.#invalidate(entry);
      } else if (!watchOwned) {
        await watch.close();
      }
      throw error;
    }
  }

  async #watchRevocation(
    entry: ProviderContextEntry,
    iterator: AsyncIterator<RegistryWatchEntry>,
  ): Promise<void> {
    try {
      while (entry.covered && !entry.disposed) {
        const result = await iterator.next();
        if (result.done) return;
        if (result.value.operation === "initialized") continue;
        this.#applyRevocation(entry, result.value);
      }
    } catch {
      // Coverage is invalidated below; the next request resynchronizes it.
    } finally {
      this.#invalidate(entry);
      try {
        await iterator.return?.();
      } catch {
        // The entry is already unusable and will reload through a new watch.
      }
    }
  }

  #applyRevocation(
    entry: ProviderContextEntry,
    event: Exclude<RegistryWatchEntry, { operation: "initialized" }>,
  ): void {
    this.#revocationRevision = Math.max(
      this.#revocationRevision,
      event.revision,
    );
    this.#lastUpdateAt = this.#now();
    if (event.operation !== "put") {
      throw new AuthorizationProviderUnavailableError(
        "authorization revocation registry entry disappeared",
      );
    }
    this.#notifyLiveChanges();
    entry.revokedAt = Math.max(
      entry.revokedAt ?? 0,
      parseRevocation(event.value),
    );
    let ownDigest: string | undefined;
    let candidateDigest: string | undefined;
    try {
      ownDigest = this.#cache.storedContextDigest();
      candidateDigest = this.#cache.hasCandidate()
        ? this.#cache.transportCurrent().contextDigest
        : undefined;
    } catch {
      // No own installation remains to invalidate.
    }
    if (ownDigest === entry.contextDigest) {
      this.#ownRevokedDigest = entry.contextDigest;
      this.refreshOwnAuthorization();
    } else if (candidateDigest === entry.contextDigest) {
      // A revoked candidate is discarded without touching the active predecessor.
      this.#ownRevokedDigest = entry.contextDigest;
      this.#cache.invalidateCandidate(entry.contextDigest);
      this.#cache.requestRefresh();
    }
  }

  async #issuer(keyId: string): Promise<AuthorizationIssuerKey> {
    if (this.#ownIssuer.keyId === keyId) return this.#ownIssuer;
    for (const entry of this.#contexts.values()) {
      if (!entry.disposed && entry.issuer.keyId === keyId) return entry.issuer;
    }
    const url = new URL(
      `/auth/keys/${encodeURIComponent(keyId)}`,
      this.#cache.trellisUrl,
    );
    if (url.origin !== new URL(this.#cache.trellisUrl).origin) {
      throw new Error("authorization issuer origin is invalid");
    }
    let response: Response;
    try {
      response = await this.#cache.fetch(url, {
        cache: "no-store",
        redirect: "error",
      });
    } catch (error) {
      throw new AuthorizationProviderUnavailableError(
        "authorization issuer is unavailable",
        error,
      );
    }
    if (!response.ok) {
      throw new AuthorizationProviderUnavailableError(
        `authorization issuer is unavailable (${response.status})`,
      );
    }
    let bytes: Uint8Array;
    try {
      bytes = await readBoundedResponse(response, 16_384);
    } catch (error) {
      throw new AuthorizationProviderUnavailableError(
        "authorization issuer response is unavailable",
        error,
      );
    }
    let record: Record<string, unknown>;
    try {
      record = parseRecord(
        JSON.parse(new TextDecoder().decode(bytes)),
        "authorization issuer",
      );
    } catch (error) {
      throw new AuthorizationProviderUnavailableError(
        "authorization issuer response is unavailable",
        error,
      );
    }
    try {
      if (record.keyId !== keyId) {
        throw new Error("authorization issuer key mismatch");
      }
      if (typeof record.publicKey !== "string") {
        throw new Error("authorization issuer public key is invalid");
      }
      assertDigest(keyId);
      assertDigest(record.publicKey);
      if (
        record.state !== "active" && record.state !== "retired" &&
        record.state !== "revoked"
      ) {
        throw new Error("authorization issuer state is invalid");
      }
      return {
        keyId,
        publicKey: record.publicKey,
        state: record.state,
      };
    } catch (error) {
      throw new AuthorizationProviderUnavailableError(
        "authorization issuer response is unavailable",
        error,
      );
    }
  }

  #verified(entry: ProviderContextEntry, historical: boolean) {
    const key = historical ? "historical" : "live";
    let pending = entry[key];
    if (!pending) {
      this.#contextVerifications += 1;
      const created = import("../protocol_wasm.ts").then(
        ({ createAuthorizationContextHandleWasm }) =>
          createAuthorizationContextHandleWasm({
            issuer: entry.issuer,
            context: entry.context,
            policy: this.#policy(this.#now()),
            historical,
          }),
      ).then((verified) => {
        if (entry.resourcesDisposed) {
          verified.handle.free();
          throw new AuthorizationProviderUnavailableError(
            "authorization context lost revocation coverage",
          );
        }
        return verified;
      });
      pending = created.catch((error) => {
        if (entry[key] === pending) entry[key] = undefined;
        throw error;
      });
      entry[key] = pending;
    }
    return pending;
  }

  #policy(nowUnixSeconds: number) {
    try {
      return this.#cache.verificationPolicy(nowUnixSeconds);
    } catch (error) {
      throw new AuthorizationProviderUnavailableError(
        "authorization verification policy is unavailable",
        error,
      );
    }
  }

  #requirePendingOwner(
    contextDigest: string,
    generation: number,
    owner: symbol,
  ): void {
    this.#requireAvailable();
    const pending = this.#inFlight.get(contextDigest);
    if (
      generation !== this.#generation || pending?.generation !== generation ||
      pending.owner !== owner
    ) {
      throw new AuthorizationProviderUnavailableError(
        "authorization context load became stale",
      );
    }
  }

  #requireEntry(entry: ProviderContextEntry): void {
    this.#requireAvailable();
    if (
      entry.generation !== this.#generation || !entry.covered ||
      entry.disposed || this.#contexts.get(entry.contextDigest) !== entry
    ) {
      throw new AuthorizationProviderUnavailableError(
        "authorization context lost revocation coverage",
      );
    }
  }

  #invalidate(entry: ProviderContextEntry): void {
    this.#notifyLiveChanges();
    const wasOwn = this.#ownEntry === entry;
    if (wasOwn) {
      this.#ownEntry = undefined;
      this.#ownUsable = false;
      this.#onOwnInvalidated?.();
      this.#release(entry);
    }
    entry.covered = false;
    entry.disposed = true;
    void entry.closeWatch?.();
    entry.closeWatch = undefined;
    if (this.#contexts.get(entry.contextDigest) === entry) {
      this.#contexts.delete(entry.contextDigest);
    }
    if (entry.leases === 0) this.#disposeResources(entry);
    if (
      wasOwn && entry.revokedAt === undefined && this.#connected &&
      !this.#cache.hasCandidate()
    ) {
      void this.#restoreOwnContext(this.#generation);
    }
  }

  #release(entry: ProviderContextEntry): void {
    entry.leases -= 1;
    if (entry.disposed && entry.leases === 0) this.#disposeResources(entry);
  }

  #disposeResources(entry: ProviderContextEntry): void {
    if (entry.resourcesDisposed) return;
    entry.resourcesDisposed = true;
    for (const pending of [entry.live, entry.historical]) {
      void pending?.then(({ handle }) => handle.free()).catch(() => {});
    }
  }

  async #registryIo<T>(
    message: string,
    operation: () => Promise<T>,
  ): Promise<T> {
    try {
      return await operation();
    } catch (error) {
      if (error instanceof AuthorizationProviderUnavailableError) throw error;
      throw new AuthorizationProviderUnavailableError(message, error);
    }
  }

  #requireAvailable(): void {
    if (!this.#started || this.#stopped || !this.#connected) {
      throw new AuthorizationProviderUnavailableError(
        "authorization provider is unavailable",
      );
    }
  }
}

function parseJsonRecord(value: Uint8Array): Record<string, unknown> {
  return parseRecord(
    JSON.parse(new TextDecoder().decode(value)),
    "authorization context",
  );
}

async function readBoundedResponse(
  response: Response,
  limit: number,
): Promise<Uint8Array> {
  const contentLength = response.headers.get("content-length");
  if (contentLength !== null && Number(contentLength) > limit) {
    throw new InvalidIssuerResponseError(
      "authorization issuer response is too large",
    );
  }
  if (!response.body) return new Uint8Array();
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > limit) {
        throw new InvalidIssuerResponseError(
          "authorization issuer response is too large",
        );
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

function parseRevocation(value: Uint8Array): number {
  try {
    const record = parseJsonRecord(value);
    const revokedAt = record.revokedAt;
    if (!Number.isSafeInteger(revokedAt) || Number(revokedAt) <= 0) {
      throw new Error("authorization revocation is invalid");
    }
    return Number(revokedAt);
  } catch (error) {
    throw new AuthorizationProviderUnavailableError(
      "authorization revocation registry is unavailable",
      error,
    );
  }
}

function parseRecord(value: unknown, kind: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${kind} is invalid`);
  }
  return value as Record<string, unknown>;
}

function assertDigest(value: string): void {
  if (!/^[A-Za-z0-9_-]{43}$/.test(value)) {
    throw new Error("authorization context digest is invalid");
  }
}

function requestFailure(
  code: AuthorizationVerificationErrorCode,
  path: string,
): VerificationFailure {
  return { ok: false, error: { code, path } };
}

function eventFailure(
  code: AuthorizationVerificationErrorCode,
  path: string,
): VerificationFailure {
  return { ok: false, error: { code, path } };
}
