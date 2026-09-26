import {
  headers as natsHeaders,
  type Msg,
  type NatsConnection,
  type Subscription,
} from "@nats-io/nats-core";
import { encodeEventSubjectParameterToken } from "../helpers.ts";
import {
  liveConstants,
  type LiveControlResponse,
  liveDataSubject,
  liveGenerateNonce,
  liveNegotiateMaxDataBodyBytes,
  liveObserveSubject,
  type LiveOfferWire,
  liveParseControlResponse,
  liveParseFrame,
  liveParseOffer,
  liveVerifyServerProof,
} from "./protocol.ts";
import type { PermissionAtom } from "../auth/protocol_wasm.ts";
import type { AuthorizationProviderCache } from "../auth/authorization/provider_cache.ts";
import type { LiveTelemetryOwner } from "./telemetry.ts";
import {
  authorityLostEnd,
  authorityLostFrom,
  LiveAuthorityGuard,
  type LiveAuthorityLost,
} from "./authority.ts";
import {
  type LiveClock,
  LiveDeadlines,
  productionLiveClock,
} from "./deadlines.ts";
import { ConsumerCore, LiveSubscription } from "./subscription.ts";
import {
  LiveCancellation,
  type LiveCloseReceipt,
  LiveEnd,
  LiveStreamError,
} from "./types.ts";
import type { LiveSessionManager } from "./manager.ts";

export const LIVE_VERSION = "trellis.live.v1";

const C = liveConstants();

/** Live session kind advertised on a signed offer. */
export type LiveSessionKind = "standalone" | "operation";

/** Fresh request proof material for one live control or open. */
export type LiveProof = {
  proof: string;
  iat: number;
  requestId: string;
  contextDigest: string;
};

/** Independently resolved binding and identity for one live open. */
export type LiveOpenAuthority = {
  cache: AuthorizationProviderCache;
  /** Selected provider deployment from the current installed binding. */
  selectedProviderDeploymentId: string;
  /** Exact descriptor-derived Subscribe or Observe permission atom. */
  permission: PermissionAtom;
  /** Opener's actual current installed context digest. */
  localContextDigest: string;
};

/** Host dependencies supplied by the owning connection. */
export type LiveOpenHost<T> = {
  nats: NatsConnection;
  inboxPrefix: string;
  timeoutMs: number;
  sessionKey: string;
  live: LiveSessionManager;
  createRequestProof: (
    subject: string,
    payload: string,
    reply: string,
  ) => Promise<LiveProof>;
  /** Mandatory selected binding and retained-authority inputs. */
  authority: LiveOpenAuthority;
  decodeEvent: (value: unknown) => T | undefined;
  /** Internal monotonic clock; production connections omit it. */
  clock?: LiveClock;
};

/** Extract exactly one non-empty security header; duplicates are rejected. */
function singletonHeader(
  headers: Msg["headers"],
  name: string,
): string | undefined {
  const values = headers?.values(name) ?? [];
  return values.length === 1 && values[0].length > 0 ? values[0] : undefined;
}

function inbox(prefix: string): string {
  return `${prefix}.${liveGenerateNonce()}`;
}

function proofHeaders(proof: LiveProof, sessionKey: string) {
  const headers = natsHeaders();
  headers.set("proof", proof.proof);
  headers.set("iat", String(proof.iat));
  headers.set("request-id", proof.requestId);
  headers.set("authorization-context", proof.contextDigest);
  headers.set("session-key", sessionKey);
  return headers;
}

function sleepUntil(clock: LiveClock, deadlineMs: number): Promise<void> {
  return new Promise((resolve) => {
    const clear = clock.scheduleAt(deadlineMs, () => {
      clear();
      resolve();
    });
  });
}

async function withTimeout<U>(
  clock: LiveClock,
  timeoutMs: number,
  work: Promise<U>,
): Promise<U | undefined> {
  let clear: (() => void) | undefined;
  const timeout = new Promise<undefined>((resolve) => {
    clear = clock.scheduleAt(
      clock.nowMs() + timeoutMs,
      () => resolve(undefined),
    );
  });
  try {
    return await Promise.race([work, timeout]);
  } finally {
    clear?.();
  }
}

