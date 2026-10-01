// Per-connection live-session ownership and admission.
//
// One manager exists per actual authenticated NATS connection owner. Generated
// facades borrow it; separate connections receive separate managers. The
// manager owns ephemeral session records, admission permits, owner-control
// registrations and closed receipts in memory. There is no session database,
// KV entry or central relay.

import { liveConstants } from "../auth/protocol_wasm.ts";

const C = liveConstants();

/** Reasons the manager itself is unavailable for new sessions. */
export type ManagerUnavailable = "stopped" | "epoch_changed";

/** One closed-session receipt retained for idempotent control handling. */
export type ClosedReceipt = {
  readonly sessionId: string;
  readonly ownerConnectionId: string;
  readonly ownerSessionKey: string;
  readonly baseSubject: string;
  readonly reason: string;
  readonly cleanup: "complete" | "incomplete";
  readonly finalSeq: string;
  readonly receivedSeq: string;
  readonly consumedSeq: string;
  readonly expiresAtMs: number;
};

/** One tracked endpoint session that the manager can fence and close. */
export interface ManagedSession {
  /** Synchronously fence the session; no new yields or publications. */
  fence(): void;
  /** Await bounded cleanup. Resolves when the session is settled. */
  close(): Promise<void>;
}

/** Permit for one provider reservation; releases its slot on dispose. */
export class ProviderPermit {
  #release: (() => void) | undefined;

  constructor(release: () => void) {
    this.#release = release;
  }

  [Symbol.dispose](): void {
    this.#release?.();
    this.#release = undefined;
  }
}

/** Permit for one consumer session; releases its slot on dispose. */
export class ConsumerPermit {
  #release: (() => void) | undefined;
  #stop: (() => void) | undefined;
  #stopped = false;

  constructor(release: () => void) {
    this.#release = release;
  }

  /** Attach the accepted endpoint to this permit's logical connection lifetime. */
  attach(stop: () => void): void {
    if (this.#stopped) stop();
    else if (this.#release) this.#stop = stop;
  }

  /** Synchronously fence the endpoint and start its bounded detached cleanup. */
  stop(): void {
    this.#stopped = true;
    const stop = this.#stop;
    this.#stop = undefined;
    stop?.();
  }

  [Symbol.dispose](): void {
    const release = this.#release;
    this.#release = undefined;
    this.#stop = undefined;
    release?.();
  }
}

/** One live session manager for an actual authenticated connection owner. */
export class LiveSessionManager {
  #generation = 1;
  #stopped = false;
  #suspended = false;
  readonly #consumers = new Set<ConsumerPermit>();
  #providers: ProviderAdmission | undefined;
  /**
   * Every session this connection owns or recently owned, keyed by id: its
   * endpoint, the provider that owns it, and whether it is still active. A
   * terminated entry is retained until its bounded receipt expires so the
   * owning provider remains the only authoritative responder through the whole
   * receipt window.
   */
  readonly #sessions = new Map<
    string,
    {
      session: ManagedSession;
      owner: unknown;
      active: boolean;
      /** Exact protocol route this session's control frames are published to. */
      baseSubject: string;
    }
  >();
  readonly #receipts = new Map<string, ClosedReceipt>();
  /**
   * Providers currently owning intake on this connection, keyed by the exact
   * protocol route they serve. Ownership of a retained session fails over only
   * to a provider serving the same route, because a frame is published to that
   * route's subject: an unrelated provider would never receive it.
   */
  readonly #liveProviders = new Map<string, Set<unknown>>();

  #admission(): ProviderAdmission {
    return this.#providers ??= new ProviderAdmission(
      C.maxProviderSessions,
      C.maxProviderSessionsPerCaller,
    );
  }

  /** Return the manager's current generation. */
  generation(): number {
    return this.#generation;
  }

  /** Return whether the manager still accepts new opens. */
  isAvailable(): boolean {
    return !this.#stopped && !this.#suspended;
  }

