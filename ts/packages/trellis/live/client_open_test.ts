/**
 * P09 component proof at the production offer-verification boundary.
 *
 * The consumer rejects offers whose consumer tuple does not match the opener's
 * pinned runtime and authenticated binding. The verifier is called directly,
 * not through a toy equality helper.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { encodeEventSubjectParameterToken } from "../helpers.ts";
import type { LiveOfferWire } from "../auth/protocol_wasm.ts";
import { verifyOfferClaims } from "./client_open.ts";

const LOCAL = {
  runtimeId: "C",
  sessionPublicKey: "consumer",
  principalId: "pru",
  binding: {
    kind: "browser" as const,
    clientId: "console",
    origin: "https://console.example",
  },
};

function offer(): LiveOfferWire {
  return {
    format: "trellis.live.v1",
    type: "offer",
    kind: "standalone",
    openId: "open",
    requestId: "req",
    sessionId: "session",
    baseSubject: "live.v1.route.Watch",
    dataSubject: "live.v1.data.A.B.C",
    controlSubject: "live.v1.route.Watch.observe.A.C",
    provider: {
      connectionId: "P",
      sessionKey: "K",
      principalId: "pr",
      participantId: "pa",
      deploymentId: "dep",
      instanceId: "inst",
    },
    consumer: {
      connectionId: "C",
      sessionKey: encodeEventSubjectParameterToken("consumer"),
      principalId: "pru",
      binding: {
        kind: "browser",
        clientId: "console",
        origin: "https://console.example",
      },
    },
    limits: {
      maxDataBodyBytes: 1024,
      windowFrames: 16,
      windowBytes: 1024,
      reservationMs: 1000,
      heartbeatIntervalMs: 1000,
      peerInactivityMs: 1000,
      consumerStallMs: 1000,
    },
  };
}

test("P09 an offer with a mutated consumer tuple is rejected", () => {
  const mutated = offer();
  mutated.consumer.binding = {
    kind: "browser",
    clientId: "console",
    origin: "https://other.example",
  };
  assert.throws(
    () =>
      verifyOfferClaims({
        offer: mutated,
        localContextDigest: "digest",
        local: LOCAL,
      }),
    (error: unknown) =>
      error instanceof Error && error.message.includes("opening caller"),
  );
});

test("P09 an offer without a pinned local context is rejected", () => {
  assert.throws(
    () =>
      verifyOfferClaims({
        offer: offer(),
        localContextDigest: undefined,
        local: LOCAL,
      }),
    (error: unknown) =>
      error instanceof Error && error.message.includes("opening caller"),
  );
});

test("P09 the opening runtime and complete browser binding are accepted", () => {
  verifyOfferClaims({
    offer: offer(),
    localContextDigest: "digest",
    local: LOCAL,
  });
});