async function request(
  host: LiveOpenHost<unknown>,
  subject: string,
  payload: string,
  deadlineMs: number,
): Promise<{ msg: Msg; requestId: string }> {
  const reply = inbox(host.inboxPrefix);
  const proof = await host.createRequestProof(subject, payload, reply);
  const clock = host.clock ?? productionLiveClock;
  const remaining = deadlineMs - clock.nowMs();
  if (remaining <= 0) {
    throw new LiveStreamError(
      "setup_timeout",
      "live opening reservation elapsed",
    );
  }
  const sub = host.nats.subscribe(reply);
  try {
    host.nats.publish(subject, payload, {
      headers: proofHeaders(proof, host.sessionKey),
      reply,
    });
    const received = await withTimeout(
      clock,
      Math.min(host.timeoutMs, remaining),
      sub[Symbol.asyncIterator]().next().then((next) =>
        next.done ? undefined : next.value
      ),
    );
    if (!received) {
      throw new LiveStreamError("setup_timeout", "live request timed out");
    }
    return { msg: received, requestId: proof.requestId };
  } finally {
    sub.unsubscribe();
  }
}

/** Open one live Live session and return a prepared handle. */
export async function openLive<T>(
  host: LiveOpenHost<T>,
  subject: string,
  inputJson: string,
): Promise<LiveSubscription<T>> {
  const openId = liveGenerateNonce();
  const maxPayload = Number(host.nats.info?.max_payload ?? C.windowBytes);
  const body = JSON.stringify({
    format: LIVE_VERSION,
    type: "open",
    openId,
    receiveMaxPayloadBytes: maxPayload,
    input: JSON.parse(inputJson),
  });
  return await openLiveSession(
    host,
    subject,
    subject,
    "standalone",
    body,
    openId,
  );
}

/** Open one Operation-watch live session on the existing control route. */
export async function openLiveOperationWatch<T>(
  host: LiveOpenHost<T>,
  operationSubject: string,
  controlSubject: string,
  args: { operationId: string; includeUpdates?: boolean },
): Promise<LiveSubscription<T>> {
  const openId = liveGenerateNonce();
  const maxPayload = Number(host.nats.info?.max_payload ?? C.windowBytes);
  const body = JSON.stringify({
    action: "watch",
    operationId: args.operationId,
    ...(args.includeUpdates ? { includeUpdates: true } : {}),
    observation: {
      format: LIVE_VERSION,
      type: "open",
      openId,
      receiveMaxPayloadBytes: maxPayload,
    },
  });
  return await openLiveSession(
    host,
    controlSubject,
    operationSubject,
    "operation",
    body,
    openId,
  );
}

async function openLiveSession<T>(
  host: LiveOpenHost<T>,
  requestSubject: string,
  expectedBaseSubject: string,
  expectedKind: LiveSessionKind,
  body: string,
  openId: string,
): Promise<LiveSubscription<T>> {
  if (!host.live.isAvailable()) {
    throw new LiveStreamError("disconnected", "live manager is unavailable");
  }
  const clock = host.clock ?? productionLiveClock;
  // The local opening/reservation budget starts before the opening exchange
  // and does not restart when the first next() runs.
  const deadline = clock.nowMs() + C.openReservationMs;
  const permit = host.live.admitConsumer();
  let localGuard: LiveAuthorityGuard | undefined;
  let providerGuard: LiveAuthorityGuard | undefined;
  try {
    const retainedLocal = await LiveAuthorityGuard.retain(
      host.authority.cache,
      host.authority.localContextDigest,
      { kind: "observer", permission: host.authority.permission },
    );
    localGuard = retainedLocal;
    const response = await request(host, requestSubject, body, deadline);
    const verified = await verifyOffer(
      host,
      expectedBaseSubject,
      openId,
      expectedKind,
      response.msg,
      response.requestId,
      deadline,
      retainedLocal,
    );
    const retainedProvider = verified.providerGuard;
    providerGuard = retainedProvider;
    const offer = verified.offer;
    const core = new ConsumerCore<T>(offer.sessionId, expectedKind);
    const cancellation = new LiveCancellation();
    const session = { seq: 0n };
    const closeFn = (): Promise<LiveCloseReceipt> =>
      closeExchange(host, offer, core, session, clock, retainedProvider);
    const fence = async (): Promise<LiveEnd | undefined> => {
      const localLost = await retainedLocal.reconcile();
      if (localLost) return authorityLostEnd(localLost);
      const peerLost = await retainedProvider.reconcile();
      if (peerLost) return authorityLostEnd(peerLost);
      return undefined;
    };
    const subscription = new LiveSubscription(
      core,
      cancellation,
      closeFn,
      permit,
      fence,
    );
    void runPump(
      host,
      core,
      cancellation,
      offer,
      session,
      clock,
      deadline,
      retainedLocal,
      retainedProvider,
    ).catch((error) => {
      // Defensive: a pump rejection outside its own terminal paths must still
      // commit one bounded end rather than surfacing as an unhandled promise.
      core.commitEnd(
        new LiveEnd(
          "disconnected",
          new LiveStreamError("disconnected", String(error)),
        ),
      );
    }).finally(() => {
      core.cleanupFinished();
    });
    return subscription;
  } catch (cause) {
    localGuard?.release();
    providerGuard?.release();
    permit[Symbol.dispose]();
    throw cause;
  }
}

