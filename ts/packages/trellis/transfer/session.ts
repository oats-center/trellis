import {
  headers,
  type Msg,
  type MsgHdrs,
  type Subscription,
} from "@nats-io/nats-core";
import { QueuedIteratorImpl } from "@nats-io/nats-core/internal";
import { ulid } from "ulid";
import { buildProofInput } from "../auth/proof.ts";
import { base64urlEncode, sha256 } from "../auth/utils.ts";
import type { TrellisAuth } from "../session.ts";
import { TransferError } from "../errors/TransferError.ts";
import { TransportError } from "../errors/TransportError.ts";
import { LiveAuthorityGuard } from "../live/authority.ts";
import type { TransportLease } from "../transport/generations.ts";
import type { TransferGrant } from "../transfer.ts";
import { type CreditPolicy, CreditScheduler } from "../data_plane/credit.ts";
import { recordCatalogCounter } from "../telemetry/mod.ts";
import {
  transferConstants,
  type TransferFrameDescriptor,
  transferFrameDigest,
  transferServerProofDigest,
  type TransferTerminalWire,
  transferVerifyServerProof,
} from "../auth/protocol_wasm.ts";

/** Owned transport/authority lifetime for a single pinned transfer. @internal */
export class TransferSession {
  readonly abort = new AbortController();
  readonly tasks = new Set<Promise<void>>();
  readonly subscriptions: Subscription[] = [];
  readonly guards: LiveAuthorityGuard[] = [];
  #timer: ReturnType<typeof setTimeout>;
  #authorityTimer: ReturnType<typeof setTimeout> | undefined;
  #status: AsyncIterator<unknown>;
  #stopStatus: () => void;
  #unsubscribe: (() => void) | undefined;
  #finished = false;
  #released = false;
  #cleanup: Promise<void> | undefined;
  peerGuard: LiveAuthorityGuard | undefined;

