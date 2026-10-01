/**
 * Observation and readiness barrier for the test transport proxy.
 *
 * The gate watches the real NATS client protocol on each proxied connection so
 * a test can (a) hold one generation's post-subscription readiness flush, (b)
 * prove which connection a real served RPC was delivered to, (c) read the
 * exact `authorization-context` an outbound request was signed with, and (d)
 * withhold exactly one request's server reply on the wire. It never inspects or
 * records credentials: `CONNECT` payloads are ignored and only the logical
 * request header used for authorization binding is read.
 */

import type { NatsOp } from "./nats_wire.ts";

/** Serialized byte writer shared by the forward pump and the barrier release. */
export type SerialWriter = {
  write(bytes: Uint8Array): Promise<void>;
};

/** Observed state for one proxied physical connection. */
export type ConnectionObservation = {
  id: number;
  closed: boolean;
  subs: { subject: string; sid: string; queue?: string }[];
  outboundContexts: { subject: string; context?: string }[];
  deliveries: { subject: string; sid: string }[];
};

type ConnectionState = ConnectionObservation & {
  writer: SerialWriter;
  heldPongs: Uint8Array[];
};

type DeliveryWaiter = {
  connectionId: number | undefined;
  subject: string;
  resolve: (value: { connectionId: number; sid: string }) => void;
};

/**
 * Synchronous predicate over a real server reply body. A hold armed with a
 * matcher retains the first candidate reply the predicate accepts instead of
 * the first candidate request's inbox. It runs inline on the proxy pump, so it
 * must not suspend. A thrown error is an owner-visible failure, not a
 * non-match: it disarms that hold and rejects its `held` promise with the
 * original cause while the reply itself is forwarded unchanged.
 */
export type ReplyBodyMatcher = (body: Uint8Array) => boolean;

/** Observed request whose server reply is being withheld. */
export type ResponseHoldObservation = {
  /** Physical proxied connection that carried the request and its reply. */
  connectionId: number;
  /** Exact request subject whose reply is held. */
  requestSubject: string;
};

/** Handle for one armed one-shot server-reply hold. */
export type ResponseHoldBarrier = {
  /**
   * Resolves once the matched request's first real reply frame has been retained
   * on the wire, so after this resolves the reply is provably withheld. Await it
   * before asserting that unrelated traffic still flows. Rejects with the
   * original cause when a matcher-armed hold's predicate throws.
   */
  readonly held: Promise<ResponseHoldObservation>;
  /**
   * Forward every retained reply frame on its original connection, or discard
   * them if that physical connection has closed, then stop holding. Resolves
   * after any retained bytes have been written. Idempotent:
   * releasing an already held, failed, or released hold never disturbs a newer
   * one.
   */
  release(): Promise<void>;
};

/** In-flight request whose reply a matcher-armed hold is still considering. */
type ResponseCandidate = {
  requestSubject: string;
  inbox: string;
  connectionId: number;
  writer: SerialWriter;
};

type ResponseHold = {
  subjectPrefix: string;
  connectionId: number | undefined;
  matcher: ReplyBodyMatcher | undefined;
  released: boolean;
  requestSubject: string | undefined;
  inbox: string | undefined;
  matchedConnectionId: number | undefined;
  writer: SerialWriter | undefined;
  candidates: ResponseCandidate[];
  frames: Uint8Array[];
  resolveHeld: (observation: ResponseHoldObservation) => void;
  rejectHeld: (cause: unknown) => void;
};

/** Readiness gate and connection observer for one native transport proxy. */
export class NativeTransportGate {
  readonly #connections = new Map<number, ConnectionState>();
  readonly #deliveryWaiters: DeliveryWaiter[] = [];
  readonly #closedWaiters: { connectionId: number; resolve: () => void }[] = [];
  #barrier = Promise.withResolvers<number>();
  #nextId = 1;
  #firstEligibleConnectionId = 1;
  #armed = false;
  #routeSuffix: string | undefined;
  #gatedConnectionId: number | undefined;
  #holding = false;
  #released = false;
  #responseHold: ResponseHold | undefined;

