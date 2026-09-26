import {
  headers as natsHeaders,
  type Msg,
  type NatsConnection,
  type Subscription,
} from "@nats-io/nats-core";
import { encodeEventSubjectParameterToken } from "../helpers.ts";
import { base64urlEncode } from "../auth/utils.ts";
import type { PermissionAtom } from "../auth/protocol_wasm.ts";
import {
  liveConstants,
  type LiveControlResponse,
  type LiveControlWire,
  liveDataSubject,
  liveGenerateNonce,
  liveLogicalControlHash,
  liveNegotiateMaxDataBodyBytes,
  liveObserveSubject,
  liveObserveWildcardSubject,
  type LiveOfferWire,
  liveParseControl,
  liveServerProofDigest,
} from "./protocol.ts";
import { LIVE_VERSION, type LiveSessionKind } from "./client_open.ts";
import {
  type LiveClock,
  LiveDeadlines,
  LiveTimer,
  productionLiveClock,
} from "./deadlines.ts";
import { type LiveCloseReceipt, LiveEnd, LiveStreamError } from "./types.ts";
import {
  authorityLostEnd,
  type LiveAuthorityLost,
  type PinnedPeerIdentity,
} from "./authority.ts";
import { LiveTelemetryOwner } from "./telemetry.ts";
import type { ClosedReceipt, LiveSessionManager } from "./manager.ts";
import {
  recordCatalogCounter,
  recordCatalogUpDown,
} from "../telemetry/metrics.ts";

const C = liveConstants();

type ProviderPhase =
  | "offered"
  | "activating"
  | "active"
  | "draining"
  | "closing"
  | "closed";

/** Provider host identity used to sign and address live messages. */
export type LiveProviderIdentity = {
  connectionId: string;
  sessionKey: string;
  principalId: string;
  participantId: string;
  deploymentId: string;
  instanceId: string;
};

/** One authenticated caller projection retained for a provider session. */
export type LiveProviderCaller = {
  connectionId: string;
  sessionKey: string;
  principalId: string;
  participantId: string;
  deploymentId?: string;
  instanceId?: string;
  contextDigest: string;
};

/**
 * Narrow retained-authority port used by the provider engine.
 *
 * {@link LiveAuthorityGuard} satisfies it in production; deterministic
 * component tests may supply an equivalent fence.
 */
export type ProviderAuthorityPort = {
  readonly contextDigest: string;
  readonly identity: PinnedPeerIdentity;
  checkNow(): LiveAuthorityLost | undefined;
  /** True while a planned credential rotation is awaiting rebind. */
  maintenance(): boolean;
  /** Reconcile across a planned rotation; returns the terminal loss, if any. */
  reconcile(): Promise<LiveAuthorityLost | undefined>;
  allows(permission: PermissionAtom): boolean;
  subscribeChanges(callback: () => void): () => void;
  release(): void;
  prepareReplacement(digest: string): Promise<ProviderAuthorityPort>;
  commitReplacement(
    candidate: ProviderAuthorityPort,
  ): LiveAuthorityLost | undefined;
};

/** Provider host dependencies supplied by the owning connection. */
export type LiveProviderHost = {
  nats: NatsConnection;
  identity: LiveProviderIdentity;
  sign: (digest: Uint8Array) => Promise<Uint8Array>;
  /** Retained own-provider authority; its digest is the current context. */
  ownGuard: ProviderAuthorityPort;
  /**
   * Re-retain the connection's current own context and commit it onto
   * `ownGuard`, so an identity-preserving context replacement keeps serving
   * live sessions.
   *
   * @throws When no current context is available, or the replacement does not
   * preserve the pinned identity or the provider's required permission.
   */
  refreshOwnAuthority: () => Promise<void>;
  /** Route permission the admitted observer must hold. */
  permission: PermissionAtom;
  /** Route session kind used for admission-level telemetry. */
  kind?: "standalone" | "operation";
  /** Retain one admitted caller context as a covered fence. */
  retainCallerAuthority: (
    digest: string,
    permission: PermissionAtom,
  ) => Promise<ProviderAuthorityPort>;
  /** The connection's single live session manager. */
  manager: LiveSessionManager;
  /** Internal monotonic clock; production connections omit it. */
  clock?: LiveClock;
};

type Outstanding = { seq: bigint; bytes: number };

type Challenge = { id: string; lastSentSeq: string };

type ControlOutcome = {
  state: "activating" | "active" | "closed";
  terminal: LiveEnd | undefined;
  cleanup: "complete" | "incomplete" | null;
  challenge: Challenge | undefined;
  startSource: boolean;
  deferCleanup: boolean;
};

type CachedOutcome = {
  seq: bigint;
  hash: string;
  outcome: ControlOutcome;
};

type PendingClose = {
  reply: string;
  requestId: string;
  control: LiveControlWire;
};