  /** Return the typed reason the manager is unavailable, if any. */
  unavailableReason(): ManagerUnavailable | undefined {
    if (this.#stopped) return "stopped";
    if (this.#suspended) return "epoch_changed";
    return undefined;
  }

  /** Fence old sessions after transport loss; new opens wait for {@link resume}. */
  suspend(): void {
    this.#suspended = true;
    this.#generation += 1;
    for (const entry of this.#sessions.values()) {
      if (!entry.active) continue;
      entry.active = false;
      entry.session.fence();
    }
  }

  /** Permit new sessions on the current transport attachment. */
  resume(): void {
    this.#suspended = false;
  }

  /** Stop the manager: fence new opens and fence every owned session. */
  stop(): void {
    this.#stopped = true;
    this.#generation += 1;
    for (const permit of this.#consumers) permit.stop();
    for (const entry of this.#sessions.values()) {
      if (!entry.active) continue;
      entry.active = false;
      entry.session.fence();
    }
  }

  /** Fence then close every owned session within one shared grace. */
  async shutdown(graceMs: number = C.closeExchangeMs): Promise<void> {
    const sessions = [...this.#sessions.values()]
      .filter((entry) => entry.active)
      .map((entry) => entry.session);
    this.stop();
    await Promise.race([
      Promise.allSettled(sessions.map((session) => session.close())),
      new Promise((resolve) => setTimeout(resolve, graceMs)),
    ]);
  }

  /** Reserve one consumer session permit. */
  admitConsumer(): ConsumerPermit {
    if (!this.isAvailable() || this.#consumers.size >= C.maxConsumerSessions) {
      throw new Error("live consumer admission exhausted");
    }
    const permit = new ConsumerPermit(() => {
      this.#consumers.delete(permit);
    });
    this.#consumers.add(permit);
    return permit;
  }

  /** Reserve one provider admission permit for a specific caller. */
  admitProvider(
    consumerConnectionId: string,
    consumerSessionKey: string,
  ): ProviderPermit {
    if (!this.isAvailable()) {
      throw new Error("live provider admission exhausted");
    }
    const key = `${consumerConnectionId}:${consumerSessionKey}`;
    const release = this.#admission().admit(key);
    let released = false;
    return new ProviderPermit(() => {
      if (released) return;
      released = true;
      release();
    });
  }

  /**
   * Register one live provider for an exact route and return its deregistration.
   * A provider deregisters when its physical ingress is disposed.
   */
  registerProvider(provider: unknown, baseSubject: string): () => void {
    let providers = this.#liveProviders.get(baseSubject);
    if (!providers) {
      providers = new Set<unknown>();
      this.#liveProviders.set(baseSubject, providers);
    }
    providers.add(provider);
    let registered = true;
    return () => {
      if (!registered) return;
      registered = false;
      const set = this.#liveProviders.get(baseSubject);
      if (!set) return;
      set.delete(provider);
      if (set.size === 0) this.#liveProviders.delete(baseSubject);
    };
  }

  /**
   * Drop one session record entirely. Used to roll back a failed opening so a
   * rejected candidate never leaves a retained or owned record behind.
   */
  forgetSession(sessionId: string): void {
    this.#sessions.delete(sessionId);
  }

  /** Register one owned endpoint session and return its deregistration. */
  registerSession(
    sessionId: string,
    session: ManagedSession,
    owner: unknown,
    baseSubject: string,
  ): () => void {
    let registered = true;
    const deregister = (): void => {
      if (!registered) return;
      registered = false;
      const entry = this.#sessions.get(sessionId);
      if (entry) entry.active = false;
    };
    this.#sessions.set(sessionId, {
      session,
      owner,
      active: true,
      baseSubject,
    });
    if (this.#stopped || this.#suspended) {
      session.fence();
    }
    return deregister;
  }

  /**
   * The provider that is authoritative for a session id across its whole
   * lifetime: while active, and through the retained terminal receipt window.
   * Returns `undefined` only for a session this connection never owned, which is
   * the genuine unknown-session case.
   */
  ownerOf(sessionId: string): unknown {
    // Expire on read: an expired receipt must never leave a stale owner
    // authoritative, and a retained entry must be pruned exactly when its
    // receipt expires.
    this.#expireReceipts(performance.now());
    const entry = this.#sessions.get(sessionId);
    if (!entry) return undefined;
    // Only a provider serving this session's exact route can receive and answer
    // its control frame.
    const providers = this.#liveProviders.get(entry.baseSubject);
    if (providers?.has(entry.owner)) return entry.owner;
    // The physical owner is gone. Elect a surviving live provider on the same
    // route so a retained receipt still has exactly one responder; with no
    // same-route survivor the record is orphaned and dropped rather than
    // answered by nobody (or by an unrelated route).
    const successor = providers?.values().next().value;
    if (successor === undefined) {
      this.#sessions.delete(sessionId);
      return undefined;
    }
    entry.owner = successor;
    return successor;
  }

  /** Record one bounded, expiring closed-session receipt. */
  insertReceipt(receipt: Omit<ClosedReceipt, "expiresAtMs">): void {
    const now = performance.now();
    this.#expireReceipts(now);
    this.#receipts.set(receipt.sessionId, {
      ...receipt,
      expiresAtMs: now + C.tombstoneMs,
    });
    while (this.#receipts.size > C.maxTombstones) {
      const oldest = this.#receipts.keys().next().value;
      if (oldest === undefined) break;
      this.#receipts.delete(oldest);
      // A cap eviction ends the ownership window too; otherwise the retained
      // inactive session would never be pruned.
      const entry = this.#sessions.get(oldest);
      if (entry && !entry.active) this.#sessions.delete(oldest);
    }
  }