/** Map one guard-retention failure into a bounded setup error. */
function guardSetupError(lost: LiveAuthorityLost): LiveStreamError {
  const end = authorityLostEnd(lost);
  return end.error ??
    new LiveStreamError(
      lost === "expired"
        ? "authorization_expired"
        : "authorization_unavailable",
      `live authority is not usable: ${lost}`,
    );
}

/** The opener's pinned local identity used to bind an offer's consumer tuple. */
type OfferClaimLocal = {
  connectionId: string;
  sessionKey: string;
  principalId: string;
  participantId: string;
};

/**
 * Verify the deployment and consumer claims of a parsed, proof-verified live
 * offer against the independently selected provider binding and the opener's
 * pinned local identity. This is the production decision; its tests call the
 * same function.
 */
export function verifyOfferClaims(args: {
  offer: LiveOfferWire;
  selectedProviderDeploymentId: string | undefined;
  localContextDigest: string | undefined;
  local: OfferClaimLocal;
}): void {
  const { offer, local } = args;
  // The independently selected binding is the evidence that this deployment's
  // participant implements the API; the offered provider deployment must be
  // exactly that binding, never the offer's own claim.
  if (
    !args.selectedProviderDeploymentId ||
    offer.provider.deploymentId !== args.selectedProviderDeploymentId
  ) {
    throw new LiveStreamError(
      "protocol_error",
      "offer provider is not the selected deployment for this API",
    );
  }
  // The offered consumer tuple must be the opener's actual current local
  // identity, resolved before the offer was accepted.
  if (
    !args.localContextDigest ||
    offer.consumer.connectionId !== local.connectionId ||
    offer.consumer.sessionKey !== encodeEventSubjectParameterToken(
        local.sessionKey,
      ) ||
    offer.consumer.principalId !== local.principalId ||
    offer.consumer.participantId !== local.participantId
  ) {
    throw new LiveStreamError(
      "protocol_error",
      "offer consumer does not match the opening caller",
    );
  }
}

