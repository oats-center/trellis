import type { Authenticator, NatsConnection } from "@nats-io/nats-core";

import {
  admittedPolicyCovers,
  type OwnAdmission,
  readOwnAdmission,
} from "../auth/authorization/transport_state.ts";
import {
  classifyTransportAuthorizationWasm,
  transportAuthorizationDigestWasm,
  type TransportAuthorizationV1,
} from "../auth/protocol_wasm.ts";
import { TransportError } from "../errors/TransportError.ts";
import { logger as noopLogger, type LoggerLike } from "../globals.ts";

/** Subjects one finite exchange or pinned observation needs on one generation. */
export type TransportRequirement = {
  publish?: readonly string[];
  subscribe?: readonly string[];
};

/** Lifecycle state of one physical transport generation. */
export type TransportGenerationState = "current" | "draining" | "closed";

/**
 * Bounded reason one generation was opened, promoted, or retired.
 *
 * This is the only classification carried into telemetry; it never carries a
 * subject, grant, or capability dimension.
 */
export type TransportGenerationReason =
  | "initial"
  | "authorization_growth"
  | "recovery"
  | "reduction_replacement"
  | "reduction"
  | "logical_close";

/** Bounded internal lifecycle telemetry for one transport generation. */
export type TransportGenerationEvent = {
  readonly type:
    | "transport_generation.opening"
    | "transport_generation.admitted"
    | "transport_generation.activated"
    | "transport_generation.draining"
    | "transport_generation.closed"
    | "transport_generation.open_failed";
  readonly generationId: number;
  readonly participantKind: "user" | "service" | "device";
  readonly reason: TransportGenerationReason;
  readonly outcome?: "ok" | "error";
};

/**
 * Immutable CONNECT material derived from one exact authorization context.
 *
 * The authenticators are built from the frozen context digest and routing JWT
 * captured at preparation time, so the NATS CONNECT cannot silently use a newer
 * mutable context.
 */
export type TransportGenerationConnect = {
  readonly servers: string[];
  readonly authenticators: Authenticator[];
  readonly inboxPrefix: string;
  readonly maxReconnectAttempts?: number;
  readonly waitOnFirstConnect?: boolean;
  readonly timeoutMs?: number;
};

/**
 * One prepared generation: the exact context digest it will CONNECT with, the
 * signed policy that supplies its immutable admitted authority, and the CONNECT
 * bundle itself.
 */
export type TransportGenerationPrepared = {
  readonly contextDigest: string;
  readonly policy: TransportAuthorizationV1;
  readonly policyDigest: string;
  readonly connect: TransportGenerationConnect;
};

/** Bounded acquisition budget for one lease request. */
export type TransportAcquireOptions = {
  signal?: AbortSignal;
  /** Absolute epoch-millisecond deadline after which acquisition fails. */
  deadlineMs?: number;
};

/** One held transport generation. The holder releases it when its work ends. */
export type TransportLease = {
  readonly nc: NatsConnection;
  release(): void;
};

/** Provides the current generation's raw connection and leases. @internal */
export type TrellisTransportProvider = {
  /** Raw connection of the current default generation, for transitional surfaces. */
  currentNats(): NatsConnection;
  acquireCurrent(opts?: TransportAcquireOptions): Promise<TransportLease>;
  acquireFor(
    requirement: TransportRequirement,
    opts?: TransportAcquireOptions,
  ): Promise<TransportLease>;
  /** Resolves when the logical connection closes. */
  closed(): Promise<void | Error>;
  /** Logical lifecycle event stream (logical disconnect/reconnect/closed). */
  status(): AsyncIterable<unknown>;
  /** Close the logical connection and every physical generation. */
  close(): Promise<void>;
  /** Whether the logical connection is closed. */
  isClosed(): boolean;
};

/**
 * Wraps one fixed physical connection as a lease provider.
 *
 * Used by runtimes that still own a single transport (device, service outbound):
 * every acquirer receives the same connection and a no-op release.
 * @internal
 */
export function fixedTransportProvider(
  nc: NatsConnection,
): TrellisTransportProvider {
  const lease: TransportLease = { nc, release: () => {} };
  return {
    currentNats: () => nc,
    acquireCurrent: () => Promise.resolve(lease),
    acquireFor: () => Promise.resolve(lease),
    closed: () => nc.closed(),
    status: () => nc.status(),
    close: () => nc.close(),
    isClosed: () => nc.isClosed(),
  };
}

/**
 * One ready physical transport generation.
 *
 * Its admitted policy is immutable: a later authorization promotion never
 * rewrites it, so a generation can always be compared against the authority it
 * actually connected with.
 */
export class TransportGeneration {
  readonly id: number;
  readonly nc: NatsConnection;
  readonly contextDigest: string;
  readonly admittedPolicy: TransportAuthorizationV1;
  readonly admittedPolicyDigest: string;
  /**
   * Broker-reported authenticated attachment name of the *current* physical
   * attachment. The admitted context digest and policy are immutable, but a
   * verified reconnect of the same context refreshes this marker rather than
   * leaving a stale identity; a reconnect admitting a different context retires
   * the generation before this can be rewritten.
   */
  physicalConnectionId: string;
  readonly createdAtSeconds: number;
  state: TransportGenerationState;
  /**
   * Whether the physical connection is currently admitted and usable for new
   * work. A disconnect clears it; a reconnect must re-verify the exact
   * admission marker before it is set again.
   */
  ready = true;
  #leases = 0;
  #activatedAtSeconds?: number;
  #closedAtSeconds?: number;
  readonly #onLeaseReleased: () => void;