  /** Find one unexpired closed-session receipt. */
  receipt(sessionId: string): ClosedReceipt | undefined {
    const now = performance.now();
    this.#expireReceipts(now);
    const receipt = this.#receipts.get(sessionId);
    return receipt && receipt.expiresAtMs > now ? receipt : undefined;
  }

  #expireReceipts(nowMs: number): void {
    for (const [sessionId, receipt] of this.#receipts) {
      if (receipt.expiresAtMs > nowMs) continue;
      this.#receipts.delete(sessionId);
      const entry = this.#sessions.get(sessionId);
      // Drop the retained owner only once the terminal receipt has expired.
      if (entry && !entry.active) this.#sessions.delete(sessionId);
    }
  }

  /** Count currently retained consumer sessions (diagnostics/tests). */
  consumerCount(): number {
    return this.#consumers.size;
  }

  /** Count currently retained provider sessions (diagnostics/tests). */
  providerCount(): number {
    return this.#admission().total();
  }
}

class ProviderAdmission {
  #total = 0;
  readonly #perCaller = new Map<string, number>();
  readonly #maxTotal: number;
  readonly #maxPerCaller: number;

  constructor(maxTotal: number, maxPerCaller: number) {
    this.#maxTotal = maxTotal;
    this.#maxPerCaller = maxPerCaller;
  }

  total(): number {
    return this.#total;
  }

  admit(callerKey: string): () => void {
    if (this.#total >= this.#maxTotal) {
      throw new Error("live provider admission exhausted");
    }
    const current = this.#perCaller.get(callerKey) ?? 0;
    if (current >= this.#maxPerCaller) {
      throw new Error("live provider admission exhausted for caller");
    }
    this.#total += 1;
    this.#perCaller.set(callerKey, current + 1);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.#total = Math.max(0, this.#total - 1);
      const next = (this.#perCaller.get(callerKey) ?? 1) - 1;
      if (next <= 0) this.#perCaller.delete(callerKey);
      else this.#perCaller.set(callerKey, next);
    };
  }
}
