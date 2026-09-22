import { jetstream } from "@nats-io/jetstream";
import type { NatsConnection } from "@nats-io/nats-core";
import { base64urlEncode } from "./auth/utils.ts";
import {
  type HealthHeartbeatSample,
  HealthHeartbeatSampleCodec,
} from "./internal_sdk/generated/types/_internal/p0.js";

const HEALTH_HEARTBEAT_SUBJECT_PREFIX = "health.v1.heartbeat";

export type HealthHeartbeatSubjectIdentity = {
  sessionKey: string;
  participantKind: "service" | "device";
  contractId: string;
  contractDigest: string;
  deploymentId: string;
  instanceId: string;
};

function subjectToken(value: string): string {
  if (value.length === 0) throw new Error("health subject identity is empty");
  return base64urlEncode(new TextEncoder().encode(value));
}

/** Builds the exact runtime subject authorized for one heartbeat publisher. */
export function healthHeartbeatSubject(
  identity: HealthHeartbeatSubjectIdentity,
): string {
  if (!/^[A-Za-z0-9_-]+$/.test(identity.sessionKey)) {
    throw new Error("health session key is not canonical base64url");
  }
  return [
    HEALTH_HEARTBEAT_SUBJECT_PREFIX,
    identity.participantKind,
    subjectToken(identity.contractId),
    subjectToken(identity.contractDigest),
    subjectToken(identity.deploymentId),
    subjectToken(identity.instanceId),
    identity.sessionKey,
  ].join(".");
}

/** Publishes one validated-size heartbeat sample to its dedicated stream. */
export async function publishHealthHeartbeatSample(args: {
  nc: NatsConnection;
  identity: HealthHeartbeatSubjectIdentity;
  sample: HealthHeartbeatSample;
}): Promise<void> {
  const payload = new TextEncoder().encode(
    JSON.stringify(HealthHeartbeatSampleCodec.encode(args.sample)),
  );
  if (payload.byteLength > 65_536) {
    throw new Error("health heartbeat sample exceeds 64 KiB");
  }
  await jetstream(args.nc).publish(
    healthHeartbeatSubject(args.identity),
    payload,
    { msgID: args.sample.sample.id },
  );
}
