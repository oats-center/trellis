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

  constructor(release: () => void) {
    this.#release = release;
  }

  [Symbol.dispose](): void {
    this.#release?.();
    this.#release = undefined;
  }
}

/** One live session manager for an actual authenticated connection owner. */
export class LiveSessionManager {
  #generation = 1;
  #stopped = false;
  #suspended = false;
  #consumers = 0;
  #providers: ProviderAdmission | undefined;
  readonly #sessions = new Map<ManagedSession, () => void>();
  readonly #receipts = new Map<string, ClosedReceipt>();

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
    for (const [session, dispose] of this.#sessions) {
      session.fence();
      dispose();
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
    for (const [session, dispose] of this.#sessions) {
      session.fence();
      dispose();
    }
  }

  /** Fence then close every owned session within one shared grace. */
  async shutdown(graceMs: number = C.closeExchangeMs): Promise<void> {
    this.stop();
    const sessions = [...this.#sessions.keys()];
    await Promise.race([
      Promise.allSettled(sessions.map((session) => session.close())),
      new Promise((resolve) => setTimeout(resolve, graceMs)),
    ]);
  }

  /** Reserve one consumer session permit. */
  admitConsumer(): ConsumerPermit {
    if (!this.isAvailable() || this.#consumers >= C.maxConsumerSessions) {
      throw new Error("live consumer admission exhausted");
    }
    this.#consumers += 1;
    let released = false;
    return new ConsumerPermit(() => {
      if (released) return;
      released = true;
      this.#consumers = Math.max(0, this.#consumers - 1);
    });
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

  /** Register one owned endpoint session and return its deregistration. */
  registerSession(session: ManagedSession): () => void {
    let registered = true;
    const deregister = (): void => {
      if (!registered) return;
      registered = false;
      this.#sessions.delete(session);
    };
    this.#sessions.set(session, deregister);
    if (this.#stopped || this.#suspended) {
      session.fence();
    }
    return deregister;
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
      if (receipt.expiresAtMs <= nowMs) this.#receipts.delete(sessionId);
    }
  }

  /** Count currently retained consumer sessions (diagnostics/tests). */
  consumerCount(): number {
    return this.#consumers;
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