  /** Allocate the next proxied connection id. */
  connectionOpened(writer: SerialWriter): number {
    const id = this.#nextId++;
    this.#connections.set(id, {
      id,
      closed: false,
      subs: [],
      outboundContexts: [],
      deliveries: [],
      writer,
      heldPongs: [],
    });
    return id;
  }

  /** Mark a proxied connection closed and release any waiter on it. */
  connectionClosed(id: number): void {
    const connection = this.#connections.get(id);
    if (connection) connection.closed = true;
    for (const waiter of [...this.#closedWaiters]) {
      if (waiter.connectionId === id) {
        this.#closedWaiters.splice(this.#closedWaiters.indexOf(waiter), 1);
        waiter.resolve();
      }
    }
  }

  /**
   * Arm the readiness barrier: the next connection opened after this call that
   * subscribes a route ending in `routeSuffix` holds its first server `PONG`
   * after that subscription. That PONG acknowledges the generation's
   * post-subscription flush, so withholding it keeps the candidate unpublished
   * while its routes are already broker-live. Release before arming a later
   * generation; arming while a readiness hold is still active is rejected.
   */
  arm(routeSuffix: string): void {
    if (this.#armed && !this.#released) {
      throw new Error(
        "a readiness hold is already armed; release it before arming another",
      );
    }
    if (this.#armed) this.#barrier = Promise.withResolvers<number>();
    this.#firstEligibleConnectionId = this.#nextId;
    this.#gatedConnectionId = undefined;
    this.#holding = false;
    this.#released = false;
    this.#armed = true;
    this.#routeSuffix = routeSuffix;
  }

  /** Resolves with the connection id once the readiness barrier is held. */
  barrierHeld(): Promise<number> {
    return this.#barrier.promise;
  }

  /**
   * Arm a one-shot hold on the server reply for the next real `PUB`/`HPUB`
   * request whose subject starts with `subjectPrefix`, optionally restricted to
   * one proxied `connectionId`. The request's actual reply inbox is taken from
   * the wire; only real server `MSG`/`HMSG` frames carrying that exact inbox on
   * the same physical connection are withheld, so unrelated traffic, heartbeats,
   * and the existing readiness barrier keep flowing. Arming while a hold is
   * still active, or with an empty prefix, is rejected.
   *
   * With a `matcher`, every matching request becomes a candidate instead of the
   * first one being selected outright: the first real server reply whose body
   * the matcher accepts selects its candidate's inbox and physical connection
   * and is withheld, while non-matching replies keep flowing. Concurrent
   * candidate requests issued before any reply all stay eligible, so selection
   * is by reply body rather than by whichever request was published first. A
   * matcher throw is an owner-visible failure: this hold alone synchronously
   * disarms, its candidate requests are cleared, `held` rejects with the
   * original cause, and that reply is forwarded unchanged so the physical
   * connection and unrelated traffic keep flowing.
   */
  armResponseHold(
    subjectPrefix: string,
    connectionId?: number,
    matcher?: ReplyBodyMatcher,
  ): ResponseHoldBarrier {
    if (subjectPrefix.length === 0) {
      throw new Error("a response hold requires a non-empty subject prefix");
    }
    if (this.#responseHold !== undefined) {
      throw new Error(
        "a response hold is already armed; release it before arming another",
      );
    }
    const { promise, resolve, reject } = Promise.withResolvers<
      ResponseHoldObservation
    >();
    // A matcher failure can reject `held` before the caller has attached its
    // own handler; absorbing that rejection here keeps the failure owner-visible
    // through `held` (which still rejects for whoever awaits it) instead of
    // surfacing as an unrelated unhandled background rejection.
    promise.catch(() => {});
    const hold: ResponseHold = {
      subjectPrefix,
      connectionId,
      matcher,
      released: false,
      requestSubject: undefined,
      inbox: undefined,
      matchedConnectionId: undefined,
      writer: undefined,
      candidates: [],
      frames: [],
      resolveHeld: resolve,
      rejectHeld: reject,
    };
    this.#responseHold = hold;
    return {
      held: promise,
      release: () => this.#releaseResponseHold(hold),
    };
  }

  async #releaseResponseHold(hold: ResponseHold): Promise<void> {
    if (hold.released) return;
    hold.released = true;
    hold.candidates = [];
    if (this.#responseHold === hold) this.#responseHold = undefined;
    const writer = hold.writer;
    const frames = hold.frames;
    hold.frames = [];
    if (writer === undefined) return;
    if (
      hold.matchedConnectionId !== undefined &&
      this.#connections.get(hold.matchedConnectionId)?.closed
    ) return;
    // Await the tracked writes so a failure surfaces to the caller instead of
    // becoming an unhandled rejection.
    await Promise.all(frames.map((bytes) => writer.write(bytes)));
  }

  /** Forward held `PONG`s, or discard them on a closed connection, and stop holding. */
  async release(): Promise<void> {
    this.#holding = false;
    this.#released = true;
    const connection = this.#gatedConnectionId === undefined
      ? undefined
      : this.#connections.get(this.#gatedConnectionId);
    if (connection === undefined) return;
    const held = connection.heldPongs;
    connection.heldPongs = [];
    if (connection.closed) return;
    // Await the tracked writes so a failure surfaces to the caller instead of
    // becoming an unhandled rejection.
    await Promise.all(held.map((bytes) => connection.writer.write(bytes)));
  }

  /** Record one client→server frame; returns whether the proxy must withhold it. */
  onClientFrame(id: number, op: NatsOp, raw: Uint8Array): boolean {
    const connection = this.#connections.get(id);
    if (!connection) return false;
    void raw;
    switch (op.kind) {
      case "sub":
        connection.subs.push({
          subject: op.subject,
          sid: op.sid,
          ...(op.queue === undefined ? {} : { queue: op.queue }),
        });
        if (
          this.#armed && this.#gatedConnectionId === undefined &&
          id >= this.#firstEligibleConnectionId &&
          !this.#released &&
          (this.#routeSuffix === undefined ||
            op.subject.endsWith(this.#routeSuffix))
        ) {
          this.#gatedConnectionId = id;
        }
        break;
      case "pub": {
        connection.outboundContexts.push({
          subject: op.subject,
          context: op.headers?.["authorization-context"],
        });
        const hold = this.#responseHold;
        if (
          hold !== undefined && !hold.released && hold.inbox === undefined &&
          op.reply !== undefined &&
          (hold.connectionId === undefined || hold.connectionId === id) &&
          op.subject.startsWith(hold.subjectPrefix)
        ) {
          if (hold.matcher === undefined) {
            hold.requestSubject = op.subject;
            hold.inbox = op.reply;
            hold.matchedConnectionId = id;
            hold.writer = connection.writer;
          } else {
            // Keep every in-flight candidate request eligible: concurrent
            // requests can all be published before any reply arrives, so a
            // single first-inbox choice would miss the later ones.
            hold.candidates.push({
              requestSubject: op.subject,
              inbox: op.reply,
              connectionId: id,
              writer: connection.writer,
            });
          }
        }
        break;
      }
      default:
        break;
    }
    return false;
  }

  /**
   * Record one server→client frame. Returns whether the proxy must withhold it
   * until {@link release}.
   */
  onServerFrame(id: number, op: NatsOp, raw: Uint8Array): boolean {
    const connection = this.#connections.get(id);
    if (!connection) return false;
    switch (op.kind) {
      case "msg": {
        connection.deliveries.push({ subject: op.subject, sid: op.sid });
        for (const waiter of [...this.#deliveryWaiters]) {
          if (
            (waiter.connectionId === undefined ||
              waiter.connectionId === id) &&
            op.subject.includes(waiter.subject)
          ) {
            this.#deliveryWaiters.splice(
              this.#deliveryWaiters.indexOf(waiter),
              1,
            );
            waiter.resolve({ connectionId: id, sid: op.sid });
          }
        }
        const hold = this.#responseHold;
        if (hold === undefined || hold.released) return false;
        if (hold.matcher !== undefined && hold.inbox === undefined) {
          const candidate = hold.candidates.find((entry) =>
            entry.connectionId === id && entry.inbox === op.subject
          );
          // A reply to a request this hold never tracked keeps flowing.
          if (candidate === undefined) return false;
          let accepted: boolean;
          try {
            accepted = hold.matcher(op.body);
          } catch (cause) {
            // The predicate is synchronous owner code; a throw is a
            // deliberately reported failure, not a non-match. Disarm only this
            // hold, clear its candidate references, surface the original cause
            // through `held`, and forward the actual reply unchanged.
            hold.released = true;
            hold.candidates = [];
            hold.frames = [];
            if (this.#responseHold === hold) this.#responseHold = undefined;
            hold.rejectHeld(cause);
            return false;
          }
          // A reply the matcher rejects keeps flowing and leaves its candidate
          // eligible.
          if (!accepted) return false;
          hold.requestSubject = candidate.requestSubject;
          hold.inbox = candidate.inbox;
          hold.matchedConnectionId = candidate.connectionId;
          hold.writer = candidate.writer;
          hold.candidates = [];
        }
        if (
          hold.inbox !== undefined && hold.matchedConnectionId === id &&
          hold.inbox === op.subject
        ) {
          hold.frames.push(raw);
          hold.resolveHeld({
            connectionId: id,
            requestSubject: hold.requestSubject!,
          });
          return true;
        }
        return false;
      }
      case "pong": {
        if (
          this.#armed && !this.#released && this.#gatedConnectionId === id &&
          connection.subs.length > 0
        ) {
          connection.heldPongs.push(raw);
          if (!this.#holding) {
            this.#holding = true;
            this.#barrier.resolve(id);
          }
          return true;
        }
        return false;
      }
      default:
        return false;
    }
  }

  /** Wait for the next server delivery whose subject contains `subject`. */
  waitForDelivery(
    subject: string,
    connectionId?: number,
  ): Promise<{ connectionId: number; sid: string }> {
    return new Promise((resolve) => {
      this.#deliveryWaiters.push({ connectionId, subject, resolve });
    });
  }

  /** Wait until a proxied connection is observed closed. */
  waitForClose(connectionId: number): Promise<void> {
    const connection = this.#connections.get(connectionId);
    if (!connection || connection.closed) return Promise.resolve();
    return new Promise((resolve) => {
      this.#closedWaiters.push({ connectionId, resolve });
    });
  }

  /** Whether a proxied connection has been observed closed. */
  isClosed(connectionId: number): boolean {
    return this.#connections.get(connectionId)?.closed ?? false;
  }

  /** Snapshot of observed connection state, newest last. */
  connections(): ConnectionObservation[] {
    return [...this.#connections.values()].map((connection) => ({
      id: connection.id,
      closed: connection.closed,
      subs: [...connection.subs],
      outboundContexts: [...connection.outboundContexts],
      deliveries: [...connection.deliveries],
    }));
  }

  /** Snapshot of one connection, or undefined when it never opened. */
  connection(id: number): ConnectionObservation | undefined {
    const connection = this.#connections.get(id);
    if (!connection) return undefined;
    return {
      id: connection.id,
      closed: connection.closed,
      subs: [...connection.subs],
      outboundContexts: [...connection.outboundContexts],
      deliveries: [...connection.deliveries],
    };
  }
}