async function verifyOffer(
  host: LiveOpenHost<unknown>,
  baseSubject: string,
  openId: string,
  expectedKind: LiveSessionKind,
  response: Msg,
  requestId: string,
  deadlineMs: number,
  localGuard: LiveAuthorityGuard,
): Promise<{ offer: LiveOfferWire; providerGuard: LiveAuthorityGuard }> {
  let offer: LiveOfferWire;
  try {
    offer = liveParseOffer(response.data);
  } catch (cause) {
    // A signed open-error or any non-offer body is a setup failure.
    const decoded = safeCode(response.data);
    throw new LiveStreamError(
      decoded ?? "invalid_request",
      `live open rejected with '${decoded ?? "invalid_request"}'`,
    );
  }
  if (offer.kind !== expectedKind) {
    throw new LiveStreamError("protocol_error", "offer session kind mismatch");
  }
  if (offer.openId !== openId) {
    throw new LiveStreamError(
      "protocol_error",
      "offer answers a different logical open",
    );
  }
  if (offer.requestId !== requestId) {
    throw new LiveStreamError(
      "protocol_error",
      "offer answers a different opening request",
    );
  }
  const contextDigest = singletonHeader(
    response.headers,
    "authorization-context",
  );
  const sessionKey = singletonHeader(response.headers, "session-key");
  const proof = singletonHeader(response.headers, "trellis-live-proof");
  if (!contextDigest || !sessionKey || !proof) {
    throw new LiveStreamError(
      "protocol_error",
      "offer omitted or duplicated proof headers",
    );
  }
  liveVerifyServerProof(
    proof,
    contextDigest,
    response.subject,
    response.data,
    sessionKey,
  );
  if (
    encodeEventSubjectParameterToken(sessionKey) !== offer.provider.sessionKey
  ) {
    throw new LiveStreamError(
      "protocol_error",
      "offer signer does not match its advertised identity",
    );
  }
  const authority = host.authority;
  // Bind every advertised provider field to the verified signed context and
  // retain that evidence for the whole session.
  let providerGuard: LiveAuthorityGuard;
  try {
    providerGuard = await LiveAuthorityGuard.retain(
      authority.cache,
      contextDigest,
      {
        kind: "peer-provider",
        expected: {
          connectionId: offer.provider.connectionId,
          sessionKey,
          principalId: offer.provider.principalId,
          participantId: offer.provider.participantId,
          ...(offer.provider.deploymentId
            ? { deploymentId: offer.provider.deploymentId }
            : {}),
          ...(offer.provider.instanceId
            ? { instanceId: offer.provider.instanceId }
            : {}),
        },
      },
    );
  } catch (error) {
    throw guardSetupError(authorityLostFrom(error) ?? "coverage_lost");
  }
  try {
    const clock = host.clock ?? productionLiveClock;
    if (clock.nowMs() >= deadlineMs) {
      throw new LiveStreamError(
        "setup_timeout",
        "live opening reservation elapsed",
      );
    }
    // The provider deployment is the independently selected binding, never the
    // offer's own claim. The offered consumer tuple must be the opener's actual
    // local identity, resolved from the retained local authority.
    verifyOfferClaims({
      offer,
      selectedProviderDeploymentId: authority.selectedProviderDeploymentId,
      localContextDigest: authority.localContextDigest,
      local: localGuard.identity,
    });
    if (
      liveDataSubject(
        offer.provider.connectionId,
        offer.consumer.connectionId,
        offer.sessionId,
      ) !== offer.dataSubject
    ) {
      throw new LiveStreamError(
        "protocol_error",
        "offer data subject is not canonical",
      );
    }
    if (
      liveObserveSubject(
        offer.baseSubject,
        offer.provider.connectionId,
        offer.sessionId,
      ) !== offer.controlSubject
    ) {
      throw new LiveStreamError(
        "protocol_error",
        "offer control subject is not canonical",
      );
    }
    if (offer.baseSubject !== baseSubject) {
      throw new LiveStreamError(
        "protocol_error",
        "offer base subject does not match",
      );
    }
    if (
      offer.limits.windowFrames !== C.windowFrames ||
      offer.limits.windowBytes !== C.windowBytes
    ) {
      throw new LiveStreamError(
        "protocol_error",
        "offer limits do not match the shared protocol",
      );
    }
    // Validate the advertised DATA body bound against what the consumer can
    // establish locally; the provider's broker limit is never inferred from
    // the consumer's own connection.
    const consumerMaxPayload = Number(host.nats.info?.max_payload ?? 0);
    if (consumerMaxPayload <= 0) {
      throw new LiveStreamError(
        "protocol_error",
        "consumer NATS payload limit is unavailable",
      );
    }
    const advertised = offer.limits.maxDataBodyBytes;
    let localCeiling: number;
    try {
      localCeiling = liveNegotiateMaxDataBodyBytes(
        consumerMaxPayload,
        consumerMaxPayload,
      );
    } catch {
      throw new LiveStreamError(
        "protocol_error",
        "consumer NATS payload limit is too small for live data",
      );
    }
    if (
      !Number.isSafeInteger(advertised) || advertised <= 0 ||
      advertised > localCeiling
    ) {
      throw new LiveStreamError(
        "protocol_error",
        "offer DATA body limit is not independently acceptable",
      );
    }
    return { offer, providerGuard };
  } catch (error) {
    providerGuard.release();
    throw error;
  }
}

function safeCode(raw: Uint8Array): string | undefined {
  try {
    const value = JSON.parse(new TextDecoder().decode(raw)) as {
      code?: string;
    };
    return typeof value.code === "string" ? value.code : undefined;
  } catch {
    return undefined;
  }
}

/** Bounded callback ingress preserving delivery order. */
class MsgIngress {
  #queue: Msg[] = [];
  #bytes = 0;
  #messageWaiter: (() => void) | undefined;
  #closed = false;