class ProviderSessionRecord {
  readonly sessionId: string;
  readonly openId: string;
  readonly kind: LiveSessionKind;
  readonly baseSubject: string;
  readonly dataSubject: string;
  readonly controlSubject: string;
  consumer: LiveProviderCaller;
  readonly callerGuard: ProviderAuthorityPort;
  maxDataBodyBytes: number;
  phase: ProviderPhase = "offered";
  readonly deadlines: LiveDeadlines;
  readonly timer: LiveTimer;
  challenge: Challenge | undefined;
  readonly outstanding: Outstanding[] = [];
  outstandingBytes = 0;
  highestSent = 0n;
  highestConsumed = 0n;
  highestReceived = 0n;
  sourceStarted = false;
  endPublished = false;
  terminal: LiveEnd | undefined;
  readonly abort = new AbortController();
  emitInFlight = false;
  closed = false;
  counted = false;
  lastControl: CachedOutcome | undefined;
  pendingClose: PendingClose | undefined;
  cleanupState: "complete" | "incomplete" | "unknown" = "unknown";
  settlement: Promise<void> | undefined;
  creditWaiter: (() => void) | undefined;
  closeSettled = false;
  permit: { [Symbol.dispose](): void } | undefined;
  deregister: (() => void) | undefined;
  lane: Promise<void> = Promise.resolve();
  startSource: () => void = () => {};
  readonly telemetry: LiveTelemetryOwner;

  constructor(
    sessionId: string,
    openId: string,
    kind: LiveSessionKind,
    baseSubject: string,
    providerConnectionId: string,
    consumer: LiveProviderCaller,
    callerGuard: ProviderAuthorityPort,
    clock: LiveClock,
    onDue: () => void,
  ) {
    this.sessionId = sessionId;
    this.openId = openId;
    this.kind = kind;
    this.baseSubject = baseSubject;
    this.consumer = consumer;
    this.callerGuard = callerGuard;
    this.dataSubject = liveDataSubject(
      providerConnectionId,
      consumer.connectionId,
      sessionId,
    );
    this.controlSubject = liveObserveSubject(
      baseSubject,
      providerConnectionId,
      sessionId,
    );
    this.maxDataBodyBytes = C.windowBytes;
    this.deadlines = LiveDeadlines.reserved(clock.nowMs());
    this.timer = new LiveTimer(clock, onDue);
    this.telemetry = new LiveTelemetryOwner(kind, "provider");
    this.telemetry.prepared();
  }

  outstandingEmpty(): boolean {
    return this.outstanding.length === 0;
  }

  wakeCredit(): void {
    const waiter = this.creditWaiter;
    this.creditWaiter = undefined;
    waiter?.();
  }
}

function jsonBytes(value: unknown): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(value));
}

/** Extract exactly one non-empty security header; duplicates are rejected. */
function singletonHeader(
  headers: Msg["headers"],
  name: string,
): string | undefined {
  const values = headers?.values(name) ?? [];
  return values.length === 1 && values[0].length > 0 ? values[0] : undefined;
}

/**
 * In-process live provider engine.
 *
 * A session is reserved by an offer, activated by a delivery-path challenge
 * round trip, then streams signed DATA frames under consumer credit until the
 * source ends or a real deadline fires. Every signed handoff for one session
 * shares a single output lane; the sent watermark advances only after a
 * successful transport handoff.
 */
export class LiveProvider {
  readonly #host: LiveProviderHost;
  readonly #clock: LiveClock;
  readonly #sessions = new Map<string, ProviderSessionRecord>();
  readonly #routeTelemetry: LiveTelemetryOwner;
  #lane: Promise<void> = Promise.resolve();

