import type { NatsConnection, Subscription } from "@nats-io/nats-core";

import type {
  AuthorizationContextHandle,
  AuthorizationIssuerKey,
  AuthorizationVerificationErrorCode,
  VerifiedAuthorizationContextTokenProjection,
  VerifiedAuthorizationEventPublisher,
} from "../protocol_wasm.ts";
import { canonicalizeJsonValue } from "../utils.ts";
import { trackCoverage } from "../../telemetry/lifecycle.ts";
import type {
  TransportGenerationManager,
  TransportGenerationPrepared,
  TransportLease,
  TransportOwnCoveragePreparation,
  TransportPublishedLease,
} from "../../transport/generations.ts";
import type { AuthorizationContextCache } from "./client_context.ts";
import type {
  AuthorizationCandidateSnapshot,
  AuthorizationRefreshAttempt,
} from "./install_refresh.ts";
import {
  type AuthorizationRegistryIoCounters,
  AuthorizationRegistryReader,
  type RegistryWatchEntry,
} from "./nats_registry.ts";
import type {
  AuthorizationProviderEvent,
  AuthorizationProviderRequest,
  AuthorizationRegistryBinding,
  VerifiedAuthorizationContext,
} from "./types.ts";

const MAX_CACHED_CONTEXTS = 256;

/** Bounded pace for retrying an uncommitted same-target preparation. */
const PROVISIONAL_RETRY_MS = 100;

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

/** Read the hint format tag, ignoring any malformed or forged payload. */
function hintFormat(message: { json<T>(): T }): string | undefined {
  try {
    const value = message.json<{ format?: unknown }>();
    return typeof value?.format === "string" ? value.format : undefined;
  } catch {
    return undefined;
  }
}

/**
 * One authoritative revocation-coverage binding for a cached context digest.
 *
 * The binding owns the exact socket its watch was created on: the reader, the
 * watch iterator, and — for a generation-managed cache — the lease that pins
 * that generation while the watch runs. Cached authority is keyed by digest and
 * outlives a binding, so a physical migration replaces this object in place
 * without recreating the context entry or disturbing its outstanding leases.
 */
export type CoverageBinding = {
  /** Reader bound to the socket this watch runs on. */
  reader: AuthorizationRegistryReader;
  /** Exact published generation pinned by `lease`, when generation-managed. */
  generationId?: number;
  /** Exact attachment lease held while this watch runs, when managed. */
  lease?: TransportLease;
  /** Private preparation owning this watch until successful finish. */
  candidateSignal?: AbortSignal;
  /** A provisional watch that ended can never become authoritative. */
  ended?: boolean;
  /** Initialization consumed by the binding's sole monitor. */
  initialized?: boolean;
  watch: AsyncIterator<RegistryWatchEntry>;
  close: () => Promise<void>;
};