  /** @internal */
  constructor(args: {
    id: number;
    nc: NatsConnection;
    contextDigest: string;
    admittedPolicy: TransportAuthorizationV1;
    admittedPolicyDigest: string;
    physicalConnectionId: string;
    state: TransportGenerationState;
    createdAtSeconds: number;
    onLeaseReleased: () => void;
  }) {
    this.id = args.id;
    this.nc = args.nc;
    this.contextDigest = args.contextDigest;
    this.admittedPolicy = args.admittedPolicy;
    this.admittedPolicyDigest = args.admittedPolicyDigest;
    this.physicalConnectionId = args.physicalConnectionId;
    this.state = args.state;
    this.createdAtSeconds = args.createdAtSeconds;
    this.#onLeaseReleased = args.onLeaseReleased;
  }

  /** Number of leases currently held on this generation. */
  get leaseCount(): number {
    return this.#leases;
  }

  /** @internal */
  get activatedAtSeconds(): number | undefined {
    return this.#activatedAtSeconds;
  }

  /** @internal */
  get closedAtSeconds(): number | undefined {
    return this.#closedAtSeconds;
  }

  /** @internal */
  markActivated(atSeconds: number): void {
    this.#activatedAtSeconds = atSeconds;
  }

  /** @internal */
  markClosed(atSeconds: number): void {
    this.#closedAtSeconds = atSeconds;
  }

  /** @internal */
  lease(): TransportLease {
    if (this.state === "closed" || !this.ready) {
      throw transportUnavailableError();
    }
    this.#leases += 1;
    let released = false;
    return {
      nc: this.nc,
      release: () => {
        if (released) return;
        released = true;
        if (this.#leases > 0) this.#leases -= 1;
        this.#onLeaseReleased();
      },
    };
  }
}

/** Dependencies owned by the logical connection that owns the manager. @internal */
export type TransportGenerationManagerOptions = {
  /** Bounded participant kind for telemetry; never a subject or grant. */
  kind: "user" | "service" | "device";
  nowSeconds(): number;
  /**
   * Newest installed desired application transport policy `D`.
   *
   * Reconcile and acquire decisions use this value; it is never the frozen
   * policy of an already-ready generation. It must return the same object
   * reference until the installed policy actually changes: the manager uses
   * reference equality to detect a concurrent promotion across awaits.
   */
  desiredPolicy(): TransportAuthorizationV1 | undefined;
  /** Capture immutable CONNECT material for the newest desired policy. */
  prepare(): Promise<TransportGenerationPrepared | undefined>;
  /** Open one physical NATS connection with the frozen CONNECT bundle. */
  open(connect: TransportGenerationConnect): Promise<NatsConnection>;
  /** Bounded own-admission read; defaults to the broker `$SYS` read. */
  readAdmission?(nc: NatsConnection): Promise<OwnAdmission | undefined>;
  admissionTimeoutMs?: number;
  /**
   * Bound on one candidate CONNECT attempt. A physical connect can keep
   * retrying a denied credential, so the manager never awaits it unbounded;
   * a late success is closed instead of adopted.
   */
  openTimeoutMs?: number;
  log?: LoggerLike;
  onEvent?(event: TransportGenerationEvent): void;
  /**
   * Install framework intake for a generation before it is published as the
   * logical default. Awaited, so provider/Live/consumer routing exists before
   * any new work can select the generation; a rejection fails the candidate.
   * @internal
   */
  onPreActivate?(generation: TransportGeneration): Promise<void> | void;
  /**
   * Stop *new* framework intake for a generation that has just been superseded
   * as the default. The physical connection and already-accepted work stay
   * alive until leases reach zero; this only retires the shared/queue-grouped
   * intake so overlapped generations do not double-deliver.
   * @internal
   */
  onDrain?(generation: TransportGeneration): void;
  /**
   * A generation has just become the logical default. Used to recompute
   * admission-derived state (transport gate, health, hints) from the new
   * current generation. @internal
   */
  onActivate?(generation: TransportGeneration): void;
  /**
   * Retire framework intake and per-generation resources for a generation.
   * Awaited before its physical connection closes and before `close()`
   * resolves, so a superseded generation is never torn down while intake can
   * still reference it.
   * @internal
   */
  onRetire?(generation: TransportGeneration): Promise<void> | void;
};

/**
 * Owns the physical transport generations of one logical Trellis connection.
 *
 * There is at most one current generation, zero or more draining generations,
 * and at most one automatic candidate worker. Growth opens a new generation
 * without disturbing existing work; reductions retire any generation whose
 * admitted policy current authority no longer covers, regardless of leases.
 * @internal
 */
export class TransportGenerationManager implements TrellisTransportProvider {
  readonly #options: TransportGenerationManagerOptions;
  readonly #log: LoggerLike;
  #generations: TransportGeneration[] = [];
  #currentId?: number;
  #nextId = 0;
  #adoptionTask?: Promise<void>;
  #adoptionRequested = false;
  #lastReason: TransportGenerationReason = "authorization_growth";
  #failure?: { digest: string; attempts: number; nextRetryAtMs: number };
  #failureTimer?: ReturnType<typeof setTimeout>;
  #closed = false;
  #logicalConnected = false;
  readonly #closeDeferred = Promise.withResolvers<void>();
  readonly #changeWaiters = new Set<() => void>();
  readonly #logicalListeners = new Set<(event: unknown) => void>();
  readonly #logicalClosers = new Set<() => void>();
  readonly #retirements = new Set<Promise<void>>();

