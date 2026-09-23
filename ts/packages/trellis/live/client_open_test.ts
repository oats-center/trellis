/**
 * P09 component proof at the production offer-verification boundary.
 *
 * The consumer rejects an offer whose provider deployment is not the
 * independently selected binding, and any offer whose consumer tuple does not
 * match the opener's pinned local identity. The verifier is called directly,
 * not through a toy equality helper.
 */

import { assertThrows } from "@std/assert";

import { encodeEventSubjectParameterToken } from "../helpers.ts";
import type { LiveOfferWire } from "../auth/protocol_wasm.ts";
import { verifyOfferClaims } from "./client_open.ts";

const LOCAL = {
  connectionId: "C",
  sessionKey: "consumer",
  principalId: "pru",
  participantId: "pau",
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
      participantId: "pau",
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

Deno.test("P09 an offer from a non-selected deployment is rejected", () => {
  assertThrows(
    () =>
      verifyOfferClaims({
        offer: offer(),
        selectedProviderDeploymentId: "dep-other",
        localContextDigest: "digest",
        local: LOCAL,
      }),
    Error,
    "selected deployment",
  );
});

Deno.test("P09 an offer with a mutated consumer tuple is rejected", () => {
  const mutated = offer();
  mutated.consumer.participantId = "other";
  assertThrows(
    () =>
      verifyOfferClaims({
        offer: mutated,
        selectedProviderDeploymentId: "dep",
        localContextDigest: "digest",
        local: LOCAL,
      }),
    Error,
    "opening caller",
  );
});

Deno.test("P09 an offer without a pinned local context is rejected", () => {
  assertThrows(
    () =>
      verifyOfferClaims({
        offer: offer(),
        selectedProviderDeploymentId: "dep",
        localContextDigest: undefined,
        local: LOCAL,
      }),
    Error,
    "opening caller",
  );
});

Deno.test("P09 the selected deployment and complete tuple are accepted", () => {
  verifyOfferClaims({
    offer: offer(),
    selectedProviderDeploymentId: "dep",
    localContextDigest: "digest",
    local: LOCAL,
  });
});