export type ProviderContextEntry = {
  contextDigest: string;
  context: Record<string, unknown>;
  issuer: AuthorizationIssuerKey;
  epoch: number;
  covered: boolean;
  disposed: boolean;
  resourcesDisposed: boolean;
  revokedAt?: number;
  leases: number;
  /** Authoritative coverage binding; replaced in place on physical migration. */
  binding?: CoverageBinding;
  /**
   * Retained coverage for a private own-refresh candidate, pending promotion.
   *
   * A pending candidate is never migrated by the published-cohort reconcile:
   * its coverage is deliberately pinned to a safe existing survivor under the
   * candidate's own policy until promotion installs it as the application's
   * own authorization.
   */
  candidatePending?: boolean;
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

/**
 * Why the caller's own authorization became unusable.
 *
 * `transport_unavailable` is a pure physical-availability signal (the logical
 * transport is down); `coverage_lost` means authority/coverage was withdrawn.
 * They are distinct: an availability loss must not be reported as a revocation.
 */
export type OwnInvalidationReason = "coverage_lost" | "transport_unavailable";

type PendingContextEntry = {
  epoch: number;
  owner: symbol;
  /** Lifecycle-owned abort for this load's pre-entry setup (read/watch). */
  abort: AbortController;
  signal: AbortSignal;
  promise: Promise<ProviderContextEntry>;
};

/**
 * One in-flight make-before-break coverage migration for a cached digest.
 *
 * `committed` is set synchronously inside the manager's publication commit, so
 * a superseding publication can never abort a binding that has already become
 * authoritative.
 */
type ProvisionalBinding = {
  generationId: number;
  abort: AbortController;
  committed: boolean;
};

/**
 * One owned in-flight default-reader/hint alignment for a published generation.
 *
 * A newer publication or a stop cancels it through `abort`, so a superseded
 * alignment cannot wait out the ordinary NATS request timeout, and same-target
 * reconciles share this one attempt instead of fanning out duplicate discovery
 * requests.
 */
type DefaultAlignment = {
  target: number;
  abort: AbortController;
  promise: Promise<void>;
};

/**
 * Explicit request to resolve one context through candidate-safe coverage.
 *
 * Presence switches the load source from exact published current to the one
 * attachment pinned by an original-operation-bound coverage preparation.
 */
type CandidateLoadSource = {
  attempt: AuthorizationRefreshAttempt;
  signal: AbortSignal;
  acquireAttachment: () => TransportPublishedLease | undefined;
  nowSeconds: () => number;
};

type OwnResumeLoadSource = {
  ownResume: true;
  acquireAttachment: () => Promise<TransportPublishedLease | undefined>;
  isCurrent: () => boolean;
  nowSeconds: () => number;
};

type ContextLoadSource = CandidateLoadSource | OwnResumeLoadSource;

/** Connected provider-side authorization verifier. */
export class AuthorizationProviderCache {
  #registry: AuthorizationRegistryReader;
  readonly #cache: AuthorizationContextCache;
  readonly #now: () => number;
  #nats: NatsConnection;
  readonly #binding: AuthorizationRegistryBinding;
  readonly #inboxPrefix: string;
  /** Published-current transport this logical cache follows, once bound. */
  #transport?: TransportGenerationManager;
  #publicationUnsubscribe?: () => void;
  /** In-flight make-before-break bindings, keyed by context digest. */
  readonly #provisionals = new Map<string, ProvisionalBinding>();
  /** Pending bounded retry timers, keyed by context digest. */
  readonly #retryTimers = new Map<string, ReturnType<typeof setTimeout>>();
  #reconcileScheduled = false;
  /** Owned in-flight default-reader alignment, if any. */
  #alignment?: DefaultAlignment;
  /** Generation whose socket currently backs the default reader and hint. */
  #defaultGenerationId?: number;
  #hintSubscription?: Subscription;
  #hintLastTriggeredAt?: number;
  #hintTimer?: ReturnType<typeof setTimeout>;
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
  #cacheEpoch = 0;
  #ownEntry?: ProviderContextEntry;
  /**
   * Retained coverage for a private own-refresh candidate, pending promotion.
   *
   * Kept separate from `#ownEntry` so a candidate failure, watch loss, or
   * revocation can never drop or disable the healthy installed own authority.
   */
  #pendingEntry?: ProviderContextEntry;
  /** Whether `#pendingEntry` belongs to a private refresh candidate. */
  #pendingIsCandidate = false;
  /** Exact verified candidate object `#pendingEntry` was retained for. */
  #pendingAttempt?: AuthorizationRefreshAttempt;
  #pendingNow?: () => number;
  readonly #candidatePreparations = new Map<
    AuthorizationRefreshAttempt,
    TransportOwnCoveragePreparation
  >();
  readonly #candidateAborts = new Set<AbortController>();
  readonly #candidateBindings = new Map<AuthorizationRefreshAttempt, {
    entry: ProviderContextEntry;
    expected: CoverageBinding | undefined;
    binding: CoverageBinding;
  }>();
  #onOwnInvalidated?: (reason: OwnInvalidationReason) => void;
  #onOwnResumed?: () => void;
  #ownRevokedDigest?: string;
  #ownUsable = true;
  #stopCoverage?: () => void;
  readonly #connectedWaiters = new Set<() => void>();
  readonly #liveChangeListeners = new Set<() => void>();

  private constructor(
    nats: NatsConnection,
    registry: AuthorizationRegistryReader,
    cache: AuthorizationContextCache,
    binding: AuthorizationRegistryBinding,
    inboxPrefix: string,
    options: AuthorizationProviderCacheOptions,
  ) {
    this.#nats = nats;
    this.#registry = registry;
    this.#cache = cache;
    this.#binding = binding;
    this.#inboxPrefix = inboxPrefix;
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
      nats,
      await AuthorizationRegistryReader.open(
        nats,
        binding,
        inboxPrefix,
      ),
      cache,
      binding,
      inboxPrefix,
      options,
    );
  }

  /** Enable provider verification. */
  start(): void {
    if (this.#started && !this.#stopped) return;
    this.#cacheEpoch += 1;
    this.#stopped = false;
    this.#started = true;
    this.#startHintSubscription();
    if (this.#transport) this.#subscribePublication();
    this.#stopCoverage = trackCoverage(() => {
      // Cache-lifecycle readiness, matching `health()`: an availability loss of
      // the default/logical transport must not classify continuously covered
      // own/peer authority as uncovered.
      const healthy = this.#started && !this.#stopped;
      const now = this.#now();
      const current = (entry: ProviderContextEntry) =>
        healthy && entry.epoch === this.#cacheEpoch && entry.covered &&
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
    clearTimeout(this.#hintTimer);
    this.#hintTimer = undefined;
    this.#hintLastTriggeredAt = undefined;
    for (const replacement of this.#candidateBindings.values()) {
      void replacement.binding.close().catch(() => undefined);
    }
    this.#candidateBindings.clear();
    for (const abort of this.#candidateAborts) abort.abort();
    for (const preparation of this.#candidatePreparations.values()) {
      void preparation.cancel();
    }
    this.#candidatePreparations.clear();
    this.#stopCoverage?.();
    this.#stopCoverage = undefined;
    this.#publicationUnsubscribe?.();
    this.#publicationUnsubscribe = undefined;
    for (const timer of this.#retryTimers.values()) clearTimeout(timer);
    this.#retryTimers.clear();
    for (const provisional of this.#provisionals.values()) {
      provisional.abort.abort();
    }
    this.#provisionals.clear();
    this.#alignment?.abort.abort();
    this.#alignment = undefined;
    this.#defaultGenerationId = undefined;
    void this.#hintSubscription?.unsubscribe();
    this.#hintSubscription = undefined;
    this.#cacheEpoch += 1;
    this.#stopped = true;
    for (const pending of this.#inFlight.values()) pending.abort.abort();
    for (const entry of this.#contexts.values()) this.#invalidate(entry);
    this.#contexts.clear();
    this.#inFlight.clear();
    if (this.#pendingEntry) this.#release(this.#pendingEntry);
    this.#pendingEntry = undefined;
    this.#pendingIsCandidate = false;
    this.#pendingAttempt = undefined;
    if (this.#ownEntry) this.#release(this.#ownEntry);
    this.#ownEntry = undefined;
    this.#notifyLiveChanges();
  }

  /**
   * Follow the published current transport generation of a logical connection.
   *
   * Called once, after the manager has adopted its initial generation and
   * before the public connection is returned. The retained publication is
   * delivered synchronously at registration, so the first reconcile that pins
   * existing fixed-mode coverage to that exact generation cannot be missed.
   *
   * Every retained watched digest — including an idle one — migrates
   * make-before-break: a replacement watch is driven to its initial snapshot on
   * the exact published generation while the previous binding stays
   * authoritative, and only a successful publication commit swaps the binding
   * and releases the previous generation's lease. Absence of a published
   * current is never treated as loss of healthy coverage.
   *
   * A cache that never receives this call stays in fixed single-attachment mode,
   * so a native single-socket device is unaffected. A second call is ignored.
   * @internal
   */
  followTransport(transport: TransportGenerationManager): void {
    if (this.#transport) return;
    this.#transport = transport;
    this.#subscribePublication();
  }

  #subscribePublication(): void {
    const transport = this.#transport;
    if (!transport || this.#publicationUnsubscribe) return;
    this.#publicationUnsubscribe = transport.subscribePublication((id) => {
      this.#onPublication(id);
    });
  }

  /** React to one retained published-current identity change. */
  #onPublication(generationId: number | undefined): void {
    // Supersede any in-flight preparation that targeted a different identity
    // (including a cleared publication). A committed binding is never aborted.
    for (const provisional of this.#provisionals.values()) {
      if (
        provisional.generationId !== generationId && !provisional.committed
      ) {
        provisional.abort.abort();
      }
    }
    // A superseded or cleared publication cancels an in-flight default
    // alignment for the old identity; a fresh reconcile will own a new attempt.
    if (this.#alignment && this.#alignment.target !== generationId) {
      this.#alignment.abort.abort();
    }
    if (generationId === undefined) return;
    this.#scheduleReconcile();
  }

  /**
   * Coalesce publication-driven reconciles into one immediate microtask.
   *
   * Reconciliation is scheduled, never awaited from the publication callback,
   * so a slow candidate setup cannot stall the old watch or a following
   * publication.
   */
  #scheduleReconcile(): void {
    if (this.#reconcileScheduled || this.#stopped || !this.#transport) return;
    this.#reconcileScheduled = true;
    queueMicrotask(() => {
      this.#reconcileScheduled = false;
      this.#reconcile();
    });
  }

  #reconcile(): void {
    const transport = this.#transport;
    if (!transport || this.#stopped) return;
    const target = transport.publishedGenerationId();
    if (target === undefined) return;
    // Alignment is best-effort and must not gate candidate preparation.
    void this.#alignDefault(target).catch(() => undefined);
    for (const entry of [...this.#contexts.values()]) {
      if (!entry.covered || entry.disposed) continue;
      if (entry.epoch !== this.#cacheEpoch) continue;
      // A private candidate's coverage is pinned to a safe existing survivor
      // under the candidate's own policy; the published cohort must not migrate
      // it (or classify it) until promotion installs it as application-current.
      if (entry.candidatePending) continue;
      if (entry.binding?.generationId === target) continue;
      if (this.#provisionals.has(entry.contextDigest)) continue;
      this.#spawnProvisional(entry, target);
    }
  }

  /**
   * Move the default reader and hint subscription onto the published current
   * socket. This is a reader/routing concern only; cached authority bindings
   * migrate independently. The short lease used to observe the socket is
   * released immediately, so the default reader never pins a generation.
   *
   * Alignment is coalesced per published target: concurrent reconciles share
   * one attempt instead of fanning out duplicate discovery requests, and a
   * superseding publication or a stop cancels an in-flight attempt through its
   * own controller.
   */
  #alignDefault(target: number): Promise<void> {
    const current = this.#alignment;
    if (current?.target === target && !current.abort.signal.aborted) {
      return current.promise;
    }
    if (this.#defaultGenerationId === target && !this.#nats.isClosed()) {
      return Promise.resolve();
    }
    current?.abort.abort();
    const alignment: DefaultAlignment = {
      target,
      abort: new AbortController(),
      promise: Promise.resolve(),
    };
    alignment.promise = this.#runAlignDefault(target, alignment).finally(() => {
      if (this.#alignment === alignment) this.#alignment = undefined;
    });
    this.#alignment = alignment;
    return alignment.promise;
  }

  async #runAlignDefault(
    target: number,
    alignment: DefaultAlignment,
  ): Promise<void> {
    const transport = this.#transport;
    if (!transport) return;
    if (this.#defaultGenerationId === target && !this.#nats.isClosed()) return;
    const epoch = this.#cacheEpoch;
    const signal = alignment.abort.signal;
    const lease = await transport.acquirePublishedAttachment();
    if (!lease) return;
    try {
      if (lease.generationId !== target || signal.aborted) return;
      if (lease.nc === this.#nats) {
        // The published socket is already the default; adopt it under the same
        // publication fence and cache-lifecycle epoch guard, without opening a
        // reader.
        await transport.commitOnPublished(target, () => {
          if (this.#stopped || epoch !== this.#cacheEpoch) return false;
          if (this.#defaultGenerationId === target) return false;
          this.#defaultGenerationId = target;
          return true;
        });
        return;
      }
      const registry = await AuthorizationRegistryReader.open(
        lease.nc,
        this.#binding,
        this.#inboxPrefix,
        signal,
      );
      // Synchronous final fence: only the exact still-published target may
      // replace the default socket and hint, so a superseded alignment can
      // never point them at a retired socket. The opened reader owns no
      // teardown of its own, and the short observation lease is released below.
      await transport.commitOnPublished(target, () => {
        if (this.#stopped || epoch !== this.#cacheEpoch) return false;
        if (this.#defaultGenerationId === target) return false;
        const previousHint = this.#hintSubscription;
        this.#nats = lease.nc;
        this.#registry = registry;
        this.#defaultGenerationId = target;
        this.#hintSubscription = undefined;
        void previousHint?.unsubscribe();
        this.#startHintSubscription();
        return true;
      });
    } finally {
      lease.release();
    }
  }

  /** Start one independently preemptible provisional binding for `entry`. */
  #spawnProvisional(entry: ProviderContextEntry, target: number): void {
    const digest = entry.contextDigest;
    const pending = this.#retryTimers.get(digest);
    if (pending !== undefined) {
      clearTimeout(pending);
      this.#retryTimers.delete(digest);
    }
    const provisional: ProvisionalBinding = {
      generationId: target,
      abort: new AbortController(),
      committed: false,
    };
    this.#provisionals.set(digest, provisional);
    const expected = entry.binding;
    void this.#runProvisional(entry, expected, target, provisional).finally(
      () => {
        if (this.#provisionals.get(digest) === provisional) {
          this.#provisionals.delete(digest);
        }
        // Re-evaluate the retained publication once this attempt settles. A
        // superseded or aborted attempt must not leave the newer retained
        // publication unfollowed, because the cohort reconcile skipped this
        // entry while the attempt's token was still present. A same-target
        // failure keeps its own bounded retry, and a stop or a cleared
        // publication is not a coverage target (`#reconcile` re-checks both),
        // so this never becomes an immediate retry loop.
        const transport = this.#transport;
        if (
          !this.#stopped && transport &&
          (provisional.abort.signal.aborted ||
            transport.publishedGenerationId() !== provisional.generationId)
        ) {
          this.#scheduleReconcile();
        }
      },
    );
  }

  /**
   * Prepare and, if still current, commit one replacement coverage binding.
   *
   * The previous binding stays authoritative throughout preparation: an aborted
   * or failed candidate releases only its own exact lease and closes only its
   * own watch. A digest-global revocation observed in the candidate's initial
   * snapshot is applied to the entry before any commit decision, while an
   * ordinary candidate error is local to that candidate.
   */
  async #runProvisional(
    entry: ProviderContextEntry,
    expected: CoverageBinding | undefined,
    target: number,
    provisional: ProvisionalBinding,
  ): Promise<void> {
    const transport = this.#transport;
    if (!transport) return;
    const digest = entry.contextDigest;
    const signal = provisional.abort.signal;
    let lease: TransportPublishedLease | undefined;
    let candidate:
      | Awaited<ReturnType<AuthorizationRegistryReader["watchRevocation"]>>
      | undefined;
    let handedOff = false;
    let failed = false;
    try {
      lease = await transport.acquirePublishedAttachment();
      if (!lease) {
        failed = true;
        return;
      }
      if (lease.generationId !== target || signal.aborted) return;
      const reader = await this.#readerFor(lease.nc, signal);
      if (signal.aborted) return;
      candidate = await reader.watchRevocation(digest, signal);
      const committedLease = lease;
      const committedCandidate = candidate;
      const binding: CoverageBinding = {
        reader,
        generationId: target,
        lease: committedLease,
        candidateSignal: signal,
        initialized: false,
        ended: false,
        watch: committedCandidate.iterator,
        close: async () => {
          try {
            await committedCandidate.close();
          } finally {
            committedLease.release();
          }
        },
      };
      // One monitor owns every read, including initialization, and keeps
      // consuming while publication awaits its policy-safety check.
      const initialized = new Promise<void>((resolve) => {
        void this.#watchRevocation(entry, binding, resolve);
      });
      await initialized;
      const committed = await transport.commitOnPublished(target, () => {
        // Synchronous final fence: lifecycle incarnation, exact cached entry,
        // and the exact authoritative binding we planned to replace.
        if (this.#stopped || entry.epoch !== this.#cacheEpoch) return false;
        if (this.#contexts.get(digest) !== entry) return false;
        if (entry.disposed || !entry.covered) return false;
        if (entry.revokedAt !== undefined) return false;
        if (entry.binding !== expected) return false;
        if (
          !binding.initialized || binding.ended || signal.aborted ||
          !committedCandidate.usable()
        ) return false;
        entry.binding = binding;
        // Mark committed synchronously, before any publication can interleave,
        // so a superseding publication never aborts the winning binding.
        provisional.committed = true;
        handedOff = true;
        return true;
      });
      if (!committed) {
        failed = true;
        return;
      }
      if (this.#provisionals.get(digest) === provisional) {
        this.#provisionals.delete(digest);
      }
      if (expected) {
        // Release the previous generation's exact pin immediately so a
        // draining generation can reap; tear the old watch down in the
        // background (its close idempotently repeats this release).
        expected.lease?.release();
        void expected.close().catch(() => undefined);
      }
      this.#notifyLiveChanges();
    } catch {
      failed = true;
    } finally {
      if (!handedOff) {
        try {
          await candidate?.close();
        } catch {
          // The candidate is provisional; the old coverage is authoritative.
        }
        lease?.release();
        if (failed && !signal.aborted && !this.#stopped) {
          this.#scheduleProvisionalRetry(entry, target);
        }
      }
    }
  }

  /**
   * Retry an uncommitted same-target preparation at a bounded pace, even when
   * no further publication arrives. A changed publication supersedes the retry
   * through the normal abort path.
   */
  #scheduleProvisionalRetry(
    entry: ProviderContextEntry,
    target: number,
  ): void {
    const digest = entry.contextDigest;
    if (this.#retryTimers.has(digest)) return;
    const timer = setTimeout(() => {
      this.#retryTimers.delete(digest);
      const transport = this.#transport;
      if (!transport || this.#stopped) return;
      if (transport.publishedGenerationId() !== target) return;
      if (entry.disposed || !entry.covered) return;
      if (entry.revokedAt !== undefined) return;
      if (entry.epoch !== this.#cacheEpoch) return;
      if (this.#contexts.get(digest) !== entry) return;
      if (entry.binding?.generationId === target) return;
      if (this.#provisionals.has(digest)) return;
      this.#spawnProvisional(entry, target);
    }, PROVISIONAL_RETRY_MS);
    this.#retryTimers.set(digest, timer);
  }

  /** Open (or reuse) a registry reader bound to exactly `nc`. */
  async #readerFor(
    nc: NatsConnection,
    signal?: AbortSignal,
  ): Promise<AuthorizationRegistryReader> {
    if (nc === this.#nats) return this.#registry;
    return await AuthorizationRegistryReader.open(
      nc,
      this.#binding,
      this.#inboxPrefix,
      signal,
    );
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
    epoch: number,
  ): Promise<LiveLeaseView> {
    const entry = await this.#lease(contextDigest, false);
    try {
      this.#requireEntry(entry);
      if (entry.epoch !== epoch || epoch !== this.#cacheEpoch) {
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
      entry.epoch !== this.#cacheEpoch || !this.#started ||
      this.#stopped
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
    // A renewal can install a newer own context while coverage retention is in
    // flight. Coverage must follow the newest installed context, so re-read the
    // digest and epoch and retry a bounded number of times instead of
    // treating a routine in-place renewal as a fatal resumption failure.
    for (let attempt = 0; attempt < 3; attempt++) {
      const digest = this.#cache.storedContextDigest();
      if (digest === undefined) {
        throw new Error("authorization context is unavailable");
      }
      const epoch = this.#cacheEpoch;
      const origin = this.#cache.current();
      const offset = this.#cache.serverClockOffsetMs();
      const transport = this.#transport;
      let attachment: TransportPublishedLease | undefined;
      const nowSeconds = () =>
        Math.floor((this.#cache.nowMilliseconds() + offset) / 1_000);
      const isCurrent = () => {
        if (
          !this.#started || this.#stopped || this.#cacheEpoch !== epoch ||
          this.#cache.hasCandidate() ||
          this.#ownRevokedDigest === digest ||
          this.#cache.serverClockOffsetMs() !== offset ||
          (attachment && (
            transport?.publishedGenerationId() !== attachment.generationId ||
            transport.currentGeneration()?.nc !== attachment.nc ||
            !transport.currentGeneration()?.ready || attachment.nc.isClosed()
          ))
        ) return false;
        try {
          return this.#cache.current(nowSeconds()) === origin;
        } catch {
          return false;
        }
      };
      await this.#retainOwnContext(
        digest,
        epoch,
        transport
          ? {
            ownResume: true,
            nowSeconds,
            isCurrent,
            acquireAttachment: async () => {
              attachment = await transport.acquireOwnRegistryMaintenance(
                origin.context.transportAuthorization,
                isCurrent,
              );
              return attachment;
            },
          }
          : undefined,
      );
      if (
        isCurrent() && this.#finalizeOwnInstallation("resume", digest, epoch)
      ) {
        this.#transport?.authorizationPromoted();
        this.#notifyLiveChanges();
        return;
      }
      if (!this.#pendingIsCandidate) this.#dropPending();
      this.#cache.requestRefresh();
    }
    throw new Error(
      "authorization context coverage changed during resumption",
    );
  }

  /**
   * Retain candidate coverage on one exact prepared attachment.
   *
   * The candidate is classified under its own fresh policy and pinned to a safe
   * existing survivor, or a private admitted socket when none remains.
   * Coverage is kept in candidate-owned pending state,
   * never swapped into the application's own installation, so promotion alone
   * publishes it. @internal
   */
  async retainOwnCandidate(
    attempt: AuthorizationRefreshAttempt,
    prepareConnect?: (
      snapshot: AuthorizationCandidateSnapshot,
    ) => Promise<TransportGenerationPrepared>,
  ): Promise<TransportOwnCoveragePreparation | undefined> {
    const { origin: candidate, cacheEpoch: epoch } = attempt;
    const digest = candidate.contextDigest;
    this.#requireRunning();
    if (epoch !== this.#cacheEpoch) {
      throw new AuthorizationProviderUnavailableError(
        "authorization candidate coverage changed before retention",
      );
    }
    const snapshot = this.#cache.candidateSnapshot(candidate);
    if (digest === this.#ownRevokedDigest) {
      throw new AuthorizationProviderUnavailableError(
        "authorization candidate is revoked",
      );
    }
    const nowSeconds = () =>
      Math.floor(
        (this.#cache.nowMilliseconds() + snapshot.clockOffsetMs) / 1_000,
      );
    this.#requireCandidateWindow(candidate, nowSeconds());
    this.#pendingAttempt = attempt;
    this.#pendingIsCandidate = true;
    this.#pendingNow = nowSeconds;
    const abort = new AbortController();
    this.#candidateAborts.add(abort);
    let preparation: TransportOwnCoveragePreparation | undefined;
    let entry: ProviderContextEntry | undefined;
    try {
      if (this.#transport) {
        preparation = await this.#transport.prepareOwnCoverage({
          contextDigest: digest,
          policy: candidate.context.transportAuthorization,
          notBefore: candidate.context.notBefore,
          expiresAt: candidate.context.expiresAt,
          routingJwtExpiresAt: snapshot.routing.bootstrapJwtExpiresAt,
          nowSeconds,
          isCurrent: () =>
            snapshot.isCurrent() && this.#pendingAttempt === attempt &&
            epoch === this.#cacheEpoch && !this.#stopped,
        }, () => {
          if (!prepareConnect) {
            throw new Error(
              "authorization candidate CONNECT preparation is unavailable",
            );
          }
          return prepareConnect(snapshot);
        }, { signal: abort.signal });
        this.#candidatePreparations.set(attempt, preparation);
      }
      entry = await this.#lease(
        digest,
        false,
        preparation
          ? {
            attempt,
            signal: preparation.signal,
            acquireAttachment: preparation.acquireAttachment,
            nowSeconds,
          }
          : undefined,
      );
      this.#requireEntry(entry);
      if (
        !snapshot.isCurrent() || this.#pendingAttempt !== attempt ||
        epoch !== this.#cacheEpoch || entry.revokedAt !== undefined ||
        digest === this.#ownRevokedDigest
      ) {
        throw new AuthorizationProviderUnavailableError(
          "authorization candidate changed during retention",
        );
      }
      this.#requireCandidateWindow(candidate, nowSeconds());
      const previous = this.#pendingEntry;
      this.#pendingEntry = entry;
      entry.candidatePending = true;
      if (previous === entry) this.#release(entry);
      else if (previous) this.#dropRetained(previous);
      return preparation;
    } catch (error) {
      if (entry) this.#dropRetained(entry);
      const replacement = this.#candidateBindings.get(attempt);
      this.#candidateBindings.delete(attempt);
      await replacement?.binding.close().catch(() => undefined);
      await preparation?.cancel();
      this.#candidatePreparations.delete(attempt);
      throw error;
    } finally {
      this.#candidateAborts.delete(abort);
    }
  }

  /** Promote the candidate only while its admitted coverage remains current. */
  promoteOwnCandidate(attempt: AuthorizationRefreshAttempt): void {
    if (
      !this.#finalizeOwnInstallation(
        "promote",
        attempt.origin.contextDigest,
        attempt.cacheEpoch,
        attempt,
      )
    ) {
      throw new Error(
        "authorization candidate coverage changed before promotion",
      );
    }
  }

  /** Require the exact candidate's signed validity window at the corrected clock. */
  #requireCandidateWindow(
    candidate: VerifiedAuthorizationContext,
    now: number,
  ): void {
    if (
      candidate.context.notBefore > now || candidate.context.expiresAt <= now
    ) {
      throw new AuthorizationProviderUnavailableError(
        "authorization candidate is not currently valid",
      );
    }
  }

  /**
   * One synchronous final own-installation publication boundary.
   *
   * Rechecks the exact retained pending entry identity, epoch, coverage,
   * revocation, and the exact private candidate object immediately before
   * publishing, then commits the pending retention as the application's own
   * entry and releases the healthy predecessor only after the commit.
   */
  #finalizeOwnInstallation(
    mode: "promote" | "resume",
    digest: string,
    epoch: number,
    attempt?: AuthorizationRefreshAttempt,
  ): boolean {
    if (!this.#started || this.#stopped) return false;
    if (epoch !== this.#cacheEpoch) return false;
    const entry = this.#pendingEntry;
    if (
      !entry || entry.contextDigest !== digest || !entry.covered ||
      entry.disposed || entry.revokedAt !== undefined ||
      entry.epoch !== epoch || this.#contexts.get(digest) !== entry ||
      this.#ownRevokedDigest === digest
    ) {
      return false;
    }
    let verified: VerifiedAuthorizationContext;
    if (mode === "promote") {
      if (!attempt || this.#pendingAttempt !== attempt) return false;
      const candidate = attempt.origin;
      if (
        !this.#cache.candidateSnapshot(candidate).isCurrent()
      ) {
        return false;
      }
      verified = candidate;
    } else {
      if (this.#pendingIsCandidate || this.#cache.hasCandidate()) return false;
      if (this.#cache.storedContextDigest() !== digest) return false;
      try {
        verified = this.#cache.current();
      } catch {
        return false;
      }
    }
    const now = mode === "promote" ? this.#pendingNow!() : this.#now();
    if (verified.context.notBefore > now || verified.context.expiresAt <= now) {
      return false;
    }
    const replacement = attempt
      ? this.#candidateBindings.get(attempt)
      : undefined;
    if (
      replacement &&
      (replacement.entry !== entry || entry.binding !== replacement.expected ||
        replacement.binding.candidateSignal?.aborted ||
        replacement.binding.ended)
    ) return false;
    if (mode === "promote") this.#cache.promote(verified);
    if (replacement) {
      entry.binding = replacement.binding;
      this.#candidateBindings.delete(attempt!);
      replacement.expected?.lease?.release();
      void replacement.expected?.close().catch(() => undefined);
    }
    const previous = this.#ownEntry;
    this.#ownEntry = entry;
    this.#pendingEntry = undefined;
    this.#pendingIsCandidate = false;
    this.#pendingAttempt = undefined;
    this.#pendingNow = undefined;
    if (attempt) this.#candidatePreparations.delete(attempt);
    // The promoted candidate is now application-current and may be migrated by
    // the published cohort like any other own entry.
    entry.candidatePending = false;
    if (entry.binding) entry.binding.candidateSignal = undefined;
    if (previous && previous !== entry) this.#dropRetained(previous);
    else if (previous === entry) {
      // The pending retention and the installed own entry are the same digest;
      // collapse the pending lease into the single own lease.
      this.#release(entry);
    }
    this.#ownUsable = true;
    this.#onOwnResumed?.();
    this.#notifyLiveChanges();
    return true;
  }

  /** Current cache epoch: changes only on verifier stop/reset. @internal */
  cacheEpoch(): number {
    return this.#cacheEpoch;
  }

  /** Return the installed own-context digest, if any. @internal */
  currentLocalContextDigest(): string | undefined {
    return this.#cache.storedContextDigest();
  }

  /**
   * Drop an unadmitted prepared candidate after a failed in-place renewal.
   *
   * Only candidate-owned coverage/leases are dropped; a healthy installed own
   * authorization and any ordinary borrowed peer entry are never touched.
   * @internal
   */
  releaseCandidate(attempt: AuthorizationRefreshAttempt): void {
    const replacement = this.#candidateBindings.get(attempt);
    this.#candidateBindings.delete(attempt);
    void replacement?.binding.close().catch(() => undefined);
    void this.#candidatePreparations.get(attempt)?.cancel();
    this.#candidatePreparations.delete(attempt);
    if (this.#pendingAttempt === attempt) this.#dropPending();
    if (this.#pendingAttempt?.origin !== attempt.origin) {
      this.#cache.invalidateCandidate(attempt.origin);
    }
    this.#notifyLiveChanges();
  }

  /**
   * Subscribe to the server-issued best-effort authorization-change hint.
   *
   * A hint only schedules a normal signed refresh: it cannot install grants,
   * change the admitted policy, suppress revocation, or initiate reconnect. It
   * is coalesced to at most one trigger per monotonic second, with one trailing
   * notification retained across physical subscription migration.
   */
  #startHintSubscription(): void {
    if (this.#hintSubscription) return;
    let subscription: Subscription;
    try {
      subscription = this.#nats.subscribe(
        `${this.#inboxPrefix}._trellis.authorization`,
      );
    } catch {
      return;
    }
    this.#hintSubscription = subscription;
    const epoch = this.#cacheEpoch;
    void (async () => {
      for await (const message of subscription) {
        if (
          this.#stopped || this.#cacheEpoch !== epoch ||
          this.#hintSubscription !== subscription
        ) return;
        if (hintFormat(message) !== "trellis.authorization-change.v1") continue;
        if (this.#hintTimer !== undefined) continue;
        const trigger = () => {
          if (this.#stopped || this.#cacheEpoch !== epoch) return;
          const now = performance.now();
          const remaining = this.#hintLastTriggeredAt === undefined
            ? 0
            : 1_000 - (now - this.#hintLastTriggeredAt);
          if (remaining > 0) {
            this.#hintTimer = setTimeout(trigger, Math.ceil(remaining));
            return;
          }
          this.#hintTimer = undefined;
          this.#hintLastTriggeredAt = now;
          this.#cache.requestRefresh();
        };
        trigger();
      }
    })().catch(() => undefined);
  }

  async #retainOwnContext(
    digest: string,
    epoch: number,
    source?: OwnResumeLoadSource,
  ): Promise<void> {
    const entry = await this.#lease(digest, false, source);
    if (
      entry.revokedAt !== undefined || !entry.covered ||
      entry.epoch !== epoch || epoch !== this.#cacheEpoch ||
      (source && !source.isCurrent())
    ) {
      this.#dropRetained(entry);
      throw new Error(
        "authorization context coverage changed during installation",
      );
    }
    const previous = this.#pendingEntry;
    this.#pendingIsCandidate = false;
    this.#pendingAttempt = undefined;
    if (previous === entry) {
      // The same retained entry already backs the pending installation; keep a
      // single pending lease instead of double-holding it.
      this.#release(entry);
      return;
    }
    this.#pendingEntry = entry;
    if (previous) this.#dropRetained(previous);
  }

  /** Release candidate-owned pending coverage without touching `#ownEntry`. */
  #dropPending(): void {
    const pending = this.#pendingEntry;
    this.#pendingEntry = undefined;
    this.#pendingIsCandidate = false;
    this.#pendingAttempt = undefined;
    if (pending) this.#dropRetained(pending);
  }

  /**
   * Release exactly one retained lease and, when it is the last owner of a
   * non-own entry, drop that entry's coverage. A shared borrowed entry and the
   * installed own entry are left authoritative.
   */
  #dropRetained(entry: ProviderContextEntry): void {
    if (
      this.#pendingEntry !== entry &&
      this.#pendingAttempt?.origin.contextDigest !== entry.contextDigest
    ) entry.candidatePending = false;
    if (this.#ownEntry === entry) {
      this.#release(entry);
      return;
    }
    this.#release(entry);
    if (
      entry.leases <= 0 && this.#contexts.get(entry.contextDigest) === entry
    ) {
      this.#invalidate(entry);
    }
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
      this.#onOwnInvalidated?.("coverage_lost");
    }
    this.#cache.requestRefresh();
    this.#notifyLiveChanges();
  }

  /**
   * Apply one transport availability event to the availability projection.
   *
   * For generation-managed clients and services this stream carries only
   * logical connection events (a planned rollover, or the loss of a superseded
   * socket, emits nothing); for a fixed single-socket device it carries that
   * socket's physical events. Neither invalidates cached authority: a digest's
   * verification coverage belongs to its own watch binding, not to the default
   * socket. Managed disconnection also requests the existing staged refresh
   * path, so loss of the last carrier can recover without waiting for renewal.
   */
  observeTransportEvent(event: unknown): void {
    if (!event || typeof event !== "object") return;
    const status = event as { type?: unknown; data?: unknown };
    switch (status.type) {
      case "disconnect":
      case "disconnected":
      case "reconnecting":
      case "forceReconnect":
        this.#observePhysicalConnected(false);
        break;
      case "reconnect":
        this.#observePhysicalConnected(true);
        break;
    }
    if (
      status.type === "error" &&
      String(status.data).toLowerCase().includes("authorization")
    ) {
      this.refreshOwnAuthorization();
    }
  }

  #observePhysicalConnected(connected: boolean): void {
    const wasConnected = this.#connected;
    this.#connected = connected;
    if (wasConnected && !this.#connected) {
      // Transport availability is not an authority event: do not reset the
      // cache epoch, invalidate entries, or suspend owned authority. Each
      // entry's revocation watch is bound to one exact attachment and its
      // finalizer invalidates exactly that entry on real loss (fail-closed).
      // Withdraw only the availability projection, not verified authority.
      this.#onOwnInvalidated?.("transport_unavailable");
      this.#notifyLiveChanges();
      if (this.#transport && this.#started && !this.#stopped) {
        // If that socket's watch also fails, coverage-only repair cannot use
        // the closed published attachment. The refresh owner can instead warm
        // a private replacement before restoring authority and publication.
        this.#cache.requestRefresh();
      }
    }
    if (!wasConnected && this.#connected) {
      for (const ready of this.#connectedWaiters) ready();
      this.#connectedWaiters.clear();
      this.#notifyLiveChanges();
      if (this.#ownUsable) {
        // Owned authority was never suspended by the availability loss:
        // restore its existing availability projection instead of rebuilding
        // coverage or creating a new watch.
        this.#onOwnResumed?.();
      } else if (!this.#cache.hasCandidate()) {
        const epoch = this.#cacheEpoch;
        void this.#restoreOwnContext(epoch);
      }
    }
  }

  /** Register the connection-owned usability withdrawal for own revocation. */
  onOwnInvalidated(callback: (reason: OwnInvalidationReason) => void): void {
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

  /**
   * Return current provider cache-lifecycle health.
   *
   * `healthy` reports verifier lifecycle readiness (started, not stopped), not
   * current socket availability: a logical or default transport being
   * unavailable does not make already-covered cached authority unusable.
   */
  health(): AuthorizationProviderCacheHealth {
    return {
      revocationRevision: this.#revocationRevision,
      lastUpdateAt: this.#lastUpdateAt,
      healthy: this.#started && !this.#stopped,
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

  async #restoreOwnContext(epoch: number): Promise<void> {
    while (
      !this.#stopped && this.#connected && this.#cacheEpoch === epoch
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
    source?: ContextLoadSource,
  ): Promise<ProviderContextEntry> {
    assertDigest(contextDigest);
    this.#requireRunning();
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
      const epoch = this.#cacheEpoch;
      const owner = Symbol(contextDigest);
      // One lifecycle-owned abort per pending load: concurrent callers share
      // this entry and its controller, so only a stop or verifier reset
      // cancels the pre-entry setup of a coalesced load.
      const abort = new AbortController();
      const signal = source && !("ownResume" in source)
        ? AbortSignal.any([abort.signal, source.signal])
        : abort.signal;
      pending = {
        epoch,
        owner,
        abort,
        signal,
        promise: this.#load(
          contextDigest,
          historical,
          epoch,
          owner,
          signal,
          source,
        ),
      };
      this.#inFlight.set(contextDigest, pending);
    }
    try {
      return await pending.promise;
    } catch (error) {
      // A successor sharing the digest must not inherit its abandoned origin's
      // canceled cold load. It warms through its own exact preparation instead.
      if (
        source && !("ownResume" in source) && !source.signal.aborted &&
        pending.signal.aborted &&
        !this.#stopped && pending.epoch === this.#cacheEpoch
      ) {
        if (this.#inFlight.get(contextDigest) === pending) {
          this.#inFlight.delete(contextDigest);
        }
        return await this.#entry(contextDigest, historical, source);
      }
      throw error;
    } finally {
      if (this.#inFlight.get(contextDigest) === pending) {
        this.#inFlight.delete(contextDigest);
      }
    }
  }

  async #lease(
    contextDigest: string,
    historical: boolean,
    source?: ContextLoadSource,
  ): Promise<ProviderContextEntry> {
    const entry = await this.#entry(contextDigest, historical, source);
    this.#requireEntry(entry);
    if (source && "ownResume" in source) {
      // A shared entry must already cover this exact publication; resumption
      // never substitutes its binding or borrows a candidate's private socket.
      entry.leases += 1;
      let attachment: TransportPublishedLease | undefined;
      try {
        attachment = await source.acquireAttachment();
        this.#requireEntry(entry);
        if (
          !attachment || !source.isCurrent() ||
          entry.binding?.generationId !== attachment.generationId ||
          entry.binding.lease?.nc !== attachment.nc
        ) {
          throw new AuthorizationProviderUnavailableError(
            "installed own coverage attachment changed",
          );
        }
      } catch (error) {
        this.#dropRetained(entry);
        throw error;
      } finally {
        attachment?.release();
      }
    }
    if (source && !("ownResume" in source)) {
      const lease = source.acquireAttachment();
      if (!lease || source.signal.aborted) {
        lease?.release();
        throw new AuthorizationProviderUnavailableError(
          "authorization candidate attachment changed",
        );
      }
      const expected = entry.binding;
      // An ordinary watch on the selected socket is already durable. A watch
      // owned by another private attempt must be replaced before that attempt's
      // cancellation can withdraw coverage from this shared entry.
      if (
        expected?.lease?.nc === lease.nc &&
        (!expected.candidateSignal ||
          expected.candidateSignal === source.signal)
      ) {
        lease.release();
      } else {
        let watch:
          | Awaited<ReturnType<AuthorizationRegistryReader["watchRevocation"]>>
          | undefined;
        let committed = false;
        const abort = new AbortController();
        this.#candidateAborts.add(abort);
        const signal = AbortSignal.any([source.signal, abort.signal]);
        try {
          const reader = await this.#readerFor(lease.nc, signal);
          watch = await reader.watchRevocation(contextDigest, signal);
          while (true) {
            const next = await watch.iterator.next();
            if (next.done) {
              throw new AuthorizationProviderUnavailableError(
                "authorization candidate watch ended",
              );
            }
            if (next.value.operation === "initialized") break;
            this.#applyRevocation(entry, next.value);
          }
          this.#requireEntry(entry);
          if (
            signal.aborted || entry.binding !== expected ||
            entry.revokedAt !== undefined
          ) {
            throw new AuthorizationProviderUnavailableError(
              "authorization candidate coverage changed",
            );
          }
          const winningWatch = watch;
          const binding: CoverageBinding = {
            reader,
            lease,
            generationId: lease.generationId,
            candidateSignal: source.signal,
            watch: watch.iterator,
            close: async () => {
              try {
                await winningWatch.close();
              } finally {
                lease.release();
              }
            },
          };
          this.#candidateBindings.set(source.attempt, {
            entry,
            expected,
            binding,
          });
          committed = true;
          void this.#watchRevocation(entry, binding);
        } finally {
          this.#candidateAborts.delete(abort);
          if (!committed) {
            lease.release();
            await watch?.close().catch(() => undefined);
          }
        }
      }
    }
    if (!source || !("ownResume" in source)) entry.leases += 1;
    // Mark before any reconcile microtask can observe the entry, so a private
    // candidate's coverage is never migrated under the installed policy while
    // it is pending promotion.
    if (source && !("ownResume" in source)) entry.candidatePending = true;
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

  /**
   * Resolve the exact socket (and managed lease) a cold load reads from.
   *
   * A generation-managed cache reads from the exact safe published generation
   * and keeps the lease for the watch's lifetime; a fixed single-attachment
   * cache reads from its current reader. A managed cache with no safe published
   * current fails closed rather than falling back to a draining attachment.
   *
   * When `source` is supplied the load uses only the original preparation's
   * exact selected attachment. Ordinary loads never use a candidate or draining
   * fallback.
   */
  async #acquireLoadSource(
    signal?: AbortSignal,
    source?: ContextLoadSource,
  ): Promise<{
    reader: AuthorizationRegistryReader;
    lease?: TransportLease;
    generationId?: number;
  }> {
    const transport = this.#transport;
    if (!transport) return { reader: this.#registry };
    const attachment = source
      ? await source.acquireAttachment()
      : await transport.acquirePublishedAttachment();
    if (!attachment) {
      throw new AuthorizationProviderUnavailableError(
        source
          ? "authorization candidate coverage is unavailable"
          : "authorization attachment is unavailable",
      );
    }
    try {
      const reader = await this.#readerFor(attachment.nc, signal);
      if (
        signal?.aborted ||
        (source && "ownResume" in source && !source.isCurrent())
      ) {
        throw new AuthorizationProviderUnavailableError(
          "installed own authorization changed",
        );
      }
      return {
        reader,
        lease: attachment,
        generationId: attachment.generationId,
      };
    } catch (error) {
      attachment.release();
      throw error;
    }
  }

  async #load(
    contextDigest: string,
    historical: boolean,
    epoch: number,
    owner: symbol,
    signal?: AbortSignal,
    loadSource?: ContextLoadSource,
  ): Promise<ProviderContextEntry> {
    const candidateSource = loadSource && !("ownResume" in loadSource)
      ? loadSource
      : undefined;
    assertDigest(contextDigest);
    this.#contextResolves += 1;
    // The revocation watch lives on one exact socket and, for a managed cache,
    // takes a lease on the published generation it reads from. That lease is
    // released only when the binding is explicitly replaced or disposed, so a
    // watch never waits for a physical close. `signal` is owned by the pending
    // load entry, so a stop or verifier reset cancels pre-entry setup and
    // releases the provisional lease and watch promptly instead of waiting out
    // the ordinary NATS request timeout.
    let source: {
      reader: AuthorizationRegistryReader;
      lease?: TransportLease;
      generationId?: number;
    };
    try {
      source = await this.#acquireLoadSource(signal, loadSource);
    } catch (error) {
      if (error instanceof AuthorizationProviderUnavailableError) throw error;
      throw new AuthorizationProviderUnavailableError(
        "authorization revocation watch is unavailable",
        error,
      );
    }
    const { reader, lease, generationId } = source;
    const requireOrigin = () => {
      if (
        signal?.aborted ||
        (loadSource && "ownResume" in loadSource && !loadSource.isCurrent())
      ) {
        throw new AuthorizationProviderUnavailableError(
          "installed own authorization changed",
        );
      }
    };
    let watch:
      | Awaited<
        ReturnType<AuthorizationRegistryReader["watchRevocation"]>
      >
      | undefined;
    try {
      watch = await this.#registryIo(
        "authorization revocation watch is unavailable",
        () => reader.watchRevocation(contextDigest, signal),
      );
      requireOrigin();
    } catch (error) {
      lease?.release();
      // Setup may have completed before the origin changed.
      await watch?.close().catch(() => undefined);
      throw error;
    }
    let entry: ProviderContextEntry | undefined;
    let owned = false;
    try {
      const contextEntry = await this.#registryIo(
        "authorization context registry is unavailable",
        () => reader.getContext(contextDigest),
        signal,
      );
      requireOrigin();
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
      const issuer = await this.#issuer(issuerKeyId, signal);
      requireOrigin();
      const binding: CoverageBinding = {
        reader,
        ...(candidateSource ? { candidateSignal: candidateSource.signal } : {}),
        ...(generationId !== undefined ? { generationId } : {}),
        ...(lease ? { lease } : {}),
        watch: watch.iterator,
        close: async () => {
          try {
            await watch.close();
          } finally {
            lease?.release();
          }
        },
      };
      entry = {
        contextDigest,
        context,
        issuer,
        epoch,
        covered: false,
        disposed: false,
        resourcesDisposed: false,
        leases: 0,
        binding,
        ...(candidateSource ? { candidatePending: true } : {}),
      };
      owned = true;
      while (true) {
        const result = await this.#registryIo(
          "authorization revocation watch is unavailable",
          () => binding.watch.next(),
        );
        requireOrigin();
        if (result.done) {
          throw new AuthorizationProviderUnavailableError(
            "authorization revocation watch ended during initialization",
          );
        }
        if (result.value.operation === "initialized") break;
        this.#applyRevocation(entry, result.value);
      }
      entry.covered = true;
      void this.#watchRevocation(entry, binding);
      const verified = await this.#verified(
        entry,
        historical,
        loadSource?.nowSeconds(),
      );
      if (verified.verified.contextDigest !== contextDigest) {
        throw new Error(
          "authorization context digest does not match its registry key",
        );
      }
      this.#requirePendingOwner(contextDigest, epoch, owner);
      if (loadSource && "ownResume" in loadSource && !loadSource.isCurrent()) {
        throw new AuthorizationProviderUnavailableError(
          "installed own authorization changed",
        );
      }
      if (entry.disposed || !entry.covered) {
        throw new AuthorizationProviderUnavailableError(
          "authorization context lost revocation coverage",
        );
      }
      this.#contexts.set(contextDigest, entry);
      // A cold load can install its entry after the publication cohort
      // reconcile already ran, leaving an exact pin on a generation that is no
      // longer published. Compare against the retained current publication now
      // so a real stale pin migrates even with no further traffic or
      // publication; `#reconcile` revalidates entry identity and epoch. A
      // candidate load is deliberately pinned to its own safe survivor until
      // promotion, so it is never scheduled for published-cohort migration.
      if (!candidateSource) {
        const transport = this.#transport;
        const published = transport?.publishedGenerationId();
        if (
          transport && published !== undefined && published !== generationId
        ) {
          this.#scheduleReconcile();
        }
      }
      this.#notifyLiveChanges();
      return entry;
    } catch (error) {
      if (owned && entry) {
        this.#invalidate(entry);
      } else {
        lease?.release();
        await watch.close().catch(() => undefined);
      }
      throw error;
    }
  }

  async #watchRevocation(
    entry: ProviderContextEntry,
    binding: CoverageBinding,
    onInitialized?: () => void,
  ): Promise<void> {
    const isActive = () => entry.binding === binding;
    try {
      while (entry.covered && !entry.disposed) {
        const result = await binding.watch.next();
        if (result.done) return;
        if (result.value.operation === "initialized") {
          binding.initialized = true;
          onInitialized?.();
          continue;
        }
        this.#applyRevocation(entry, result.value);
      }
    } catch {
      // Coverage is invalidated below; the next request resynchronizes it.
    } finally {
      // A binding that has already been replaced by a make-before-break
      // migration must not invalidate the entry that now owns its successor.
      binding.ended = true;
      onInitialized?.();
      if (isActive()) this.#invalidate(entry);
      try {
        await binding.watch.return?.();
      } catch {
        // The entry is already unusable and will reload through a new binding.
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
    // Apply the verified revocation to the shared entry before notifying, so a
    // retained peer listener that re-checks coverage synchronously during the
    // notification already observes the revoked state instead of a stale
    // covered one it would never be told about again.
    entry.revokedAt = Math.max(
      entry.revokedAt ?? 0,
      parseRevocation(event.value),
    );
    this.#notifyLiveChanges();
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
      const candidate = this.#cache.candidateVerified();
      if (candidate) this.#cache.invalidateCandidate(candidate);
      // Drop the candidate-owned pending coverage promptly instead of waiting
      // for the next refresh to supersede it.
      if (this.#pendingEntry?.contextDigest === entry.contextDigest) {
        this.#dropPending();
      }
      this.#cache.requestRefresh();
    }
  }

  async #issuer(
    keyId: string,
    signal?: AbortSignal,
  ): Promise<AuthorizationIssuerKey> {
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
        signal,
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

  #verified(
    entry: ProviderContextEntry,
    historical: boolean,
    now = this.#now(),
  ) {
    const key = historical ? "historical" : "live";
    let pending = entry[key];
    if (!pending) {
      this.#contextVerifications += 1;
      const created = import("../protocol_wasm.ts").then(
        ({ createAuthorizationContextHandleWasm }) =>
          createAuthorizationContextHandleWasm({
            issuer: entry.issuer,
            context: entry.context,
            policy: this.#policy(now),
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
    epoch: number,
    owner: symbol,
  ): void {
    this.#requireRunning();
    const pending = this.#inFlight.get(contextDigest);
    if (
      epoch !== this.#cacheEpoch || pending?.epoch !== epoch ||
      pending.owner !== owner
    ) {
      throw new AuthorizationProviderUnavailableError(
        "authorization context load became stale",
      );
    }
  }

  #requireEntry(entry: ProviderContextEntry): void {
    this.#requireRunning();
    if (
      entry.epoch !== this.#cacheEpoch || !entry.covered ||
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
      this.#onOwnInvalidated?.("coverage_lost");
      this.#release(entry);
    }
    entry.covered = false;
    entry.disposed = true;
    const binding = entry.binding;
    entry.binding = undefined;
    if (binding) void binding.close();
    this.#abortProvisional(entry.contextDigest);
    if (this.#contexts.get(entry.contextDigest) === entry) {
      this.#contexts.delete(entry.contextDigest);
    }
    if (entry.leases === 0) this.#disposeResources(entry);
    if (
      wasOwn && entry.revokedAt === undefined && this.#connected &&
      !this.#cache.hasCandidate()
    ) {
      void this.#restoreOwnContext(this.#cacheEpoch);
    }
  }

  /** Cancel any in-flight provisional binding and retry for `digest`. */
  #abortProvisional(digest: string): void {
    const timer = this.#retryTimers.get(digest);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.#retryTimers.delete(digest);
    }
    const provisional = this.#provisionals.get(digest);
    if (provisional && !provisional.committed) provisional.abort.abort();
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
    signal?: AbortSignal,
  ): Promise<T> {
    let abort: (() => void) | undefined;
    try {
      signal?.throwIfAborted();
      if (!signal) return await operation();
      const cancelled = new Promise<never>((_, reject) => {
        abort = () => reject(signal.reason);
        signal.addEventListener("abort", abort, { once: true });
      });
      return await Promise.race([operation(), cancelled]);
    } catch (error) {
      if (error instanceof AuthorizationProviderUnavailableError) throw error;
      throw new AuthorizationProviderUnavailableError(message, error);
    } finally {
      if (abort) signal?.removeEventListener("abort", abort);
    }
  }

  /**
   * Cache-lifecycle readiness for cached-authority operations.
   *
   * This is verifier lifecycle only (started and not stopped), deliberately not
   * a transport-availability check: cached authorization is digest/coverage
   * scoped and must not be discarded because a default or logical transport is
   * momentarily unavailable. Finite registry IO acquires its own physical
   * attachment and fails on its own if none is usable.
   */
  #requireRunning(): void {
    if (!this.#started || this.#stopped) {
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