  constructor(options: TransportGenerationManagerOptions) {
    this.#options = options;
    this.#log = options.log ?? noopLogger;
  }

  /**
   * Logical transport lifecycle over all generations.
   *
   * Planned rollover never emits a disconnect/reconnect; only a real loss of
   * the current default with no safe survivor, or a logical close, does.
   * @internal
   */
  status(): AsyncIterable<unknown> {
    const listeners = this.#logicalListeners;
    const closers = this.#logicalClosers;
    return {
      [Symbol.asyncIterator]() {
        const queue: unknown[] = [];
        let wake: (() => void) | undefined;
        let done = false;
        const wakeUp = () => {
          wake?.();
          wake = undefined;
        };
        const listener = (event: unknown) => {
          queue.push(event);
          wakeUp();
        };
        const closer = () => {
          done = true;
          wakeUp();
        };
        listeners.add(listener);
        closers.add(closer);
        const detach = () => {
          listeners.delete(listener);
          closers.delete(closer);
        };
        return {
          async next(): Promise<IteratorResult<unknown>> {
            while (queue.length === 0 && !done) {
              await new Promise<void>((resolve) => {
                wake = resolve;
              });
            }
            if (queue.length > 0) {
              return { value: queue.shift(), done: false };
            }
            detach();
            return { value: undefined, done: true };
          },
          return(): Promise<IteratorResult<unknown>> {
            done = true;
            detach();
            wakeUp();
            return Promise.resolve({ value: undefined, done: true });
          },
        };
      },
    };
  }

  /**
   * Adopt an already-connected initial generation after verifying its exact
   * admission.
   *
   * Fails closed when the broker reports no marker, a malformed marker, or a
   * digest other than the one used to open the connection.
   */
  async initialize(
    nc: NatsConnection,
    prepared: TransportGenerationPrepared,
  ): Promise<TransportGeneration> {
    if (this.#closed) {
      await Promise.resolve(nc.close()).catch(() => undefined);
      throw transportClosedError();
    }
    let admission: OwnAdmission | undefined;
    try {
      admission = await this.#readAdmission(nc);
      assertExactAdmission(admission, prepared);
    } catch (error) {
      // Fail closed: an unverified initial attachment never becomes a generation.
      await Promise.resolve(nc.close()).catch(() => undefined);
      throw error;
    }
    if (this.#closed) {
      await Promise.resolve(nc.close()).catch(() => undefined);
      throw transportClosedError();
    }
    const generation = this.#createGeneration(nc, admission!, prepared);
    try {
      await this.#options.onPreActivate?.(generation);
    } catch (error) {
      this.#closeGeneration(generation, "logical_close");
      throw error;
    }
    if (this.#closed || this.#isGenerationClosed(generation)) {
      this.#closeGeneration(generation, "logical_close");
      throw transportClosedError();
    }
    this.#activate(generation, "initial");
    return generation;
  }

  /** Raw connection of the current default generation. @internal */
  currentNats(): NatsConnection {
    const current = this.#currentGeneration();
    if (current) return current.nc;
    const ready = this.#readyGenerations();
    const fallback = ready[ready.length - 1];
    if (fallback) return fallback.nc;
    throw transportUnavailableError();
  }

  /** Current default generation, when one exists. @internal */
  currentGeneration(): TransportGeneration | undefined {
    return this.#currentGeneration();
  }

  /** Every non-closed generation, oldest first. @internal */
  readyGenerations(): readonly TransportGeneration[] {
    return this.#readyGenerations();
  }

  acquireCurrent(opts?: TransportAcquireOptions): Promise<TransportLease> {
    return this.acquireFor({}, opts);
  }