  push(msg: Msg): boolean {
    if (this.#queue.length + 1 > C.windowFrames + 8) return false;
    if (
      this.#bytes + msg.data.length > C.windowBytes + 8 * C.maxControlBodyBytes
    ) {
      return false;
    }
    this.#queue.push(msg);
    this.#bytes += msg.data.length;
    const waiter = this.#messageWaiter;
    this.#messageWaiter = undefined;
    waiter?.();
    return true;
  }

  /** Shift the oldest retained message; never waits and never discards. */
  tryNext(): Msg | undefined {
    const msg = this.#queue.shift();
    if (msg) this.#bytes -= msg.data.length;
    return msg;
  }

  /** Resolve once a new message is retained (or the ingress is closed). */
  waitMessage(): Promise<void> {
    if (this.#closed || this.#queue.length > 0) return Promise.resolve();
    return new Promise<void>((resolve) => {
      this.#messageWaiter = resolve;
    });
  }

  close(): void {
    this.#closed = true;
    const waiter = this.#messageWaiter;
    this.#messageWaiter = undefined;
    waiter?.();
  }
}

async function runPump<T>(
  host: LiveOpenHost<T>,
  core: ConsumerCore<T>,
  cancellation: LiveCancellation,
  offer: LiveOfferWire,
  session: { seq: bigint },
  clock: LiveClock,
  deadlineMs: number,
  localGuard: LiveAuthorityGuard,
  providerGuard: LiveAuthorityGuard,
): Promise<void> {
  await Promise.race([core.waitStart(), cancellation.cancelled()]);
  if (cancellation.aborted) return;
  let changeResolvers = Promise.withResolvers<void>();
  const unsubscribeLocal = localGuard.subscribeChanges(() =>
    changeResolvers.resolve()
  );
  const unsubscribePeer = providerGuard.subscribeChanges(() =>
    changeResolvers.resolve()
  );
  const ingress = new MsgIngress();
  let subscription: Subscription | undefined;
  const localFence = async (): Promise<boolean> => {
    const localLost = await localGuard.reconcile();
    if (localLost) {
      core.discardQueue();
      core.commitEnd(authorityLostEnd(localLost));
      return true;
    }
    return false;
  };
  const authorityFence = async (): Promise<boolean> => {
    if (await localFence()) return true;
    const peerLost = await providerGuard.reconcile();
    if (peerLost) {
      core.discardQueue();
      core.commitEnd(authorityLostEnd(peerLost));
      return true;
    }
    return false;
  };
  try {
    subscription = host.nats.subscribe(offer.dataSubject, {
      callback: (error, msg) => {
        if (error) {
          ingress.close();
          return;
        }
        if (!ingress.push(msg)) {
          core.commitEnd(
            new LiveEnd(
              "consumer_slow",
              new LiveStreamError(
                "consumer_slow",
                "bounded live ingress exceeded",
              ),
            ),
          );
        }
      },
    });
    if (!(await flush(host.nats))) {
      core.commitEnd(
        new LiveEnd(
          "disconnected",
          new LiveStreamError(
            "disconnected",
            "live data subscription could not be flushed",
          ),
        ),
      );
      return;
    }
    if (await authorityFence()) return;
    core.setPhase("activating");
    const deadlines = LiveDeadlines.preparedUntil(deadlineMs);

    // Activation: bounded fresh-proof retries within the absolute reservation.
    const activateSeq = nextSeq(session).toString();
    let activated = false;
    while (!activated) {
      const response = await controlAttempt(
        host,
        core,
        offer,
        {
          action: "activate",
          controlSeq: activateSeq,
          receivedSeq: "0",
          consumedSeq: "0",
        },
        deadlineMs,
        providerGuard,
      );
      if (
        response && response.kind === "ack" &&
        response.body.state !== "closed"
      ) {
        activated = true;
        break;
      }
      if (response && response.kind === "error") {
        core.commitEnd(
          new LiveEnd(
            "setup_timeout",
            new LiveStreamError(response.body.code, "live activation rejected"),
          ),
        );
        return;
      }
      if (clock.nowMs() >= deadlineMs) {
        core.commitEnd(
          new LiveEnd(
            "setup_timeout",
            new LiveStreamError(
              "setup_timeout",
              "live activation did not complete within the reservation",
            ),
          ),
        );
        return;
      }
    }

    let lastCreditSent = 0n;
    let nextExpected = 1n;
    // One logical credit control outstanding at a time: retries reuse its
    // identical body and sequence, and newer consumption is coalesced.
    let pendingCredit:
      | { seq: string; received: string; consumed: string }
      | undefined;
    while (true) {
      core.commitDrainIfComplete();
      if (core.committedEnd()) return;
      const draining = core.phase === "draining";
      if (await (draining ? localFence() : authorityFence())) return;
      if (draining) {
        // Data ingress and peer pulse work are stopped; only the bounded local
        // drain, its stall deadline and the local guard remain. Real handoff
        // progress re-arms the stall clock.
        if (core.drainComplete()) {
          core.commitDrainIfComplete();
          return;
        }
        const stall = createDeadlineWaiter(clock, deadlines.nextDue());
        try {
          const winner = await Promise.race([
            cancellation.cancelled().then(() => "cancel" as const),
            new Promise<"end">((resolve) => core.onEnd(() => resolve("end"))),
            stall.promise.then(() => "timer" as const),
            changeResolvers.promise.then(() => "change" as const),
          ]);
          if (winner === "cancel") {
            core.discardQueue();
            core.commitEnd(new LiveEnd("cancelled"));
            return;
          }
          if (winner === "change") {
            changeResolvers = Promise.withResolvers();
            continue;
          }
          if (winner === "end") {
            changeResolvers = Promise.withResolvers();
            continue;
          }
          if (deadlines.evaluate(clock.nowMs()) === "consumer_stalled") {
            core.discardQueue();
            core.commitEnd(
              new LiveEnd(
                "consumer_slow",
                new LiveStreamError(
                  "consumer_slow",
                  "live draining queue was not consumed",
                ),
              ),
            );
            return;
          }
        } finally {
          stall.dispose();
        }
        continue;
      }

      // Recover any consumption that advanced while the pump was busy.
      const consumedNow = core.consumedSeq();
      if (consumedNow > lastCreditSent) {
        deadlines.noteConsumption(
          clock.nowMs(),
          Number(consumedNow - lastCreditSent),
        );
      }

      let winner: "cancel" | "credit" | "timer" | "msg" | "change";
      let message = ingress.tryNext();
      if (message) {
        winner = "msg";
      } else {
        const waiter = createDeadlineWaiter(clock, deadlines.nextDue());
        try {
          winner = await Promise.race([
            cancellation.cancelled().then(() => "cancel" as const),
            core.waitCredit().then(() => "credit" as const),
            waiter.promise.then(() => "timer" as const),
            ingress.waitMessage().then(() => "msg" as const),
            changeResolvers.promise.then(() => "change" as const),
          ]);
        } finally {
          waiter.dispose();
        }
        if (winner === "msg") {
          message = ingress.tryNext();
          if (!message) continue;
        }
      }
      if (winner === "cancel") {
        core.discardQueue();
        core.commitEnd(new LiveEnd("cancelled"));
        return;
      }
      if (winner === "credit") continue;
      if (winner === "change") {
        changeResolvers = Promise.withResolvers();
        if (await authorityFence()) return;
        continue;
      }
      if (winner === "timer") {
        const action = deadlines.evaluate(clock.nowMs());
        switch (action) {
          case "reservation_expired":
            core.commitEnd(
              new LiveEnd(
                "setup_timeout",
                new LiveStreamError(
                  "setup_timeout",
                  "live activation did not complete within the reservation",
                ),
              ),
            );
            return;
          case "peer_inactive":
            core.commitEnd(
              new LiveEnd(
                "peer_lost",
                new LiveStreamError(
                  "peer_lost",
                  "live provider silent past the inactivity bound",
                ),
              ),
            );
            return;
          case "credit_due": {
            deadlines.creditSent();
            if (!pendingCredit) {
              const consumed = core.consumedSeq();
              if (consumed > lastCreditSent) {
                pendingCredit = {
                  seq: nextSeq(session).toString(),
                  received: core.receivedSeq().toString(),
                  consumed: consumed.toString(),
                };
              }
            }
            if (pendingCredit) {
              const response = await controlAttempt(
                host,
                core,
                offer,
                {
                  action: "ack",
                  controlSeq: pendingCredit.seq,
                  receivedSeq: pendingCredit.received,
                  consumedSeq: pendingCredit.consumed,
                },
                undefined,
                providerGuard,
              );
              if (response && response.kind === "ack") {
                lastCreditSent = BigInt(pendingCredit.consumed);
                pendingCredit = undefined;
              } else {
                // Retain the logical control and re-arm one bounded retry.
                deadlines.noteConsumption(clock.nowMs(), 0);
              }
            }
            break;
          }
          default:
            break;
        }
        continue;
      }
      const msg = message;
      if (!msg) continue;
      if (await authorityFence()) return;
      let frame;
      try {
        frame = liveParseFrame(msg.data, offer.limits.maxDataBodyBytes);
      } catch {
        core.telemetry.rejection("invalid_protocol");
        continue;
      }
      if (
        !(await verifyProviderFrame(offer, msg, frame.sessionId, providerGuard))
      ) {
        core.telemetry.rejection("invalid_signature");
        continue;
      }
      core.telemetry.frame("control", "receive");
      if (frame.type === "data") {
        core.telemetry.frame("data", "receive");

        const seq = BigInt(frame.seq);
        if (seq > nextExpected) {
          core.commitEnd(
            new LiveEnd(
              "delivery_gap",
              new LiveStreamError("delivery_gap", "live delivery sequence gap"),
            ),
          );
          return;
        }
        if (seq < nextExpected) continue;
        nextExpected = seq + 1n;
        try {
          const value = host.decodeEvent(frame.value);
          if (value === undefined) {
            if (!core.releaseFiltered()) {
              core.commitEnd(
                new LiveEnd(
                  "consumer_slow",
                  new LiveStreamError(
                    "consumer_slow",
                    "bounded live ingress exceeded",
                  ),
                ),
              );
              return;
            }
            continue;
          }
          if (!core.admit({ value, encodedLen: msg.data.length })) {
            core.commitEnd(
              new LiveEnd(
                "consumer_slow",
                new LiveStreamError(
                  "consumer_slow",
                  "bounded live ingress exceeded",
                ),
              ),
            );
            return;
          }
        } catch (error) {
          core.commitEnd(
            new LiveEnd(
              "protocol_error",
              error instanceof LiveStreamError
                ? error
                : new LiveStreamError("protocol_error", String(error)),
            ),
          );
          return;
        }
      } else if (frame.type === "challenge") {
        if (BigInt(frame.lastSentSeq) > core.receivedSeq()) {
          core.commitEnd(
            new LiveEnd(
              "delivery_gap",
              new LiveStreamError(
                "delivery_gap",
                "live challenge referred to unreceived data",
              ),
            ),
          );
          return;
        }
        const response = await controlAttempt(
          host,
          core,
          offer,
          {
            action: "pulse",
            controlSeq: nextSeq(session).toString(),
            challengeId: frame.challengeId,
            receivedSeq: core.receivedSeq().toString(),
            consumedSeq: core.consumedSeq().toString(),
          },
          undefined,
          providerGuard,
        );
        if (
          response && response.kind === "ack" &&
          response.body.state === "active"
        ) {
          if (core.phase === "activating") {
            deadlines.commitActive(clock.nowMs(), false);
            core.setPhase("active");
          } else {
            deadlines.freshRoundTrip(clock.nowMs(), false);
          }
        }
      } else if (frame.type === "end") {
        if (BigInt(frame.finalSeq) !== core.receivedSeq()) {
          core.commitEnd(
            new LiveEnd(
              "delivery_gap",
              new LiveStreamError(
                "delivery_gap",
                "live end did not follow the complete sequence",
              ),
            ),
          );
          return;
        }
        await controlAttempt(
          host,
          core,
          offer,
          {
            action: "end-ack",
            controlSeq: nextSeq(session).toString(),
            finalSeq: frame.finalSeq,
            receivedSeq: core.receivedSeq().toString(),
            consumedSeq: core.consumedSeq().toString(),
          },
          undefined,
          providerGuard,
        );
        const end = frame.terminal.error
          ? new LiveEnd(
            frame.terminal.reason,
            new LiveStreamError(
              frame.terminal.error.code,
              frame.terminal.error.message,
            ),
          )
          : new LiveEnd(frame.terminal.reason);
        if (end.isComplete()) {
          core.setPendingEnd(end);
          core.setPhase("draining");
          deadlines.beginDraining();
          deadlines.dataAdmitted(clock.nowMs());
        } else {
          core.commitEnd(end);
          return;
        }
      }
    }
  } catch (error) {
    core.commitEnd(
      new LiveEnd(
        "disconnected",
        new LiveStreamError("disconnected", String(error)),
      ),
    );
  } finally {
    unsubscribeLocal();
    unsubscribePeer();
    ingress.close();
    subscription?.unsubscribe();
  }
}

type DeadlineWaiter = { promise: Promise<void>; dispose: () => void };

function createDeadlineWaiter(
  clock: LiveClock,
  deadlineMs: number | undefined,
): DeadlineWaiter {
  if (deadlineMs === undefined) {
    return { promise: new Promise<void>(() => {}), dispose: () => {} };
  }
  let disposed = false;
  let clear: (() => void) | undefined;
  const promise = new Promise<void>((resolve) => {
    clear = clock.scheduleAt(deadlineMs, () => {
      if (!disposed) resolve();
    });
  });
  return {
    promise,
    dispose: () => {
      disposed = true;
      clear?.();
    },
  };
}

function nextSeq(session: { seq: bigint }): bigint {
  session.seq += 1n;
  return session.seq;
}

async function verifyProviderFrame(
  offer: LiveOfferWire,
  msg: Msg,
  sessionId: string,
  providerGuard: LiveAuthorityGuard,
): Promise<boolean> {
  const contextDigest = singletonHeader(msg.headers, "authorization-context");
  const sessionKey = singletonHeader(msg.headers, "session-key");
  const proof = singletonHeader(msg.headers, "trellis-live-proof");
  if (!proof || !contextDigest || !sessionKey) return false;
  if (sessionKey !== providerGuard.identity.sessionKey) return false;
  if (sessionId !== offer.sessionId) return false;
  try {
    liveVerifyServerProof(
      proof,
      contextDigest,
      msg.subject,
      msg.data,
      sessionKey,
    );
  } catch {
    return false;
  }
  // A later frame may carry an identity-preserving refreshed context; retain
  // its exact coverage before the frame is admitted.
  if (contextDigest !== providerGuard.contextDigest) {
    let candidate: LiveAuthorityGuard;
    try {
      candidate = await providerGuard.prepareReplacement(contextDigest);
    } catch {
      return false;
    }
    if (providerGuard.commitReplacement(candidate)) return false;
  }
  return (await providerGuard.reconcile()) === undefined;
}

async function controlAttempt(
  host: LiveOpenHost<unknown>,
  core: { telemetry: LiveTelemetryOwner },
  offer: LiveOfferWire,
  control: Record<string, string>,
  deadlineMs: number | undefined,
  providerGuard: LiveAuthorityGuard,
): Promise<LiveControlResponse | undefined> {
  const reply = inbox(host.inboxPrefix);
  const payload = JSON.stringify({
    format: LIVE_VERSION,
    type: "control",
    sessionId: offer.sessionId,
    ...control,
  });
  const proof = await host.createRequestProof(
    offer.controlSubject,
    payload,
    reply,
  );
  const clock = host.clock ?? productionLiveClock;
  // An explicit deadline bounds the opening/close budget. Post-activation
  // controls (credit, pulse, end-ack) pass no absolute deadline: the opening
  // reservation must not expire a session that is already active.
  const remaining = deadlineMs === undefined
    ? Number.POSITIVE_INFINITY
    : deadlineMs - clock.nowMs();
  if (remaining <= 0) return undefined;
  const sub = host.nats.subscribe(reply);
  try {
    host.nats.publish(offer.controlSubject, payload, {
      headers: proofHeaders(proof, host.sessionKey),
      reply,
    });
    core.telemetry.frame("control", "send");
    const received = await withTimeout(
      clock,
      Math.min(host.timeoutMs, remaining),
      sub[Symbol.asyncIterator]().next().then((next) =>
        next.done ? undefined : next.value
      ),
    );
    if (!received) return undefined;
    const contextDigest = singletonHeader(
      received.headers,
      "authorization-context",
    );
    const sessionKey = singletonHeader(received.headers, "session-key");
    const proofHeader = singletonHeader(received.headers, "trellis-live-proof");
    if (!contextDigest || !sessionKey || !proofHeader) return undefined;
    if (sessionKey !== providerGuard.identity.sessionKey) return undefined;
    try {
      liveVerifyServerProof(
        proofHeader,
        contextDigest,
        received.subject,
        received.data,
        sessionKey,
      );
    } catch {
      return undefined;
    }
    // Retain an identity-preserving provider refresh before trusting it.
    if (contextDigest !== providerGuard.contextDigest) {
      let candidate: LiveAuthorityGuard;
      try {
        candidate = await providerGuard.prepareReplacement(contextDigest);
      } catch {
        return undefined;
      }
      if (providerGuard.commitReplacement(candidate)) return undefined;
    }
    if (await providerGuard.reconcile()) return undefined;
    let response: LiveControlResponse;
    try {
      response = liveParseControlResponse(received.data);
    } catch {
      return undefined;
    }
    if (response.body.sessionId !== offer.sessionId) return undefined;
    if (response.body.controlSeq !== control.controlSeq) return undefined;
    if (response.body.requestId !== proof.requestId) return undefined;
    if (response.kind === "ack" && response.body.action !== control.action) {
      return undefined;
    }
    return response;
  } catch {
    return undefined;
  } finally {
    sub.unsubscribe();
  }
}

async function closeExchange<T>(
  host: LiveOpenHost<T>,
  offer: LiveOfferWire,
  core: ConsumerCore<T>,
  session: { seq: bigint },
  clock: LiveClock,
  providerGuard: LiveAuthorityGuard,
): Promise<LiveCloseReceipt> {
  const end = core.committedEnd() ?? new LiveEnd("cancelled");
  // One absolute close budget across every attempt, retry and wait. The
  // logical close body and sequence stay identical across retries; only the
  // fresh proof, request id and reply change.
  const deadline = clock.nowMs() + C.closeExchangeMs;
  const closeSeq = nextSeq(session).toString();
  let cleanup: "complete" | "incomplete" | "unknown" = "unknown";
  let remote: "confirmed" | "unconfirmed" = "unconfirmed";
  while (clock.nowMs() < deadline) {
    const response = await controlAttempt(
      host,
      core,
      offer,
      {
        action: "close",
        reason: "cancelled",
        controlSeq: closeSeq,
        receivedSeq: core.receivedSeq().toString(),
        consumedSeq: core.consumedSeq().toString(),
      },
      deadline,
      providerGuard,
    );
    // Only a verified matching closed result establishes remote confirmation.
    if (
      response && response.kind === "ack" && response.body.state === "closed"
    ) {
      cleanup = response.body.cleanup === "complete"
        ? "complete"
        : response.body.cleanup === "incomplete"
        ? "incomplete"
        : "unknown";
      remote = "confirmed";
      break;
    }
    if (
      response && response.kind === "error" &&
      response.body.code === "session_not_found"
    ) {
      break;
    }
    const remaining = deadline - clock.nowMs();
    if (remaining <= 0) break;
    await sleepUntil(
      clock,
      clock.nowMs() + Math.min(C.closeRetryMs, remaining),
    );
  }
  return { end, remote, cleanup };
}

async function flush(nats: NatsConnection): Promise<boolean> {
  try {
    await nats.flush();
    return true;
  } catch {
    return false;
  }
}
