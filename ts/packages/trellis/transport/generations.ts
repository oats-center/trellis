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
 * Handle to retire and dispose one generation's owner resources.
 *
 * `intakeStopped` resolves once every outstanding intake is accounted for; the
 * manager awaits it and the generation's accepted lease count before invoking
 * `dispose` on the graceful path. @internal
 */
export type TransportGenerationDisposal = {
  readonly intakeStopped: Promise<void>;
  dispose(): Promise<void>;
};

/**
 * Discriminated terminal result retained by the logical connection.
 *
 * An explicit `close()` and an authoritative terminal failure are distinct: the
 * latter carries the existing typed auth/transport error. Exposed only through
 * the existing `closed()` result, never as a new public status surface. @internal
 */
export type TransportTerminalResult =
  | { kind: "closed" }
  | { kind: "failure"; error: Error };

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
  /**
   * Register a concrete cleanup handle for this generation's owner resources.
   *
   * The owner pushes its real retirement/disposal scope here synchronously,
   * before its first install await, so the manager owns cleanup from
   * installation onward rather than looking it up later. Returns false when the
   * generation is already retired, in which case the manager has taken over the
   * handle's cleanup and the owner must not add further resources. @internal
   */
  readonly registerDisposal: (
    handle: TransportGenerationDisposal,
  ) => boolean;

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
    registerDisposal: (handle: TransportGenerationDisposal) => boolean;
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
    this.registerDisposal = args.registerDisposal;
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
   * The generation's `registerDisposal` carries cleanup ownership pushed by the
   * owner. @internal
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
  readonly #closeDeferred = Promise.withResolvers<void | Error>();
  readonly #changeWaiters = new Set<() => void>();
  readonly #logicalListeners = new Set<(event: unknown) => void>();
  readonly #logicalClosers = new Set<() => void>();
  readonly #retirements = new Set<Promise<void>>();
  /**
   * Cleanup handles pushed by each owning ingress at installation, per
   * generation. The manager owns them from install onward; a zero-owner
   * generation (e.g. a client with no provider ingress) legitimately has none.
   */
  readonly #disposals = new Map<number, TransportGenerationDisposal[]>();
  /** Generations with a graceful reap in flight. */
  readonly #reaping = new Set<number>();
  /** In-flight graceful reap tasks, so a reactivation can abort and await one. */
  readonly #reapTasks = new Map<number, Promise<void>>();
  /** Abort hooks for in-flight graceful reaps, keyed by generation id. */
  readonly #reapCancels = new Map<number, () => void>();
  /**
   * Generations whose disposal has irreversibly begun. A generation in this set
   * can never be reactivated, so its owner's torn-down support is never
   * resuscitated.
   */
  readonly #disposing = new Set<number>();
  /** Pending lease-zero waiters per generation id. */
  readonly #leaseZeroWaiters = new Map<number, Set<() => void>>();
  /** Internal discriminated terminal result, latched once. */
  #terminal?: TransportTerminalResult;

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
      // Owner cleanup is already pushed into the generation's registration;
      // the connect caller owns this failed logical connection and force-closes
      // its socket, so no accepted-work preservation is claimed here: run owner
      // cleanup, drop the generation, and rethrow without leaking it.
      generation.state = "closed";
      generation.ready = false;
      generation.markClosed(this.#options.nowSeconds());
      await this.#forcedRetirement(generation);
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

  /**
   * Lease the preferred attachment for the lifetime of one follower's use.
   *
   * Holding the lease keeps the generation from being reaped while verification
   * still reads from it; the follower releases it when it moves on. When `nc` is
   * given, that exact attachment is leased (never a substitute) so a watch and
   * its pin stay paired. Returns undefined when no admitted attachment matches.
   * @internal
   */
  acquirePreferredAttachment(
    nc?: NatsConnection,
  ): Promise<TransportLease | undefined> {
    const acquire = (
      generation: TransportGeneration | undefined,
    ): TransportLease | undefined => {
      if (!generation?.ready) return undefined;
      // An explicit attachment must be leased exactly, never substituted.
      if (nc !== undefined && generation.nc !== nc) return undefined;
      try {
        return generation.lease();
      } catch {
        return undefined;
      }
    };
    let preferred = acquire(this.#currentGeneration());
    if (!preferred) {
      const ready = this.#readyGenerations();
      for (let i = ready.length - 1; i >= 0 && !preferred; i -= 1) {
        preferred = acquire(ready[i]);
      }
    }
    return Promise.resolve(preferred);
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
        const safe = !desired || await this.#safeUnder(current, desired);
        // Revalidate after the awaited classification: acquisition deadline,
        // logical-connection terminal state, the logical default identity, and
        // readiness may all have changed while the check was pending.
        this.#assertNotAborted(opts);
        this.#throwIfClosed();
        if (safe) {
          if (this.#options.desiredPolicy() !== desired) continue;
          if (this.#currentGeneration() !== current || !current.ready) continue;
          return current.lease();
        }
        if (this.#options.desiredPolicy() !== desired) continue;
        if (this.#currentGeneration() !== current || !current.ready) continue;
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

  /**
   * Retained terminal result, once latched. @internal
   */
  terminalResult(): TransportTerminalResult | undefined {
    return this.#terminal;
  }

  /** Stop adoption and close every generation. @internal */
  async close(): Promise<void> {
    await this.#terminate({ kind: "closed" });
  }

  /**
   * Terminate the logical connection from an authoritative failure.
   *
   * Supplied only by the authorization controller once a failure is known to be
   * terminal (never by a per-generation failure). The typed cause is retained
   * and resolved through `closed()`, mirroring an explicit close for every
   * existing consumer while preserving the discriminated reason.
   * @internal
   */
  async terminate(error: Error): Promise<void> {
    await this.#terminate({ kind: "failure", error });
  }

  async #terminate(result: TransportTerminalResult): Promise<void> {
    if (this.#closed) {
      await this.#closeDeferred.promise;
      return;
    }
    this.#terminal = result;
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
    // Retirement must complete before the physical sockets are gone. Drain
    // repeatedly so a late owner cleanup registered during teardown is awaited
    // too instead of being dropped.
    while (this.#retirements.size > 0) {
      await Promise.allSettled([...this.#retirements]);
    }
    // Never let a candidate CONNECT attempt block close; a late candidate is
    // retired by the `#closed` check when it resolves.
    if (this.#adoptionTask) {
      await Promise.race([
        this.#adoptionTask.catch(() => undefined),
        new Promise<void>((resolve) => setTimeout(resolve, 2_000)),
      ]);
    }
    this.#closeDeferred.resolve(
      result.kind === "failure" ? result.error : undefined,
    );
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
      onLeaseReleased: () => this.#onLeaseReleased(generation),
      registerDisposal: (handle): boolean =>
        this.#registerDisposal(generation, handle),
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
      // exists. Its pinned sessions and accepted work are untouched; the
      // nonblocking reap closes it only after intake stops and its accepted
      // leases drain.
      this.#options.onDrain?.(previous);
      this.#beginGracefulRetire(previous, reason);
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

  /**
   * Force a generation closed immediately.
   *
   * Physical loss, uncovered authority, and logical terminal bypass the
   * graceful work wait, but disposal cleanup is still owned and observed before
   * the generation is dropped and its socket closed.
   */
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
    const retirement = this.#forcedRetirement(generation);
    this.#retirements.add(retirement);
    void retirement.finally(() => this.#retirements.delete(retirement));
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

  /**
   * Nonblocking graceful retirement of a superseded candidate.
   *
   * Retire intake immediately, wait for every outstanding intake and accepted
   * lease to settle, invoke disposal, then close the socket and drop the
   * generation. The wait is nonblocking so successor publication is never
   * delayed by a slow draining generation.
   */
  #beginGracefulRetire(
    generation: TransportGeneration,
    reason: TransportGenerationReason,
  ): void {
    if (this.#reaping.has(generation.id)) return;
    this.#reaping.add(generation.id);
    let aborted = false;
    const cancel = Promise.withResolvers<void>();
    this.#reapCancels.set(generation.id, () => {
      aborted = true;
      cancel.resolve();
    });
    const abandoned = (): boolean =>
      aborted || this.#closed || this.#isGenerationClosed(generation);
    let closed = false;
    const task = (async () => {
      await Promise.race([
        this.#intakeStoppedAll(generation.id),
        cancel.promise,
      ]);
      if (abandoned()) return;
      await Promise.race([this.#awaitLeaseZero(generation), cancel.promise]);
      if (abandoned()) return;
      // Disposal start is the irreversible retirement boundary: once it begins
      // the generation finishes closing and is never reinstated.
      this.#disposing.add(generation.id);
      await this.#disposeAll(generation.id);
      this.#finalizeRetired(generation, reason);
      closed = true;
    })().catch((error) => {
      aborted = true;
      this.#log.warn(
        { error, generationId: generation.id },
        "transport generation reaping failed",
      );
      // A reaping failure still leaves the generation unusable; finish its
      // physical teardown rather than leaking a half-retired generation.
      this.#finalizeRetired(generation, reason);
      closed = true;
    }).finally(() => {
      this.#reaping.delete(generation.id);
      this.#reapCancels.delete(generation.id);
      this.#reapTasks.delete(generation.id);
      // Only a finalized generation closes its socket; an aborted reap belongs
      // to a generation that was reinstated and stays open.
      if (closed && !this.#closed) {
        void Promise.resolve(generation.nc.close()).catch(() => undefined);
      }
    });
    this.#reapTasks.set(generation.id, task);
  }

  /**
   * Reinstall generic intake on a survivor that had been drained, before it is
   * published again.
   *
   * Serialized against any in-flight graceful reap: the reap is aborted and
   * awaited so it can never dispose the reinstalled generic intake or close the
   * socket. Live/control/session owners registered outside generic intake are
   * preserved. Returns false when the survivor is no longer usable.
   */
  async #ensureIntake(generation: TransportGeneration): Promise<boolean> {
    if (generation.state !== "draining") return true;
    // A generation whose disposal has begun is irreversibly retired; never
    // abort it or resuscitate torn-down support.
    if (this.#disposing.has(generation.id)) return false;
    const task = this.#reapTasks.get(generation.id);
    this.#reapCancels.get(generation.id)?.();
    if (task) await task;
    if (
      this.#closed || this.#isGenerationClosed(generation) ||
      this.#disposing.has(generation.id)
    ) return false;
    try {
      await this.#options.onPreActivate?.(generation);
      return true;
    } catch (error) {
      this.#log.warn(
        {
          error: error instanceof Error
            ? { name: error.name, message: error.message }
            : String(error),
          generationId: generation.id,
        },
        "transport survivor intake reinstall failed",
      );
      this.#closeGeneration(generation, "recovery");
      return false;
    }
  }

  /**
   * Reject an opened-but-unpublished candidate.
   *
   * Safety is judged against the *newest* desired policy with a post-await
   * snapshot fence, so a change that lands during classification forces an
   * immediate close instead of waiting for the next adoption pass. A candidate
   * whose admitted policy safely supports current authority is always tracked
   * draining (even with no current lease, because its intake may still account
   * buffered arrivals); a candidate current authority no longer covers is
   * force-closed as a reduction.
   */
  async #rejectCandidate(
    generation: TransportGeneration,
    reason: TransportGenerationReason,
  ): Promise<void> {
    if (generation.state === "closed") return;
    let safe = false;
    for (;;) {
      const newest = this.#options.desiredPolicy();
      if (!newest) {
        safe = false;
        break;
      }
      try {
        safe = await this.#safeUnder(generation, newest);
      } catch (error) {
        this.#log.warn(
          { error, generationId: generation.id },
          "transport candidate safety classification failed",
        );
        safe = false;
        break;
      }
      // Post-await fence: only accept the classification if the desired policy
      // reference did not change across the await.
      if (this.#options.desiredPolicy() === newest) break;
    }
    if (!safe) {
      this.#closeGeneration(generation, "reduction");
      return;
    }
    if (!this.#generations.includes(generation)) {
      this.#generations.push(generation);
    }
    generation.state = "draining";
    this.#options.onDrain?.(generation);
    this.#beginGracefulRetire(generation, reason);
  }

  async #forcedRetirement(generation: TransportGeneration): Promise<void> {
    try {
      // Cleanup on a forced path is owned and observed, but never blocks on
      // accepted work the lost physical attachment can no longer serve.
      await this.#disposeAll(generation.id);
    } catch (error) {
      this.#log.warn(
        { error, generationId: generation.id },
        "transport generation retirement failed",
      );
    } finally {
      this.#generations = this.#generations.filter((candidate) =>
        candidate !== generation
      );
      this.#reapTasks.delete(generation.id);
      this.#reapCancels.delete(generation.id);
      this.#disposals.delete(generation.id);
      this.#disposing.delete(generation.id);
      if (!generation.nc.isClosed()) {
        await Promise.resolve(generation.nc.close()).catch(() => undefined);
      }
    }
  }

  #finalizeRetired(
    generation: TransportGeneration,
    reason: TransportGenerationReason,
  ): void {
    if (generation.state === "closed") return;
    generation.state = "closed";
    generation.ready = false;
    generation.markClosed(this.#options.nowSeconds());
    if (this.#currentId === generation.id) this.#currentId = undefined;
    this.#generations = this.#generations.filter((candidate) =>
      candidate !== generation
    );
    this.#reapTasks.delete(generation.id);
    this.#reapCancels.delete(generation.id);
    this.#disposals.delete(generation.id);
    this.#disposing.delete(generation.id);
    this.#emit({
      type: "transport_generation.closed",
      generationId: generation.id,
      reason,
    });
    this.#notify();
  }

  /**
   * Await every pushed cleanup handle's intake stop, then dispose them all.
   *
   * All owners are attempted even if one fails; the first failure is rethrown
   * after every owner has been given its cleanup. A zero-owner generation (a
   * client with no provider ingress) resolves trivially.
   */
  /**
   * Take ownership of one owner's cleanup scope for a generation.
   *
   * Returns true when the generation is still live and the handle is stored for
   * its retirement; returns false when the generation is already closed or its
   * disposal has begun, in which case the manager immediately takes over the
   * late handle's cleanup under a tracked task (errors reported through the
   * manager's normal cleanup path) and never stores an orphan on a dropped
   * generation. The owner must stop installing when this returns false.
   */
  #registerDisposal(
    generation: TransportGeneration,
    handle: TransportGenerationDisposal,
  ): boolean {
    if (
      this.#closed || generation.state === "closed" ||
      this.#disposing.has(generation.id)
    ) {
      this.#trackLateCleanup(generation.id, handle);
      return false;
    }
    const handles = this.#disposals.get(generation.id) ?? [];
    handles.push(handle);
    this.#disposals.set(generation.id, handles);
    return true;
  }

  /** Run and observe a late owner cleanup, reporting failure without dropping it. */
  #trackLateCleanup(
    id: number,
    handle: TransportGenerationDisposal,
  ): void {
    const task = (async () => {
      try {
        await handle.dispose();
      } catch (error) {
        this.#log.warn(
          {
            error: error instanceof Error
              ? { name: error.name, message: error.message }
              : String(error),
            generationId: id,
          },
          "transport late owner cleanup failed",
        );
      }
    })();
    this.#retirements.add(task);
    void task.finally(() => this.#retirements.delete(task));
  }

  async #intakeStoppedAll(id: number): Promise<void> {
    const handles = this.#disposals.get(id) ?? [];
    await Promise.allSettled(
      handles.map(async (handle) => await handle.intakeStopped),
    );
  }

  async #disposeAll(id: number): Promise<void> {
    const handles = this.#disposals.get(id) ?? [];
    const results = await Promise.allSettled(
      handles.map(async (handle) => await handle.dispose()),
    );
    const failure = results.find(
      (result): result is PromiseRejectedResult => result.status === "rejected",
    );
    if (failure) throw failure.reason;
  }

  #onLeaseReleased(generation: TransportGeneration): void {
    if (generation.leaseCount === 0) {
      const waiters = this.#leaseZeroWaiters.get(generation.id);
      if (waiters) {
        this.#leaseZeroWaiters.delete(generation.id);
        for (const waiter of waiters) waiter();
      }
    }
    this.#notify();
  }

  #awaitLeaseZero(generation: TransportGeneration): Promise<void> {
    if (generation.leaseCount === 0) return Promise.resolve();
    return new Promise<void>((resolve) => {
      const waiters = this.#leaseZeroWaiters.get(generation.id) ?? new Set();
      waiters.add(resolve);
      this.#leaseZeroWaiters.set(generation.id, waiters);
    });
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
      // Keep a current generation that already serves the newest desired policy.
      const current = this.#currentGeneration();
      if (current?.ready && await this.#policyEqual(current, desired)) continue;
      // Reuse a ready generation that already represents the desired policy
      // (e.g. a reduction survivor), reinstalling its generic intake first.
      let reused: TransportGeneration | undefined;
      for (const generation of this.#readyGenerations()) {
        if (generation.ready && await this.#policyEqual(generation, desired)) {
          reused = generation;
          break;
        }
      }
      if (reused) {
        if (
          this.#currentId !== reused.id && await this.#ensureIntake(reused)
        ) {
          this.#activate(
            reused,
            this.#currentId === undefined
              ? "reduction_replacement"
              : "authorization_growth",
          );
        }
        continue;
      }
      // With no *ready* default, publish the newest ready generation that does
      // not exceed the desired policy as a provisional survivor instead of
      // opening a duplicate. A desired policy still showing the just-forced
      // wider authority narrows on the next pass, which then matches this
      // survivor; a doomed candidate for the superseded policy (whose reset
      // socket would be unowned) is never opened.
      if (!current?.ready) {
        let survivor: TransportGeneration | undefined;
        const ready = this.#readyGenerations().filter((generation) =>
          generation.ready
        );
        for (let i = ready.length - 1; i >= 0; i -= 1) {
          if (await this.#safeUnder(ready[i], desired)) {
            survivor = ready[i];
            break;
          }
        }
        if (survivor && await this.#ensureIntake(survivor)) {
          this.#activate(survivor, "reduction_replacement");
          continue;
        }
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

  /**
   * Whether a generation's admitted policy is content-equal to `desired`.
   *
   * Used to reuse a healthy generation across a context re-issue whose signed
   * digest changed but whose authorized policy did not. Digest equality is not
   * the contract; authorized-policy equality is.
   */
  async #policyEqual(
    generation: TransportGeneration,
    desired: TransportAuthorizationV1,
  ): Promise<boolean> {
    const relation = await classifyTransportAuthorizationWasm(
      generation.admittedPolicy,
      desired,
      this.#options.nowSeconds(),
    );
    return relation === "current";
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
      const desiredSnapshot = desired;
      // Install before activation: routing exists before the generation can be
      // selected as the default. A partial install retains whatever intake it
      // registered so accepted work is not detached.
      try {
        await this.#options.onPreActivate?.(generation);
      } catch (error) {
        // Owner cleanup is already pushed into the generation's registration, so
        // track/force-close the candidate without immediately closing the socket
        // out from under accepted work. A local install failure recovers from
        // newest authority; it is not a logical-connection terminal.
        await this.#rejectCandidate(generation, "recovery");
        this.#recordFailure(prepared.policyDigest);
        this.#emit({
          type: "transport_generation.open_failed",
          generationId: generation.id,
          reason: this.#lastReason,
          outcome: "error",
        });
        this.#log.warn(
          { error },
          "transport generation candidate failed to install",
        );
        return;
      }
      // Recheck the newest desired policy after intake install. The desired
      // snapshot is captured before digesting and re-verified by object identity
      // synchronously, with no await before activation, so an obsolete candidate
      // is never published.
      const latestDigest = await this.#latestDesiredDigest();
      if (
        this.#closed || this.#isGenerationClosed(generation) ||
        this.#options.desiredPolicy() !== desiredSnapshot ||
        latestDigest !== prepared.policyDigest
      ) {
        // The candidate was superseded by a newer authorization while it was
        // unpublished; it did not become the default.
        await this.#rejectCandidate(generation, "authorization_growth");
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