  async acquireFor(
    requirement: TransportRequirement,
    opts?: TransportAcquireOptions,
  ): Promise<TransportLease> {
    for (;;) {
      this.#throwIfClosed();
      this.#assertNotAborted(opts);
      const generation = await this.#selectGeneration(requirement);
      if (generation) {
        // Revalidate synchronously: an awaited policy check may have let a
        // promotion or loss change the generation's eligibility.
        this.#assertNotAborted(opts);
        if (generation.state !== "closed" && generation.ready) {
          return generation.lease();
        }
        continue;
      }
      const desired = this.#options.desiredPolicy();
      if (
        desired &&
        await admittedPolicyCovers(
          desired,
          requirement,
          this.#options.nowSeconds(),
        )
      ) {
        this.#assertNotAborted(opts);
        this.#requestAdoption("authorization_growth");
        await this.#waitForChange(opts);
        continue;
      }
      const current = this.#currentGeneration();
      if (current && current.ready) {
        // The fallback only exists to reach a real broker denial for an
        // ungranted requirement; it must never lease a generation whose
        // authority current desired policy no longer covers.
        if (!desired || await this.#safeUnder(current, desired)) {
          if (this.#options.desiredPolicy() !== desired) continue;
          return current.lease();
        }
        if (this.#options.desiredPolicy() !== desired) continue;
      }
      if (desired) {
        this.#requestAdoption("reduction");
        await this.#waitForChange(opts);
        continue;
      }
      throw transportUnavailableError();
    }
  }

  /**
   * Record that the newest installed authorization differs from the generation
   * default and let the adoption worker converge it.
   * @internal
   */
  authorizationPromoted(): void {
    if (this.#closed) return;
    this.#clearFailure();
    this.#requestAdoption("authorization_growth");
  }

  /**
   * Force one adoption pass and await its convergence.
   *
   * Transitional bridge for the explicit `refreshTransport()` API until that
   * surface is removed.
   * @internal
   */
  async adoptNow(opts?: TransportAcquireOptions): Promise<void> {
    this.#throwIfClosed();
    this.#clearFailure();
    const deadlineMs = opts?.deadlineMs;
    for (;;) {
      this.#throwIfClosed();
      this.#assertNotAborted(opts);
      this.#requestAdoption("authorization_growth");
      const task = this.#adoptionTask;
      if (task) await task;
      this.#throwIfClosed();
      this.#assertNotAborted(opts);
      const desired = this.#options.desiredPolicy();
      if (!desired) return;
      const desiredDigest = await this.#policyDigest(desired);
      const current = this.#currentGeneration();
      if (
        current?.admittedPolicyDigest === desiredDigest &&
        this.#options.desiredPolicy() === desired
      ) {
        return;
      }
      if (deadlineMs !== undefined && Date.now() >= deadlineMs) {
        throw transportTimeoutError();
      }
      if (this.#failure?.digest === desiredDigest) {
        throw transportUnavailableError();
      }
      await this.#waitForChange({
        ...(opts?.signal ? { signal: opts.signal } : {}),
        ...(deadlineMs === undefined ? {} : { deadlineMs }),
      });
    }
  }

  /** Whether the logical connection has been closed. @internal */
  isClosed(): boolean {
    return this.#closed;
  }

  /** Resolves when the logical connection closes. @internal */
  closed(): Promise<void | Error> {
    return this.#closeDeferred.promise;
  }

  /** Stop adoption and close every generation. @internal */
  async close(): Promise<void> {
    if (this.#closed) {
      await this.#closeDeferred.promise;
      return;
    }
    this.#closed = true;
    this.#adoptionRequested = false;
    this.#clearFailure();
    this.#emitLogical({ type: "closed" });
    for (const closer of [...this.#logicalClosers]) closer();
    this.#notify();
    const generations = [...this.#generations];
    for (const generation of generations) {
      this.#closeGeneration(generation, "logical_close");
    }
    this.#currentId = undefined;
    // Retirement must complete before the physical sockets are gone.
    await Promise.all([...this.#retirements]);
    await Promise.all(
      generations.map((generation) =>
        Promise.resolve(generation.nc.close()).catch(() => undefined)
      ),
    );
    // Never let a candidate CONNECT attempt block close; a late candidate is
    // retired by the `#closed` check when it resolves.
    if (this.#adoptionTask) {
      await Promise.race([
        this.#adoptionTask.catch(() => undefined),
        new Promise<void>((resolve) => setTimeout(resolve, 2_000)),
      ]);
    }
    this.#closeDeferred.resolve();
  }

  #createGeneration(
    nc: NatsConnection,
    admission: OwnAdmission,
    prepared: TransportGenerationPrepared,
  ): TransportGeneration {
    const generation = new TransportGeneration({
      id: ++this.#nextId,
      nc,
      contextDigest: prepared.contextDigest,
      admittedPolicy: prepared.policy,
      admittedPolicyDigest: prepared.policyDigest,
      physicalConnectionId: admission.authenticatedUser,
      state: "draining",
      createdAtSeconds: this.#options.nowSeconds(),
      onLeaseReleased: () => this.#notify(),
    });
    // The candidate is deliberately NOT in the selectable set yet: it is
    // published by `#activate` only after intake install and the final desired
    // recheck, so `acquireFor` can never lease an unpublished generation.
    // A permanently closed physical connection must never remain selectable. A
    // planned close marks the generation closed first, so this only acts on an
    // unexpected loss.
    void Promise.resolve(nc.closed()).then(
      () => this.#closeGeneration(generation, "recovery"),
      () => this.#closeGeneration(generation, "recovery"),
    );
    this.#monitorGeneration(generation);
    return generation;
  }

  /**
   * Watch one generation's own status stream.
   *
   * A disconnect stops the generation being selected for new work while pinned
   * and in-flight work continues. A native reconnect re-proves the generation's
   * exact admitted context digest: the same digest keeps the generation (and
   * refreshes its physical attachment marker), a different digest or a
   * re-authentication failure retires it and recovers from newest authorization.
   */
  #monitorGeneration(generation: TransportGeneration): void {
    const iterator = generation.nc.status()[Symbol.asyncIterator]();
    void (async () => {
      try {
        while (true) {
          const next = await iterator.next();
          if (next.done) return;
          const event = next.value as
            | { type?: unknown; isAuthError?: () => boolean }
            | null;
          const type = event?.type;
          if (
            type === "disconnect" || type === "reconnecting" ||
            type === "staleConnection" || type === "forceReconnect"
          ) {
            this.#markUnavailable(generation);
          } else if (type === "reconnect") {
            await this.#reverifyReconnect(generation);
          } else if (type === "error" && event?.isAuthError?.()) {
            // A frozen credential that can no longer re-authenticate recovers
            // from the newest authorization instead of retrying stale material.
            this.#closeGeneration(generation, "recovery");
          }
        }
      } catch {
        // The connection is gone; `nc.closed()` performs the recovery.
      }
    })();
  }

  /**
   * Re-prove a reconnected socket against the generation's exact admission.
   *
   * The admitted context digest and policy stay immutable. The broker-reported
   * attachment marker is a property of the current physical attachment, so a
   * successful re-admission of the same context refreshes it rather than
   * leaving a stale identity; a different digest retires the generation.
   */
  async #reverifyReconnect(generation: TransportGeneration): Promise<void> {
    if (this.#closed || this.#isGenerationClosed(generation)) return;
    const admission = await this.#readAdmission(generation.nc);
    if (this.#closed || this.#isGenerationClosed(generation)) return;
    if (admission?.contextDigest === generation.contextDigest) {
      generation.physicalConnectionId = admission.authenticatedUser;
      this.#setReady(generation, true);
    } else {
      this.#closeGeneration(generation, "recovery");
    }
  }

  #isGenerationClosed(generation: TransportGeneration): boolean {
    return generation.state === "closed";
  }

  /** Mark a generation's physical attachment lost; it is never leased again. */
  #markUnavailable(generation: TransportGeneration): void {
    if (generation.state === "closed" || !generation.ready) return;
    generation.ready = false;
    if (this.#currentId === generation.id) {
      // A transient disconnect waits for the native reconnect; only a real
      // close or a changed admission recovers a new generation.
      this.#handleCurrentUnavailable(false);
    }
    this.#notify();
  }

  #setReady(generation: TransportGeneration, ready: boolean): void {
    if (generation.state === "closed" || generation.ready === ready) return;
    generation.ready = ready;
    if (this.#currentId === generation.id && !this.#logicalConnected) {
      this.#logicalConnected = true;
      this.#emitLogical({ type: "reconnect" });
    }
    this.#notify();
  }

  /**
   * React to the current default losing readiness or closing.
   *
   * With a ready survivor the logical connection stays connected and a
   * survivor is promoted; without one it enters ordinary disconnected
   * lifecycle until recovery opens the newest authorization.
   */
  #handleCurrentUnavailable(recoverFromLoss: boolean): void {
    if (this.#closed) return;
    const survivor = this.#readyGenerations().some((generation) =>
      generation.ready
    );
    if (survivor) {
      this.#requestAdoption("recovery");
      return;
    }
    if (this.#logicalConnected) {
      this.#logicalConnected = false;
      this.#emitLogical({ type: "disconnect" });
    }
    // A disconnect waits for the native reconnect; a generation that is really
    // gone recovers a fresh one from the newest authorization.
    if (recoverFromLoss) this.#requestAdoption("recovery");
  }

  async #readAdmission(
    nc: NatsConnection,
  ): Promise<OwnAdmission | undefined> {
    const read = this.#options.readAdmission;
    if (read) return await read(nc);
    return await readOwnAdmission(
      nc,
      this.#options.admissionTimeoutMs ?? 5_000,
    );
  }

