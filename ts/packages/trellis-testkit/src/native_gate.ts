/**
 * Observation and readiness barrier for the test transport proxy.
 *
 * The gate watches the real NATS client protocol on each proxied connection so
 * a test can (a) hold one generation's post-subscription readiness flush, (b)
 * prove which connection a real served RPC was delivered to, and (c) read the
 * exact `authorization-context` an outbound request was signed with. It never
 * inspects or records credentials: `CONNECT` payloads are ignored and only the
 * logical request header used for authorization binding is read.
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

/** Readiness gate and connection observer for one native transport proxy. */
export class NativeTransportGate {
  readonly #connections = new Map<number, ConnectionState>();
  readonly #deliveryWaiters: DeliveryWaiter[] = [];
  readonly #closedWaiters: { connectionId: number; resolve: () => void }[] = [];
  readonly #barrier = Promise.withResolvers<number>();
  #nextId = 1;
  #armed = false;
  #routeSuffix: string | undefined;
  #gatedConnectionId: number | undefined;
  #holding = false;
  #released = false;

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
   * while its routes are already broker-live.
   */
  arm(routeSuffix: string): void {
    this.#armed = true;
    this.#routeSuffix = routeSuffix;
  }

  /** Resolves with the connection id once the readiness barrier is held. */
  barrierHeld(): Promise<number> {
    return this.#barrier.promise;
  }

  /** Forward every held `PONG` and stop holding. */
  async release(): Promise<void> {
    this.#holding = false;
    this.#released = true;
    const forwards: Promise<void>[] = [];
    for (const connection of this.#connections.values()) {
      if (connection.heldPongs.length === 0) continue;
      const held = connection.heldPongs;
      connection.heldPongs = [];
      for (const bytes of held) forwards.push(connection.writer.write(bytes));
    }
    // Await the tracked writes so a failure surfaces to the caller instead of
    // becoming an unhandled rejection.
    await Promise.all(forwards);
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
          !this.#released &&
          (this.#routeSuffix === undefined ||
            op.subject.endsWith(this.#routeSuffix))
        ) {
          this.#gatedConnectionId = id;
        }
        break;
      case "pub":
        connection.outboundContexts.push({
          subject: op.subject,
          context: op.headers?.["authorization-context"],
        });
        break;
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
      case "msg":
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
        return false;
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