  constructor(host: LiveProviderHost) {
    this.#host = host;
    this.#clock = host.clock ?? productionLiveClock;
    this.#routeTelemetry = new LiveTelemetryOwner(
      host.kind ?? "standalone",
      "provider",
    );
  }

  /** Run one publication under this provider's single ordering lane. */
  #withLane<U>(work: () => Promise<U>): Promise<U> {
    const run = this.#lane.then(work, work);
    this.#lane = run.then(
      () => {},
      () => {},
    );
    return run;
  }

  /**
   * Run one session-scoped publication under that session's own lane.
   *
   * A blocked source or signing wait for one session must never delay another
   * session's liveness or controls. Only bounded route-level verification and
   * receipt handling share the route lane.
   */
  #withSessionLane<U>(
    record: ProviderSessionRecord,
    work: () => Promise<U>,
  ): Promise<U> {
    const run = record.lane.then(work, work);
    record.lane = run.then(
      () => {},
      () => {},
    );
    return run;
  }

  /** Return the guard's current own-context digest, never a frozen copy. */
  #ownDigest(): string {
    return this.#host.ownGuard.contextDigest;
  }

  /**
   * Refresh the retained own authority before a failed own-context check ends
   * a session.
   *
   * @returns true when the retained own authority is healthy again.
   */
  async #refreshOwnAuthority(): Promise<boolean> {
    try {
      await this.#host.refreshOwnAuthority();
    } catch {
      return false;
    }
    return this.#host.ownGuard.checkNow() === undefined;
  }

  /**
   * Report the provider's own authority loss after attempting maintenance or a
   * same-generation refresh, so a replaced context never drops a live session.
   */
  async #ownAuthorityLost(): Promise<LiveAuthorityLost | undefined> {
    const guard = this.#host.ownGuard;
    if (guard.maintenance()) return await guard.reconcile();
    const lost = guard.checkNow();
    if (!lost) return undefined;
    if (await this.#refreshOwnAuthority()) return undefined;
    return guard.checkNow() ?? lost;
  }

  wildcardSubject(baseSubject: string): string {
    return liveObserveWildcardSubject(
      baseSubject,
      this.#host.identity.connectionId,
    );
  }

  /** Install the owner-control subscription for one exact route. */
  subscribeControl(baseSubject: string): Subscription {
    return this.#host.nats.subscribe(this.wildcardSubject(baseSubject));
  }

  /** Reserve one session, publish its signed offer and start its timer. */
  async offer(
    msg: Msg,
    baseSubject: string,
    open: { openId: string; receiveMaxPayloadBytes: number },
    caller: LiveProviderCaller,
    startSource: (session: {
      emit: (value: unknown) => Promise<void>;
      signal: AbortSignal;
    }) => Promise<void>,
    kind: LiveSessionKind = "standalone",
  ): Promise<void> {
    // An opening without a reply destination cannot hand off an offer.
    if (!msg.reply) return;
    // Validate the shared opening bound before any resource is allocated.
    const maxDataBodyBytes = this.#negotiate(open.receiveMaxPayloadBytes);
    const sessionId = liveGenerateNonce();
    const callerGuard = await this.#host.retainCallerAuthority(
      caller.contextDigest,
      this.#host.permission,
    );
    let permit: { [Symbol.dispose](): void };
    try {
      permit = this.#host.manager.admitProvider(
        caller.connectionId,
        caller.sessionKey,
      );
    } catch (cause) {
      this.#routeTelemetry.rejection("over_capacity");
      callerGuard.release();
      throw cause;
    }
    let record: ProviderSessionRecord;
    try {
      record = new ProviderSessionRecord(
        sessionId,
        open.openId,
        kind,
        baseSubject,
        this.#host.identity.connectionId,
        caller,
        callerGuard,
        this.#clock,
        () => void this.#onDue(record),
      );
    } catch (cause) {
      permit[Symbol.dispose]();
      callerGuard.release();
      throw cause;
    }
    record.permit = permit;
    record.maxDataBodyBytes = maxDataBodyBytes;
    record.startSource = () => {
      if (record.sourceStarted || record.phase === "closed") return;
      record.sourceStarted = true;
      record.phase = "active";
      record.telemetry.active();
      void this.#runSource(record, startSource);
    };
    this.#sessions.set(sessionId, record);
    record.deregister = this.#host.manager.registerSession({
      fence: () => this.#fence(record),
      close: async () => {
        await this.#terminate(record, new LiveEnd("local_shutdown"));
      },
    });
    // Arm the reservation deadline before any externally interruptible handoff
    // so a stalled or failed offer still enters owned teardown.
    this.#armTimer(record);
    const offer: LiveOfferWire = {
      format: LIVE_VERSION,
      type: "offer",
      kind,
      openId: open.openId,
      requestId: singletonHeader(msg.headers, "request-id") ?? "",
      sessionId,
      baseSubject,
      dataSubject: record.dataSubject,
      controlSubject: record.controlSubject,
      provider: {
        connectionId: this.#host.identity.connectionId,
        sessionKey: encodeEventSubjectParameterToken(
          this.#host.identity.sessionKey,
        ),
        principalId: this.#host.identity.principalId,
        participantId: this.#host.identity.participantId,
        deploymentId: this.#host.identity.deploymentId,
        instanceId: this.#host.identity.instanceId,
      },
      consumer: {
        connectionId: caller.connectionId,
        sessionKey: encodeEventSubjectParameterToken(caller.sessionKey),
        principalId: caller.principalId,
        participantId: caller.participantId,
      },
      limits: {
        maxDataBodyBytes,
        windowFrames: C.windowFrames,
        windowBytes: C.windowBytes,
        reservationMs: C.openReservationMs,
        heartbeatIntervalMs: C.heartbeatIntervalMs,
        peerInactivityMs: C.peerInactivityMs,
        consumerStallMs: C.consumerStallMs,
      },
    };
    try {
      await this.#withSessionLane(
        record,
        () => this.#publishSigned(msg.reply!.toString(), jsonBytes(offer)),
      );
    } catch (cause) {
      // A failed handoff must enter owned teardown, never leave an untimed
      // record holding admission.
      await this.#terminate(record, new LiveEnd("cancelled"));
      throw cause;
    }
  }

  /**
   * Handle one control message.
   *
   * The pipeline is: bounded header admission, caller authentication (which
   * verifies the signed request binding and the exact safe reply), caller
   * identity comparison, strict protocol parse, logical control validation,
   * one allowed state transition, then one signed reply for the actual reply
   * destination. No error is reflected before the safe-reply boundary.
   */
  async handleControl(
    msg: Msg,
    authenticate: (
      msg: Msg,
    ) => Promise<LiveProviderCaller | undefined>,
  ): Promise<void> {
    if (!msg.reply || !msg.headers) return;
    const sessionKey = singletonHeader(msg.headers, "session-key");
    const contextDigest = singletonHeader(msg.headers, "authorization-context");
    if (!sessionKey || !contextDigest) {
      this.#routeTelemetry.rejection("invalid_signature");
      return;
    }
    const caller = await authenticate(msg);
    if (!caller) {
      this.#routeTelemetry.rejection("invalid_signature");
      return;
    }
    if (caller.sessionKey !== sessionKey) {
      this.#routeTelemetry.rejection("foreign_identity");
      return;
    }
    let control: LiveControlWire;
    try {
      control = liveParseControl(msg.data);
    } catch {
      this.#routeTelemetry.rejection("invalid_protocol");
      return;
    }
    const record = this.#sessions.get(control.sessionId);
    if (!record) {
      // Safely authenticated unknown session: a bounded signed error is safe
      // because authenticate already validated the exact reply destination.
      // A receipt is only disclosed to its original owner.
      const receipt = this.#host.manager.receipt(control.sessionId);
      if (
        receipt && control.action === "close" &&
        receipt.ownerConnectionId === caller.connectionId &&
        receipt.ownerSessionKey === caller.sessionKey
      ) {
        await this.#publishReceiptAck(msg, control, receipt);
        return;
      }
      await this.#publishControlError(
        msg,
        control,
        "session_not_found",
        caller,
      );
      return;
    }
    if (record.closed) {
      const receipt = this.#host.manager.receipt(control.sessionId);
      if (
        receipt && control.action === "close" &&
        receipt.ownerConnectionId === caller.connectionId &&
        receipt.ownerSessionKey === caller.sessionKey
      ) {
        await this.#publishReceiptAck(msg, control, receipt);
        return;
      }
      await this.#publishControlError(
        msg,
        control,
        "session_not_found",
        caller,
      );
      return;
    }
    if (sessionKey !== record.consumer.sessionKey) {
      this.#routeTelemetry.rejection("foreign_identity");
      return;
    }
    if (
      caller.connectionId !== record.consumer.connectionId ||
      caller.principalId !== record.consumer.principalId ||
      caller.participantId !== record.consumer.participantId
    ) {
      this.#routeTelemetry.rejection("foreign_identity");
      return;
    }
    // An identity-preserving caller refresh replaces the retained guard before
    // its control is applied.
    if (caller.contextDigest !== record.consumer.contextDigest) {
      try {
        const candidate = await record.callerGuard.prepareReplacement(
          caller.contextDigest,
        );
        if (record.callerGuard.commitReplacement(candidate)) {
          this.#routeTelemetry.rejection("invalid_signature");
          return;
        }
      } catch {
        this.#routeTelemetry.rejection("invalid_signature");
        return;
      }
      record.consumer = caller;
    }
    record.telemetry.frame("control", "receive");
    const now = this.#clock.nowMs();
    const requestId = singletonHeader(msg.headers, "request-id") ?? "";
    const result = this.#applyControl(record, control, msg.data, now);
    if (result.kind === "error") {
      const error = this.#controlErrorBody(
        record,
        control,
        requestId,
        result.code,
      );
      await this.#withSessionLane(
        record,
        () => this.#tryPublishSigned(msg.reply!.toString(), jsonBytes(error)),
      );
      return;
    }
    const outcome = result.outcome;
    if (outcome.deferCleanup) {
      record.pendingClose = { reply: msg.reply.toString(), requestId, control };
      this.#settleClose(record);
      return;
    }
    const ack = this.#ackBody(record, control, requestId, outcome);
    const published = await this.#withSessionLane(
      record,
      () => this.#tryPublishSigned(msg.reply!.toString(), jsonBytes(ack)),
    );
    if (!published) {
      await this.#terminate(
        record,
        new LiveEnd(
          "peer_lost",
          new LiveStreamError(
            "peer_lost",
            "activation acknowledgement handoff failed",
          ),
        ),
      );
      return;
    }
    if (outcome.challenge) {
      await this.#publishChallenge(record);
    }
    if (outcome.startSource) {
      if (record.phase !== "active" || record.closed) return;
      const ownLost = await this.#ownAuthorityLost();
      const callerLost = await record.callerGuard.reconcile();
      if (ownLost || callerLost) {
        await this.#terminate(
          record,
          authorityLostEnd(ownLost ?? callerLost ?? "coverage_lost"),
        );
        return;
      }
      record.startSource();
    }
  }

  #applyControl(
    record: ProviderSessionRecord,
    control: LiveControlWire,
    rawBody: Uint8Array,
    nowMs: number,
  ): { kind: "ok"; outcome: ControlOutcome } | {
    kind: "error";
    code: string;
  } {
    let hash: string;
    try {
      hash = liveLogicalControlHash(rawBody);
    } catch {
      return { kind: "error", code: "invalid_request" };
    }
    const seq = BigInt(control.controlSeq);
    const previous = record.lastControl;
    if (previous) {
      if (seq === previous.seq) {
        if (previous.hash !== hash) {
          return { kind: "error", code: "control_conflict" };
        }
        // Logical replay: cached semantic outcome, no mutation, no renewal.
        return {
          kind: "ok",
          outcome: { ...previous.outcome, startSource: false },
        };
      }
      if (seq < previous.seq) return { kind: "error", code: "stale_control" };
      if (seq > previous.seq + 1n) {
        return { kind: "error", code: "control_gap" };
      }
    } else if (seq !== 1n) {
      return { kind: "error", code: "control_gap" };
    }
    const outcome = this.#transition(record, control, nowMs);
    if ("code" in outcome) return { kind: "error", code: outcome.code };
    record.lastControl = { seq, hash, outcome };
    return { kind: "ok", outcome };
  }

  #transition(
    record: ProviderSessionRecord,
    control: LiveControlWire,
    nowMs: number,
  ): ControlOutcome | { code: string } {
    switch (control.action) {
      case "activate": {
        if (record.phase !== "offered") {
          return { code: "stale_control" };
        }
        const challenge: Challenge = {
          id: liveGenerateNonce(),
          lastSentSeq: record.highestSent.toString(),
        };
        record.phase = "activating";
        record.telemetry.activating();
        record.challenge = challenge;
        record.deadlines.beginActivating(nowMs, challenge.id);
        this.#armTimer(record);
        return {
          state: "activating",
          terminal: undefined,
          cleanup: null,
          challenge,
          startSource: false,
          deferCleanup: false,
        };
      }
      case "pulse": {
        if (record.deadlines.outstandingChallenge() !== control.challengeId) {
          return { code: "invalid_challenge" };
        }
        if (!this.#validateCredit(record, control)) {
          return { code: "invalid_cursor" };
        }
        const activating = record.phase === "activating";
        if (activating) {
          record.deadlines.commitActive(nowMs, true);
          record.phase = "active";
          record.telemetry.active();
        } else {
          record.deadlines.freshRoundTrip(nowMs, true);
        }
        record.challenge = undefined;
        this.#applyCredit(record, control);
        this.#armTimer(record);
        return {
          state: "active",
          terminal: undefined,
          cleanup: null,
          challenge: undefined,
          startSource: activating && !record.sourceStarted,
          deferCleanup: false,
        };
      }
      case "ack": {
        if (!this.#validateCredit(record, control)) {
          return { code: "invalid_cursor" };
        }
        this.#applyCredit(record, control);
        this.#armTimer(record);
        return {
          state: this.#stateFor(record),
          terminal: undefined,
          cleanup: null,
          challenge: undefined,
          startSource: false,
          deferCleanup: false,
        };
      }
      case "close":
      case "end-ack": {
        if (!this.#validateCredit(record, control)) {
          return { code: "invalid_cursor" };
        }
        if (control.action === "end-ack") {
          // Only an acknowledgement of the END already published is valid; it
          // must not invent or overwrite the committed terminal reason.
          if (!record.endPublished) {
            return { code: "invalid_request" };
          }
          const acked = control.receivedSeq ? BigInt(control.receivedSeq) : 0n;
          if (acked !== record.highestSent) {
            return { code: "invalid_cursor" };
          }
        }
        this.#applyCredit(record, control);
        record.terminal ??= new LiveEnd("cancelled");
        record.phase = "closing";
        record.telemetry.closing();
        record.deadlines.beginClosing(nowMs);
        this.#fence(record);
        this.#armTimer(record);
        return {
          state: "closed",
          terminal: record.terminal,
          cleanup: null,
          challenge: undefined,
          startSource: false,
          deferCleanup: true,
        };
      }
      default:
        return { code: "invalid_request" };
    }
  }

  #stateFor(
    record: ProviderSessionRecord,
  ): "activating" | "active" | "closed" {
    if (record.phase === "closed" || record.phase === "closing") {
      return "closed";
    }
    if (record.phase === "active" || record.phase === "draining") {
      return "active";
    }
    return "activating";
  }

  #validateCredit(
    record: ProviderSessionRecord,
    control: LiveControlWire,
  ): boolean {
    const received = control.receivedSeq ? BigInt(control.receivedSeq) : 0n;
    const consumed = control.consumedSeq ? BigInt(control.consumedSeq) : 0n;
    if (consumed > received || received > record.highestSent) return false;
    if (consumed < record.highestConsumed) return false;
    if (received < record.highestReceived) return false;
    return true;
  }

  #applyCredit(record: ProviderSessionRecord, control: LiveControlWire): void {
    const received = control.receivedSeq ? BigInt(control.receivedSeq) : 0n;
    const consumed = control.consumedSeq ? BigInt(control.consumedSeq) : 0n;
    if (consumed > received || received > record.highestSent) return;
    if (consumed < record.highestConsumed) return;
    const advanced = consumed > record.highestConsumed;
    record.highestConsumed = consumed;
    record.highestReceived = received;
    while (
      record.outstanding.length > 0 && record.outstanding[0].seq <= consumed
    ) {
      const frame = record.outstanding.shift();
      if (frame) record.outstandingBytes -= frame.bytes;
    }
    if (advanced) record.deadlines.noteStallReset(this.#clock.nowMs());
    if (record.outstandingEmpty()) record.deadlines.outstandingCleared();
    record.wakeCredit();
  }

  #ackBody(
    record: ProviderSessionRecord,
    control: LiveControlWire,
    requestId: string,
    outcome: ControlOutcome,
  ): LiveControlResponse["body"] & { type: "control-ack" } {
    return {
      format: LIVE_VERSION,
      type: "control-ack",
      sessionId: record.sessionId,
      controlSeq: control.controlSeq,
      requestId,
      action: control.action,
      state: outcome.state,
      acceptedReceivedSeq: record.highestReceived.toString(),
      acceptedConsumedSeq: record.highestConsumed.toString(),
      terminal: outcome.terminal
        ? {
          reason: outcome.terminal.reason,
          error: outcome.terminal.error
            ? {
              code: outcome.terminal.error.code,
              message: outcome.terminal.error.message,
            }
            : null,
        }
        : null,
      cleanup: outcome.cleanup,
    };
  }

  #controlErrorBody(
    record: ProviderSessionRecord | undefined,
    control: LiveControlWire,
    requestId: string,
    code: string,
  ) {
    return {
      format: LIVE_VERSION,
      type: "control-error",
      sessionId: control.sessionId,
      controlSeq: control.controlSeq,
      requestId,
      code,
    };
  }

  async #publishControlError(
    msg: Msg,
    control: LiveControlWire,
    code: string,
    _caller: LiveProviderCaller | undefined,
  ): Promise<void> {
    const requestId = singletonHeader(msg.headers, "request-id") ?? "";
    const body = {
      format: LIVE_VERSION,
      type: "control-error",
      sessionId: control.sessionId,
      controlSeq: control.controlSeq,
      requestId,
      code,
    };
    const reply = msg.reply?.toString();
    if (!reply) return;
    await this.#withLane(() => this.#tryPublishSigned(reply, jsonBytes(body)));
  }

  async #publishReceiptAck(
    msg: Msg,
    control: LiveControlWire,
    receipt: ClosedReceipt,
  ): Promise<void> {
    const requestId = singletonHeader(msg.headers, "request-id") ?? "";
    const body = {
      format: LIVE_VERSION,
      type: "control-ack",
      sessionId: control.sessionId,
      controlSeq: control.controlSeq,
      requestId,
      action: control.action,
      state: "closed",
      acceptedReceivedSeq: receipt.receivedSeq,
      acceptedConsumedSeq: receipt.consumedSeq,
      terminal: {
        reason: receipt.reason,
        error: null,
      },
      cleanup: receipt.cleanup,
    };
    const reply = msg.reply?.toString();
    if (!reply) return;
    await this.#withLane(() => this.#tryPublishSigned(reply, jsonBytes(body)));
  }

  /** Arm the timer for the record's next deadline. */
  #armTimer(record: ProviderSessionRecord): void {
    record.timer.arm(record.deadlines.nextDue());
  }

  /** Evaluate one due deadline and perform its action. */
  async #onDue(record: ProviderSessionRecord): Promise<void> {
    if (record.closed) {
      record.timer.dispose();
      return;
    }
    const ownLost = await this.#ownAuthorityLost();
    if (ownLost) {
      await this.#terminate(record, authorityLostEnd(ownLost));
      return;
    }
    const callerLost = await record.callerGuard.reconcile();
    if (callerLost) {
      await this.#terminate(record, authorityLostEnd(callerLost));
      return;
    }
    const now = this.#clock.nowMs();
    const action = record.deadlines.evaluate(now);
    switch (action) {
      case "reservation_expired":
        await this.#terminate(
          record,
          new LiveEnd(
            "setup_timeout",
            new LiveStreamError(
              "setup_timeout",
              "live activation did not complete",
            ),
          ),
        );
        return;
      case "challenge_retry":
        await this.#publishChallenge(record);
        record.deadlines.rearmChallengeRetry(this.#clock.nowMs());
        break;
      case "challenge_due": {
        const challenge: Challenge = {
          id: liveGenerateNonce(),
          lastSentSeq: record.highestSent.toString(),
        };
        record.challenge = challenge;
        record.deadlines.beginChallenge(this.#clock.nowMs(), challenge.id);
        await this.#publishChallenge(record);
        break;
      }
      case "peer_inactive":
        await this.#terminate(
          record,
          new LiveEnd(
            "peer_lost",
            new LiveStreamError(
              "peer_lost",
              "live consumer silent past the inactivity bound",
            ),
          ),
        );
        return;
      case "consumer_stalled":
        await this.#terminate(
          record,
          new LiveEnd(
            "consumer_slow",
            new LiveStreamError(
              "consumer_slow",
              "live consumer did not consume outstanding data",
            ),
          ),
        );
        return;
      case "credit_due":
        record.deadlines.creditSent();
        break;
      case "close_exchange_elapsed":
        await this.#terminate(record, new LiveEnd("cancelled"));
        return;
      case "cleanup_grace_elapsed":
        record.deadlines.markCleanupGraceElapsed();
        break;
      default:
        break;
    }
    this.#armTimer(record);
  }

  async #publishChallenge(record: ProviderSessionRecord): Promise<void> {
    const challenge = record.challenge;
    if (!challenge || record.closed || record.phase === "closing") return;
    const body = jsonBytes({
      format: LIVE_VERSION,
      type: "challenge",
      sessionId: record.sessionId,
      challengeId: challenge.id,
      lastSentSeq: challenge.lastSentSeq,
    });
    await this.#withSessionLane(
      record,
      () => this.#tryPublishFrame(record, body, "control"),
    );
  }

  async #publishSigned(
    subject: string,
    body: Uint8Array,
  ): Promise<void> {
    const headers = await this.#liveHeaders(subject, body);
    this.#host.nats.publish(subject, body, { headers });
  }

  async #tryPublishSigned(
    subject: string,
    body: Uint8Array,
  ): Promise<boolean> {
    try {
      await this.#publishSigned(subject, body);
      this.#routeTelemetry.frame("control", "send");
      return true;
    } catch {
      return false;
    }
  }

  async #tryPublishFrame(
    record: ProviderSessionRecord,
    body: Uint8Array,
    frameClass: "data" | "control",
  ): Promise<boolean> {
    if (this.#publicationBlocked(record, frameClass)) return false;
    try {
      const headers = await this.#liveHeaders(record.dataSubject, body);
      // Final transport admission after the awaited signature: the fence,
      // phase, cancellation, or authority may have moved while signing.
      if (this.#publicationBlocked(record, frameClass)) return false;
      this.#host.nats.publish(record.dataSubject, body, { headers });
      record.telemetry.frame(frameClass, "send");
      return true;
    } catch {
      return false;
    }
  }

  /** Whether one frame may still enter transport for this session. */
  #publicationBlocked(
    record: ProviderSessionRecord,
    frameClass: "data" | "control",
  ): boolean {
    if (record.closed) return true;
    if (frameClass === "control") return false;
    if (record.phase !== "active" || record.abort.signal.aborted) return true;
    return Boolean(
      this.#host.ownGuard.checkNow() || record.callerGuard.checkNow(),
    );
  }

  async #liveHeaders(
    subject: string,
    body: Uint8Array,
  ) {
    const digest = this.#ownDigest();
    if (!digest) {
      throw new LiveStreamError(
        "authorization_unavailable",
        "provider authorization context is unavailable",
      );
    }
    const proof = base64urlEncode(
      await this.#host.sign(liveServerProofDigest(digest, subject, body)),
    );
    const headers = natsHeaders();
    headers.set("authorization-context", digest);
    headers.set("session-key", this.#host.identity.sessionKey);
    headers.set("trellis-live-proof", proof);
    return headers;
  }

  #negotiate(consumerMaxPayloadBytes: number): number {
    const providerMax = Number(
      this.#host.nats.info?.max_payload ?? 0,
    );
    if (providerMax <= 0) {
      throw new LiveStreamError(
        "protocol_error",
        "provider NATS payload limit is unavailable",
      );
    }
    return liveNegotiateMaxDataBodyBytes(
      consumerMaxPayloadBytes,
      providerMax,
    );
  }

  async #runSource(
    record: ProviderSessionRecord,
    startSource: (session: {
      emit: (value: unknown) => Promise<void>;
      signal: AbortSignal;
    }) => Promise<void>,
  ): Promise<void> {
    const running = (async () => {
      try {
        await startSource({
          signal: record.abort.signal,
          emit: async (value) => {
            await this.#emit(record, value);
          },
        });
        return new LiveEnd("complete");
      } catch (cause) {
        if (record.abort.signal.aborted || record.closed) {
          return undefined;
        }
        return new LiveEnd(
          "source_error",
          new LiveStreamError(
            "source_failed",
            cause instanceof Error ? cause.message : String(cause),
          ),
        );
      }
    })();
    record.settlement = running.then(
      () => {},
      () => {},
    );
    const end = await running;
    if (!end) return;
    await this.#terminate(record, end);
  }

  async #emit(record: ProviderSessionRecord, value: unknown): Promise<void> {
    if (
      record.closed || record.phase === "closing" || record.phase === "closed"
    ) {
      throw new LiveStreamError("closed", "live session is closed");
    }
    const ownLost = await this.#ownAuthorityLost();
    const callerLost = await record.callerGuard.reconcile();
    if (ownLost || callerLost) {
      await this.#terminate(
        record,
        authorityLostEnd(ownLost ?? callerLost ?? "coverage_lost"),
      );
      throw new LiveStreamError("closed", "live session is closed");
    }
    if (record.emitInFlight) {
      throw new LiveStreamError(
        "concurrent_emit",
        "a live emit is already in flight",
      );
    }
    const maxDataBodyBytes = record.maxDataBodyBytes;
    const seq = record.highestSent + 1n;
    const body = jsonBytes({
      format: LIVE_VERSION,
      type: "data",
      sessionId: record.sessionId,
      seq: seq.toString(),
      value,
    });
    if (body.length > maxDataBodyBytes) {
      throw new LiveStreamError(
        "payload_too_large",
        "live data frame exceeds the negotiated body limit",
      );
    }
    record.emitInFlight = true;
    record.telemetry.buffered(body.length);
    try {
      // Wait for credit through an owned notification, not a zero-delay spin.
      while (
        record.outstanding.length + 1 > C.windowFrames ||
        record.outstandingBytes + body.length > C.windowBytes
      ) {
        if (record.abort.signal.aborted || record.closed) {
          throw new LiveStreamError("cancelled", "live session was cancelled");
        }
        await new Promise<void>((resolve) => {
          record.creditWaiter = resolve;
        });
        if (record.abort.signal.aborted || record.closed) {
          throw new LiveStreamError("cancelled", "live session was cancelled");
        }
      }
      const published = await this.#withSessionLane(
        record,
        () => this.#tryPublishFrame(record, body, "data"),
      );
      if (!published) {
        throw new LiveStreamError("source_failed", "live publication failed");
      }
      // The watermark advances only after a successful handoff.
      record.highestSent = seq;
      record.outstanding.push({ seq, bytes: body.length });
      record.outstandingBytes += body.length;
      record.deadlines.dataAdmitted(this.#clock.nowMs());
      this.#armTimer(record);
    } finally {
      record.telemetry.buffered(-body.length);
      record.emitInFlight = false;
    }
  }

  async #publishEnd(
    record: ProviderSessionRecord,
    end: LiveEnd,
  ): Promise<boolean> {
    if (record.endPublished) return true;
    // The first committed terminal is authoritative; a later source or control
    // completion never overwrites it.
    const committed = record.terminal ??= end;
    const body = jsonBytes({
      format: LIVE_VERSION,
      type: "end",
      sessionId: record.sessionId,
      finalSeq: record.highestSent.toString(),
      terminal: {
        reason: committed.reason,
        error: committed.error
          ? { code: committed.error.code, message: committed.error.message }
          : null,
      },
    });
    const published = await this.#withSessionLane(
      record,
      () => this.#tryPublishFrame(record, body, "control"),
    );
    if (!published) return false;
    record.endPublished = true;
    record.phase = "closing";
    record.deadlines.beginClosing(this.#clock.nowMs());
    this.#armTimer(record);
    return true;
  }

  /** Fence one session without awaiting any cleanup. */
  #fence(record: ProviderSessionRecord): void {
    if (record.closed) return;
    if (record.phase !== "closing") record.phase = "closing";
    record.abort.abort();
    record.wakeCredit();
  }

  /** One termination entrypoint: commit, END, fence, settle, receipt. */
  async #terminate(
    record: ProviderSessionRecord,
    end: LiveEnd,
  ): Promise<LiveCloseReceipt> {
    if (record.closed) {
      return {
        end: record.endPublished ? end : new LiveEnd("cancelled"),
        remote: "confirmed",
        cleanup: record.cleanupState === "unknown"
          ? "unknown"
          : record.cleanupState,
      };
    }
    this.#fence(record);
    const committed = record.terminal ?? end;
    if (!record.endPublished && committed.reason !== "cancelled") {
      await this.#publishEnd(record, committed);
    }
    record.closed = true;
    record.phase = "closed";
    record.timer.dispose();
    record.telemetry.closing();
    record.telemetry.end(committed);
    const settled = await this.#awaitSettlement(record);
    this.#finishCleanup(record, settled);
    this.#sessions.delete(record.sessionId);
    record.deregister?.();
    record.deregister = undefined;
    this.#host.manager.insertReceipt({
      sessionId: record.sessionId,
      ownerConnectionId: record.consumer.connectionId,
      ownerSessionKey: record.consumer.sessionKey,
      baseSubject: record.baseSubject,
      reason: committed.reason,
      cleanup: record.cleanupState === "complete" ? "complete" : "incomplete",
      finalSeq: record.highestSent.toString(),
      receivedSeq: record.highestReceived.toString(),
      consumedSeq: record.highestConsumed.toString(),
    });
    record.callerGuard.release();
    return {
      end: committed,
      remote: "confirmed",
      cleanup: record.cleanupState === "unknown"
        ? "unknown"
        : record.cleanupState,
    };
  }

  /** Await owned source settlement under the shared grace. */
  async #awaitSettlement(record: ProviderSessionRecord): Promise<boolean> {
    const settlement = record.settlement;
    if (!settlement) return true;
    let settled = true;
    await new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        settled = false;
        resolve();
      }, C.cleanupGraceMs);
      void settlement.then(() => {
        clearTimeout(timer);
        resolve();
      });
    });
    return settled;
  }

  /** Record the cleanup outcome and release admission exactly once. */
  #finishCleanup(record: ProviderSessionRecord, settled: boolean): void {
    record.cleanupState = settled ? "complete" : "incomplete";
    if (record.lastControl) {
      record.lastControl.outcome.cleanup = record.cleanupState;
    }
    if (settled) {
      record.telemetry.cleanupFinished();
      record.permit?.[Symbol.dispose]();
      record.permit = undefined;
      return;
    }
    record.telemetry.cleanupExceededGrace();
    // The source is still running: retain admission until actual settlement,
    // then release exactly once. The terminal cause never changes.
    const settlement = record.settlement ?? Promise.resolve();
    void settlement.then(() => {
      record.cleanupState = "complete";
      if (record.lastControl) {
        record.lastControl.outcome.cleanup = "complete";
      }
      record.telemetry.cleanupFinished();
      record.permit?.[Symbol.dispose]();
      record.permit = undefined;
    });
  }

  /** Await cleanup, then answer any deferred close acknowledgement. */
  #settleClose(record: ProviderSessionRecord): void {
    if (record.closeSettled) return;
    record.closeSettled = true;
    void (async () => {
      record.telemetry.closing();
      record.telemetry.end(
        record.terminal ?? record.lastControl?.outcome.terminal ??
          new LiveEnd("cancelled"),
      );
      const settled = await this.#awaitSettlement(record);
      this.#finishCleanup(record, settled);
      const pending = record.pendingClose;
      if (!pending) return;
      record.pendingClose = undefined;
      const outcome: ControlOutcome = {
        state: "closed",
        terminal: record.lastControl?.outcome.terminal,
        cleanup: record.cleanupState === "unknown" ? null : record.cleanupState,
        challenge: undefined,
        startSource: false,
        deferCleanup: false,
      };
      const ack = this.#ackBody(
        record,
        pending.control,
        pending.requestId,
        outcome,
      );
      await this.#withSessionLane(
        record,
        () => this.#tryPublishSigned(pending.reply, jsonBytes(ack)),
      );
    })();
  }
}

/** Parse one bounded live Live opening request. */
export function parseLiveOpen(
  raw: Uint8Array,
):
  | { openId: string; receiveMaxPayloadBytes: number; input: unknown }
  | undefined {
  try {
    const value = JSON.parse(new TextDecoder().decode(raw)) as {
      format?: string;
      type?: string;
      openId?: string;
      receiveMaxPayloadBytes?: number;
      input?: unknown;
    };
    if (value.format !== LIVE_VERSION || value.type !== "open") return;
    if (typeof value.openId !== "string") return;
    if (typeof value.receiveMaxPayloadBytes !== "number") return;
    return {
      openId: value.openId,
      receiveMaxPayloadBytes: value.receiveMaxPayloadBytes,
      input: value.input,
    };
  } catch {
    return;
  }
}