  #currentGeneration(): TransportGeneration | undefined {
    if (this.#currentId === undefined) return undefined;
    const generation = this.#generations.find(
      (candidate) => candidate.id === this.#currentId,
    );
    return generation && generation.state !== "closed" ? generation : undefined;
  }

  #readyGenerations(): TransportGeneration[] {
    return this.#generations.filter((generation) =>
      generation.state !== "closed"
    );
  }

  async #selectGeneration(
    requirement: TransportRequirement,
  ): Promise<TransportGeneration | undefined> {
    const now = this.#options.nowSeconds();
    const desired = this.#options.desiredPolicy();
    const current = this.#currentGeneration();
    const candidates: TransportGeneration[] = [];
    if (current) candidates.push(current);
    const ready = this.#readyGenerations();
    // Newest first, so a newly opened wider generation wins over older ones.
    for (let index = ready.length - 1; index >= 0; index--) {
      if (ready[index] !== current) candidates.push(ready[index]);
    }
    for (const generation of candidates) {
      if (generation.state === "closed" || !generation.ready) continue;
      // Never lease a generation carrying authority current authorization no
      // longer covers; reduction retires it, and server enforcement is
      // authoritative until then.
      if (desired && !await this.#safeUnder(generation, desired)) continue;
      const covers = await admittedPolicyCovers(
        generation.admittedPolicy,
        requirement,
        now,
      );
      // Revalidate after the last await: a promotion or readiness change during
      // the awaited checks must not yield a stale or unsafe lease.
      if (this.#options.desiredPolicy() !== desired) return undefined;
      if (this.#isGenerationClosed(generation) || !generation.ready) continue;
      if (covers) return generation;
    }
    return undefined;
  }

  #activate(
    generation: TransportGeneration,
    reason: TransportGenerationReason,
  ): void {
    if (
      this.#closed || generation.state === "closed" || !generation.ready
    ) {
      this.#closeGeneration(generation, "logical_close");
      return;
    }
    const previous = this.#currentGeneration();
    if (previous && previous !== generation) {
      previous.state = "draining";
      this.#emit({
        type: "transport_generation.draining",
        generationId: previous.id,
        reason,
      });
      // Retire the superseded generation's shared intake now that a default
      // exists. Its pinned sessions and accepted work are untouched.
      this.#options.onDrain?.(previous);
    }
    if (!this.#generations.includes(generation)) {
      this.#generations.push(generation);
    }
    generation.state = "current";
    generation.markActivated(this.#options.nowSeconds());
    this.#currentId = generation.id;
    this.#emit({
      type: "transport_generation.activated",
      generationId: generation.id,
      reason,
    });
    this.#clearFailure();
    if (!this.#logicalConnected) {
      this.#logicalConnected = true;
      this.#emitLogical({ type: "reconnect" });
    }
    this.#options.onActivate?.(generation);
    this.#notify();
  }

  #closeGeneration(
    generation: TransportGeneration,
    reason: TransportGenerationReason,
  ): void {
    if (generation.state === "closed") return;
    const wasCurrent = this.#currentId === generation.id;
    generation.state = "closed";
    generation.ready = false;
    generation.markClosed(this.#options.nowSeconds());
    if (wasCurrent) this.#currentId = undefined;
    // Retire framework intake first; the physical socket closes afterwards so a
    // superseded generation is never torn down while intake still references it.
    const retirement = this.#runRetirement(generation);
    this.#retirements.add(retirement);
    void retirement.finally(() => {
      this.#retirements.delete(retirement);
      void Promise.resolve(generation.nc.close()).catch(() => undefined);
    });
    this.#emit({
      type: "transport_generation.closed",
      generationId: generation.id,
      reason,
    });
    if (wasCurrent && reason !== "logical_close") {
      this.#handleCurrentUnavailable(true);
    }
    this.#notify();
  }

  #runRetirement(generation: TransportGeneration): Promise<void> {
    try {
      return Promise.resolve(this.#options.onRetire?.(generation)).catch(
        (error) => {
          this.#log.warn(
            { error, generationId: generation.id },
            "transport generation retirement failed",
          );
        },
      );
    } catch (error) {
      this.#log.warn(
        { error, generationId: generation.id },
        "transport generation retirement failed",
      );
      return Promise.resolve();
    }
  }

  #requestAdoption(reason: TransportGenerationReason): void {
    if (this.#closed) return;
    this.#lastReason = reason;
    this.#adoptionRequested = true;
    // Requesting adoption is not itself a state change: notifying here would
    // wake every waiter into a no-op re-request loop.
    if (this.#adoptionTask) return;
    const task = this.#runAdoption().catch((error) => {
      this.#log.warn(
        { error },
        "transport generation adoption failed unexpectedly",
      );
    }).finally(() => {
      if (this.#adoptionTask === task) this.#adoptionTask = undefined;
      if (this.#adoptionRequested && !this.#closed) {
        this.#requestAdoption(reason);
      }
    });
    this.#adoptionTask = task;
  }

  async #runAdoption(): Promise<void> {
    while (this.#adoptionRequested && !this.#closed) {
      this.#adoptionRequested = false;
      const desired = this.#options.desiredPolicy();
      if (!desired) {
        // The desired context can be temporarily unavailable (for example
        // while the runtime is unreachable). Keep a bounded retry alive so
        // recovery resumes once it returns instead of stalling until the next
        // authorization promotion.
        this.#recordFailure("trellis.transport.no_desired_policy");
        return;
      }
      const desiredDigest = await this.#policyDigest(desired);
      if (this.#closed) return;
      // A newer authorization landed while digesting: re-run with it rather
      // than converging the superseded policy.
      if (this.#options.desiredPolicy() !== desired) {
        this.#adoptionRequested = true;
        continue;
      }
      // Reduction is hard: retire any generation current authority no longer
      // covers before considering a replacement.
      for (const generation of this.#readyGenerations()) {
        if (!await this.#safeUnder(generation, desired)) {
          this.#closeGeneration(generation, "reduction");
        }
      }
      if (this.#closed) return;
      if (this.#options.desiredPolicy() !== desired) {
        this.#adoptionRequested = true;
        continue;
      }
      const usable = this.#readyGenerations().filter((generation) =>
        generation.ready
      );
      if (!this.#currentGeneration()) {
        const survivor = usable.findLast((generation) =>
          generation.admittedPolicyDigest === desiredDigest
        ) ?? usable[usable.length - 1];
        if (survivor) {
          this.#activate(survivor, "reduction_replacement");
        }
      }
      const matching = this.#matchingGeneration(desiredDigest);
      if (matching) {
        // A same-policy generation that is temporarily not ready is waited for
        // rather than duplicated; its monitor re-verifies or it is retired.
        if (this.#currentId !== matching.id && matching.ready) {
          this.#activate(matching, "authorization_growth");
        }
        continue;
      }
      await this.#openCandidate(desired, desiredDigest);
    }
  }

  async #latestDesiredDigest(): Promise<string | undefined> {
    const desired = this.#options.desiredPolicy();
    if (!desired) return undefined;
    return await this.#policyDigest(desired);
  }

  async #safeUnder(
    generation: TransportGeneration,
    desired: TransportAuthorizationV1,
  ): Promise<boolean> {
    const relation = await classifyTransportAuthorizationWasm(
      generation.admittedPolicy,
      desired,
      this.#options.nowSeconds(),
    );
    return relation !== "reduction_required";
  }

  #matchingGeneration(
    desiredDigest: string,
  ): TransportGeneration | undefined {
    return this.#readyGenerations().find((generation) =>
      generation.admittedPolicyDigest === desiredDigest
    );
  }

  /**
   * Open one candidate from the newest desired policy.
   *
   * Immutable prepared material is captured before CONNECT and the broker must
   * report exactly that context digest. Before publication the candidate is
   * compared against the latest desired policy so an obsolete generation is
   * never published.
   */
  async #openCandidate(
    desired: TransportAuthorizationV1,
    desiredDigest: string,
  ): Promise<void> {
    // Bounded coalesced recovery: one open per policy at a time, and no
    // immediate retry of a policy that just failed.
    if (
      this.#failure?.digest === desiredDigest &&
      Date.now() < this.#failure.nextRetryAtMs
    ) {
      return;
    }
    const prepared = await this.#options.prepare();
    if (this.#closed) return;
    if (!prepared) {
      // Connect material was unavailable (for example expired routing material
      // while the runtime was unreachable). Keep a bounded retry alive rather
      // than waiting for the next authorization promotion.
      this.#recordFailure("trellis.transport.no_prepared_connect");
      return;
    }
    if (prepared.policyDigest !== desiredDigest) {
      this.#adoptionRequested = true;
      return;
    }
    this.#emit({
      type: "transport_generation.opening",
      generationId: this.#nextId + 1,
      reason: this.#lastReason,
    });
    let nc: NatsConnection;
    try {
      nc = await this.#openCandidateConnection(prepared.connect);
    } catch (error) {
      this.#recordFailure(prepared.policyDigest);
      this.#emit({
        type: "transport_generation.open_failed",
        generationId: this.#nextId + 1,
        reason: this.#lastReason,
        outcome: "error",
      });
      this.#log.warn(
        { error },
        "transport generation candidate failed to open",
      );
      return;
    }
    try {
      const admission = await this.#readAdmission(nc);
      assertExactAdmission(admission, prepared);
      // Only the newest desired policy is worth publishing. The identity check
      // is synchronous and runs after every await, so an obsolete candidate is
      // never activated.
      if (this.#closed || this.#options.desiredPolicy() !== desired) {
        await Promise.resolve(nc.close()).catch(() => undefined);
        if (!this.#closed) this.#adoptionRequested = true;
        return;
      }
      this.#emit({
        type: "transport_generation.admitted",
        generationId: this.#nextId + 1,
        reason: this.#lastReason,
        outcome: "ok",
      });
      const generation = this.#createGeneration(nc, admission!, prepared);
      // Install before activation: routing exists before the generation can be
      // selected as the default. A failure retires the unpublished candidate.
      try {
        await this.#options.onPreActivate?.(generation);
      } catch (error) {
        this.#closeGeneration(generation, "logical_close");
        throw error;
      }
      // Recheck the newest desired policy after intake install: a promotion
      // that landed during preactivation must not publish an obsolete
      // candidate. No await follows this check before activation.
      const latestDigest = await this.#latestDesiredDigest();
      if (
        this.#closed || this.#isGenerationClosed(generation) ||
        latestDigest !== prepared.policyDigest
      ) {
        this.#closeGeneration(generation, "logical_close");
        if (!this.#closed) this.#adoptionRequested = true;
        return;
      }
      this.#activate(generation, this.#lastReason);
    } catch (error) {
      await Promise.resolve(nc.close()).catch(() => undefined);
      this.#recordFailure(prepared.policyDigest);
      this.#emit({
        type: "transport_generation.open_failed",
        generationId: this.#nextId + 1,
        reason: this.#lastReason,
        outcome: "error",
      });
      this.#log.warn(
        { error },
        "transport generation candidate was not admitted",
      );
    }
  }

  async #openCandidateConnection(
    connect: TransportGenerationConnect,
  ): Promise<NatsConnection> {
    const pending = this.#options.open(connect);
    const timeoutMs = this.#options.openTimeoutMs ?? 20_000;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      return await Promise.race([
        pending,
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(transportTimeoutError()), timeoutMs);
        }),
      ]);
    } catch (error) {
      // A connection that succeeds after the bound must not leak.
      void pending.then(
        (nc) => Promise.resolve(nc.close()).catch(() => undefined),
        () => undefined,
      );
      throw error;
    } finally {
      if (timer !== undefined) clearTimeout(timer);
    }
  }

  async #policyDigest(policy: TransportAuthorizationV1): Promise<string> {
    return await transportAuthorizationDigestWasm(policy);
  }

  #throwIfClosed(): void {
    if (this.#closed) throw transportClosedError();
  }

  #assertNotAborted(opts?: TransportAcquireOptions): void {
    if (opts?.signal?.aborted) {
      throw transportAbortedError(opts.signal.reason);
    }
    if (opts?.deadlineMs !== undefined && Date.now() >= opts.deadlineMs) {
      throw transportTimeoutError();
    }
  }

  #nextChange(): Promise<void> {
    return new Promise((resolve) => this.#changeWaiters.add(resolve));
  }

  #notify(): void {
    if (this.#changeWaiters.size === 0) return;
    const waiters = [...this.#changeWaiters];
    this.#changeWaiters.clear();
    for (const waiter of waiters) waiter();
  }

  #emitLogical(event: unknown): void {
    for (const listener of [...this.#logicalListeners]) listener(event);
  }

  /**
   * Record a bounded, coalesced recovery window for one failed policy.
   *
   * A later authorization promotion clears it immediately; otherwise a single
   * timer retries, with capped exponential backoff, so waiters never spin and
   * never wait indefinitely after a failed adoption.
   */
  #recordFailure(digest: string): void {
    const attempts =
      (this.#failure?.digest === digest ? this.#failure.attempts : 0) + 1;
    const delayMs = Math.min(1_000 * 2 ** Math.min(attempts - 1, 4), 8_000);
    // Keep the failure record (so backoff keeps growing) and only push the
    // retry window forward; the open gate skips opens until that time.
    this.#failure = {
      digest,
      attempts,
      nextRetryAtMs: Date.now() + delayMs,
    };
    if (this.#failureTimer !== undefined) clearTimeout(this.#failureTimer);
    this.#failureTimer = setTimeout(() => {
      this.#failureTimer = undefined;
      if (this.#closed) return;
      this.#requestAdoption("recovery");
    }, delayMs);
  }

  #clearFailure(): void {
    if (this.#failureTimer !== undefined) {
      clearTimeout(this.#failureTimer);
      this.#failureTimer = undefined;
    }
    this.#failure = undefined;
  }

  #waitForChange(opts?: TransportAcquireOptions): Promise<void> {
    const signal = opts?.signal;
    const deadlineMs = opts?.deadlineMs;
    // With no caller deadline, a failed-adoption retry window still bounds the
    // wait: it resolves (rather than times out) so the caller re-evaluates the
    // next bounded attempt instead of waiting forever or spinning.
    const retryAtMs = deadlineMs === undefined
      ? this.#failure?.nextRetryAtMs
      : undefined;
    if (!signal && deadlineMs === undefined && retryAtMs === undefined) {
      return this.#nextChange();
    }
    return new Promise<void>((resolve, reject) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      let settled = false;
      const cleanup = () => {
        this.#changeWaiters.delete(finish);
        if (timer !== undefined) clearTimeout(timer);
        signal?.removeEventListener("abort", abort);
      };
      const finish = () => {
        if (settled) return;
        settled = true;
        cleanup();
        resolve();
      };
      const abort = () => {
        if (settled) return;
        settled = true;
        cleanup();
        reject(transportAbortedError(signal?.reason));
      };
      const timeOut = () => {
        if (settled) return;
        settled = true;
        cleanup();
        reject(transportTimeoutError());
      };
      this.#changeWaiters.add(finish);
      if (deadlineMs !== undefined) {
        const remaining = deadlineMs - Date.now();
        if (remaining <= 0) {
          timeOut();
          return;
        }
        timer = setTimeout(timeOut, remaining);
      } else if (retryAtMs !== undefined) {
        const remaining = retryAtMs - Date.now();
        if (remaining <= 0) {
          finish();
          return;
        }
        timer = setTimeout(finish, remaining);
      }
      if (signal) {
        if (signal.aborted) {
          abort();
          return;
        }
        signal.addEventListener("abort", abort, { once: true });
      }
    });
  }

  #emit(event: Omit<TransportGenerationEvent, "participantKind">): void {
    this.#log.debug(event, "transport generation lifecycle");
    this.#options.onEvent?.({ ...event, participantKind: this.#options.kind });
  }
}