  constructor(
    readonly grant: TransferGrant,
    readonly lease: TransportLease,
    readonly auth: TrellisAuth,
    readonly provider: boolean,
  ) {
    const statusQueue = lease.nc.status();
    if (!(statusQueue instanceof QueuedIteratorImpl)) {
      lease.release();
      throw new Error("transfer requires cancellable NATS status observation");
    }
    this.#stopStatus = () => statusQueue.stop();
    this.#timer = setTimeout(
      () => this.fail(new Error("transfer expired")),
      Math.max(0, Date.parse(grant.expiresAt) - Date.now()),
    );
    const status = statusQueue[Symbol.asyncIterator]();
    this.#status = status;
    this.run((async () => {
      while (!this.#finished) {
        const event = await transferWait(
          status.next(),
          this.abort.signal,
        );
        if (event.done) throw new Error("transfer connection closed");
        if (
          (event.value as { type?: string }).type === "disconnect" ||
          (event.value as { type?: string }).type === "close"
        ) {
          throw new Error("transfer physical connection lost");
        }
      }
    })());
  }

  /** Retain local evidence and enforce the grant's logical connection identity. */
  async retainLocal(): Promise<void> {
    const cache = this.auth.authorizationProviderCache;
    if (!cache) throw new Error("transfer authorization cache unavailable");
    const digest = typeof this.auth.contextDigest === "function"
      ? this.auth.contextDigest()
      : this.auth.contextDigest;
    if (!digest) throw new Error("transfer authorization context unavailable");
    const guard = await transferWait(
      LiveAuthorityGuard.retain(cache, digest, {
        kind: "local-provider",
      }).then((guard) => {
        if (this.abort.signal.aborted) {
          guard.release();
          this.throwIfAborted();
        }
        return guard;
      }),
      this.abort.signal,
    );
    if (this.abort.signal.aborted) {
      guard.release();
      this.throwIfAborted();
    }
    const identity = this.provider ? this.grant.provider : this.grant.consumer;
    if (
      guard.identity.sessionKey !== identity.sessionKey ||
      guard.identity.connectionId !== identity.connectionId
    ) {
      guard.release();
      throw new Error("transfer local identity mismatch");
    }
    this.guards.push(guard);
    this.#unsubscribe = guard.subscribeChanges(() => {
      this.run((async () => {
        const lost = await this.#reconcileLocal();
        if (lost) throw new Error(`transfer authority lost: ${lost}`);
        if (this.peerGuard?.checkNow()) {
          throw new Error("transfer peer authority lost");
        }
        this.refreshDeadline();
      })());
    });
    this.refreshDeadline();
  }

  /** Own a task and turn failures into one terminal transfer error. */
  run(work: Promise<void>): void {
    let tracked: Promise<void>;
    tracked = work.catch((cause) => this.fail(cause)).finally(() =>
      this.tasks.delete(tracked)
    );
    this.tasks.add(tracked);
  }

  /** Fail pending stream/capacity waits and immediately stop wire intake. */
  fail(cause: unknown): void {
    if (this.abort.signal.aborted || this.#finished) return;
    this.abort.abort(
      cause instanceof TransferError || cause instanceof TransportError
        ? cause
        : new TransferError({ operation: "transfer", cause }),
    );
    for (const sub of this.subscriptions) sub.unsubscribe();
    if (!this.provider) this.finish();
  }

  /** Read current retained local authority, without migrating the carrier. */
  async contextDigest(): Promise<string> {
    this.throwIfAborted();
    const guard = this.guards[0];
    if (!guard || await this.#reconcileLocal()) {
      throw new Error("transfer local authority lost");
    }
    this.throwIfAborted();
    return guard.contextDigest;
  }

  throwIfAborted(): void {
    if (this.abort.signal.aborted) throw this.abort.signal.reason;
  }

  async #reconcileLocal(): Promise<string | undefined> {
    const guard = this.guards[0];
    if (!guard) return "coverage_lost";
    return await transferWait(
      guard.reconcile().finally(() => {
        if (this.abort.signal.aborted) guard.release();
      }),
      this.abort.signal,
    );
  }

  /** Authenticate an exact one-way request with the session's fixed signal reply. */
  async requestHeaders(
    subject: string,
    payload: Uint8Array,
    seq?: bigint,
    control: TransferFrameDescriptor["kind"] = "data",
  ): Promise<MsgHdrs> {
    const contextDigest = await this.contextDigest();
    const proofPayload = seq === undefined ? payload : transferFrameDigest(
      {
        transferId: this.grant.transferId,
        direction: this.grant.direction,
        sequence: seq.toString(),
        kind: control,
      },
      payload,
    );
    const iat = this.auth.currentIat?.() ?? Math.floor(Date.now() / 1000);
    const requestId = ulid();
    const proof = await this.auth.sign(
      await sha256(buildProofInput(
        contextDigest,
        subject,
        this.grant.signalSubject,
        await sha256(proofPayload),
        iat,
        requestId,
      )),
    );
    const result = headers();
    result.set("authorization-context", contextDigest);
    result.set("session-key", this.auth.sessionKey);
    result.set("proof", base64urlEncode(proof));
    result.set("iat", String(iat));
    result.set("request-id", requestId);
    if (seq !== undefined) {
      result.set(transferConstants().sequenceHeader, seq.toString());
      result.set(transferConstants().controlHeader, control);
    }
    return result;
  }

  /** Sign provider data and signals in the canonical Transfer-specific domain. */
  async serverHeaders(
    subject: string,
    payload: Uint8Array,
    seq: string,
    control: TransferFrameDescriptor["kind"],
    terminal?: TransferTerminalWire,
    allowAfterAbort = false,
  ): Promise<MsgHdrs> {
    const local = this.guards[0];
    if (allowAfterAbort && (!local || local.checkNow())) {
      throw new Error("transfer authority unavailable for terminal signal");
    }
    const contextDigest = allowAfterAbort && local
      ? local.contextDigest
      : await this.contextDigest();
    const proof = await this.auth.sign(transferServerProofDigest(
      contextDigest,
      subject,
      {
        transferId: this.grant.transferId,
        direction: this.grant.direction,
        sequence: seq,
        kind: control,
        ...(terminal ? { terminal } : {}),
      },
      payload,
    ));
    const result = headers();
    result.set("authorization-context", contextDigest);
    result.set("session-key", this.auth.sessionKey);
    const C = transferConstants();
    result.set(C.proofHeader, base64urlEncode(proof));
    result.set(C.sequenceHeader, seq);
    result.set(C.controlHeader, control);
    if (terminal) result.set(C.terminalHeader, JSON.stringify(terminal));
    return result;
  }

  /** Verify the actual subject/proof and retain the exact provider identity. */
  async verifyProvider(
    msg: Msg,
    seq: string,
    control: TransferFrameDescriptor["kind"],
    terminal?: TransferTerminalWire,
  ): Promise<void> {
    const single = (name: string) => {
      const values = msg.headers?.values(name);
      if (!values || values.length !== 1 || !values[0]) {
        throw new Error(`invalid transfer ${name}`);
      }
      return values[0];
    };
    const contextDigest = single("authorization-context");
    const sessionKey = single("session-key");
    if (sessionKey !== this.grant.provider.sessionKey) {
      throw new Error("transfer provider signer mismatch");
    }
    const C = transferConstants();
    if (
      single(C.sequenceHeader) !== seq || single(C.controlHeader) !== control ||
      (!terminal && msg.headers?.values(C.terminalHeader)?.length)
    ) throw new Error("transfer provider frame descriptor mismatch");
    transferVerifyServerProof(
      single(C.proofHeader),
      contextDigest,
      msg.subject,
      {
        transferId: this.grant.transferId,
        direction: this.grant.direction,
        sequence: seq,
        kind: control,
        ...(terminal ? { terminal } : {}),
      },
      msg.data,
      sessionKey,
    );
    const cache = this.auth.authorizationProviderCache;
    if (!cache) throw new Error("transfer authorization cache unavailable");
    if (this.peerGuard?.contextDigest === contextDigest) {
      if (this.peerGuard.checkNow()) {
        throw new Error("transfer provider authority lost");
      }
      return;
    }
    const verified = await transferWait(
      cache.resolveContext(contextDigest),
      this.abort.signal,
    );
    this.throwIfAborted();
    if (
      verified.context.connectionId !== this.grant.provider.connectionId ||
      verified.context.sessionKey !== sessionKey
    ) throw new Error("transfer provider identity mismatch");
    const candidate = await transferWait(
      LiveAuthorityGuard.retain(cache, contextDigest, {
        kind: "peer-provider",
        expected: {
          connectionId: verified.context.connectionId,
          sessionKey,
          principalId: verified.context.principalId,
          participantId: verified.context.participantId,
          ...(verified.context.deploymentId
            ? { deploymentId: verified.context.deploymentId }
            : {}),
          ...(verified.context.instanceId
            ? { instanceId: verified.context.instanceId }
            : {}),
        },
      }).then((candidate) => {
        if (this.abort.signal.aborted) {
          candidate.release();
          this.throwIfAborted();
        }
        return candidate;
      }),
      this.abort.signal,
    );
    if (this.abort.signal.aborted) {
      candidate.release();
      this.throwIfAborted();
    }
    if (this.peerGuard) {
      // Replacement must preserve the whole independently verified identity.
      candidate.release();
      const replacement = await transferWait(
        this.peerGuard.prepareReplacement(contextDigest).then((replacement) => {
          if (this.abort.signal.aborted) {
            replacement.release();
            this.throwIfAborted();
          }
          return replacement;
        }),
        this.abort.signal,
      );
      const lost = this.peerGuard.commitReplacement(replacement);
      replacement.release();
      if (lost) throw new Error(`transfer provider authority lost: ${lost}`);
    } else {
      this.peerGuard = candidate;
      this.guards.push(candidate);
    }
    this.throwIfAborted();
    this.refreshDeadline();
  }

  /** Bound idle waits by the exact retained authority expiration as well as grant TTL. */
  refreshDeadline(): void {
    if (this.#finished || this.abort.signal.aborted) return;
    if (this.#authorityTimer !== undefined) clearTimeout(this.#authorityTimer);
    const now = this.auth.authorizationProviderCache?.liveNowSeconds() ??
      Date.now() / 1000;
    const expires = Math.min(
      ...this.guards.map((guard) => guard.expiresAtSeconds),
    );
    if (!Number.isFinite(expires)) return;
    this.#authorityTimer = setTimeout(() => {
      this.#authorityTimer = undefined;
      this.run((async () => {
        if (
          await this.#reconcileLocal() ||
          this.guards.some((guard) => guard.checkNow())
        ) throw new Error("transfer authority expired");
        this.refreshDeadline();
      })());
    }, Math.max(0, (expires - now) * 1000));
  }

  /** Release exact subscriptions, retained evidence and the pinned generation. */
  finish(): void {
    if (this.#released) return;
    this.#released = true;
    this.#finished = true;
    if (!this.abort.signal.aborted) {
      this.abort.abort(new Error("transfer released"));
    }
    clearTimeout(this.#timer);
    if (this.#authorityTimer !== undefined) clearTimeout(this.#authorityTimer);
    this.#unsubscribe?.();
    for (const sub of this.subscriptions) sub.unsubscribe();
    this.#stopStatus();
    this.run(Promise.resolve(this.#status.return?.()).then(() => {}));
    for (const guard of this.guards) guard.release();
    this.lease.release();
    // Start the join but never await it from a failing task on this same lane.
    this.#cleanup = this.#joinTasks();
  }

  /** Join endpoint-owned loops after their waiters and subscriptions are stopped. */
  async join(): Promise<void> {
    await (this.#cleanup ?? this.#joinTasks());
  }

  async #joinTasks(): Promise<void> {
    while (this.tasks.size) await Promise.all([...this.tasks]);
  }
}

/** Latest-value cumulative credit with one owned timer and one publication lane. */
export class TransferCredit {
  readonly #scheduler: CreditScheduler;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #received = 0n;
  #consumed = 0n;
  #bytes = 0;
  #reportedSeq = 0n;
  #reportedBytes = 0;
  #work: Promise<void> | undefined;
  #closed = false;

  constructor(
    readonly session: TransferSession,
    policy: CreditPolicy,
    readonly send: (received: bigint, consumed: bigint) => Promise<void>,
  ) {
    this.#scheduler = new CreditScheduler(policy);
  }

  /** Schedule credit only after downstream ownership handoff. */
  note(received: bigint, consumed: bigint, bytes: number): void {
    if (this.#closed) return;
    this.#received = received;
    this.#consumed = consumed;
    this.#bytes = bytes;
    if (consumed <= this.#reportedSeq) return;
    this.#scheduler.notePending(
      performance.now(),
      Number(consumed - this.#reportedSeq),
      bytes - this.#reportedBytes,
    );
    this.#arm();
  }

  #arm(): void {
    if (this.#closed || this.#work) return;
    if (this.#timer !== undefined) clearTimeout(this.#timer);
    const due = this.#scheduler.nextDue();
    if (due === undefined) return;
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      this.session.run(this.flush());
    }, Math.max(0, due - performance.now()));
  }

  /** Force the exact latest cumulative cursor before terminal settlement. */
  async flush(): Promise<void> {
    if (this.#closed) return;
    if (this.#work) {
      await this.#work;
      return await this.flush();
    }
    if (this.#timer !== undefined) clearTimeout(this.#timer);
    this.#timer = undefined;
    if (this.#consumed <= this.#reportedSeq) return;
    const received = this.#received;
    const consumed = this.#consumed;
    const bytes = this.#bytes;
    this.#scheduler.clear();
    this.#work = this.send(received, consumed);
    try {
      await this.#work;
      this.#reportedSeq = consumed;
      this.#reportedBytes = bytes;
      recordCatalogCounter("trellis.transfer.credit.controls", 1, {
        "trellis.direction": this.session.grant.direction === "send"
          ? "upload"
          : "download",
      });
    } finally {
      this.#work = undefined;
      if (this.#consumed > this.#reportedSeq) {
        this.#scheduler.notePending(
          performance.now(),
          Number(this.#consumed - this.#reportedSeq),
          this.#bytes - this.#reportedBytes,
        );
        this.#arm();
      }
    }
  }

  /** Release the timer; the session still owns any in-flight publication. */
  close(): void {
    this.#closed = true;
    if (this.#timer !== undefined) clearTimeout(this.#timer);
    this.#timer = undefined;
    this.#scheduler.clear();
  }
}

/** Wait on a real task while cancellation releases the waiter and its listener. */
export function transferWait<T>(
  promise: PromiseLike<T>,
  signal: AbortSignal,
): Promise<T> {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise<T>((resolve, reject) => {
    const aborted = () => {
      signal.removeEventListener("abort", aborted);
      reject(signal.reason);
    };
    signal.addEventListener("abort", aborted, { once: true });
    Promise.resolve(promise).then(
      (value) => {
        signal.removeEventListener("abort", aborted);
        resolve(value);
      },
      (error) => {
        signal.removeEventListener("abort", aborted);
        reject(error);
      },
    );
  });
}