/**
 * Require the broker's exact own-admission marker for one prepared generation.
 *
 * A missing, malformed, or non-matching marker is a hard candidate failure; the
 * mutable current context is never used as a fallback.
 */
function assertExactAdmission(
  admission: OwnAdmission | undefined,
  prepared: TransportGenerationPrepared,
): void {
  if (!admission) {
    throw admissionError(
      "trellis.transport.admission_missing",
      "The broker did not report this connection's Trellis admission marker.",
      "Retry the connection. If it keeps failing, check Trellis runtime health.",
      prepared.contextDigest,
    );
  }
  if (admission.contextDigest === undefined) {
    throw admissionError(
      "trellis.transport.admission_malformed",
      "The broker reported a malformed Trellis admission marker.",
      "Retry the connection. If it keeps failing, check Trellis runtime health.",
      prepared.contextDigest,
    );
  }
  if (admission.contextDigest !== prepared.contextDigest) {
    throw admissionError(
      "trellis.transport.admission_mismatch",
      "The broker admitted a different authorization context than the one this connection opened with.",
      "Retry the connection. If it keeps failing, check Trellis runtime health.",
      prepared.contextDigest,
    );
  }
}

function admissionError(
  code: string,
  message: string,
  hint: string,
  contextDigest: string,
): TransportError {
  return new TransportError({
    code,
    message,
    hint,
    context: { contextDigest },
  });
}

function transportUnavailableError(): TransportError {
  return new TransportError({
    code: "trellis.transport.unavailable",
    message: "No admitted Trellis transport generation is available.",
    hint: "Retry when the Trellis runtime connection is available.",
  });
}

function transportTimeoutError(): TransportError {
  return new TransportError({
    code: "trellis.transport.timeout",
    message:
      "The Trellis runtime did not establish the required transport authority in time.",
    hint:
      "Retry the operation. If it keeps failing, check Trellis runtime health.",
  });
}

function transportAbortedError(cause: unknown): TransportError {
  return new TransportError({
    code: "trellis.transport.aborted",
    message: "The transport request was cancelled before it could complete.",
    hint: "Retry the operation when the caller is ready.",
    cause,
  });
}

function transportClosedError(): TransportError {
  return new TransportError({
    code: "trellis.transport.closed",
    message: "The Trellis logical connection is closed.",
    hint: "Connect to Trellis again before making another request.",
  });
}
