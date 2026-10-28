import type { NatsConnection } from "@nats-io/nats-core";
import { isErr } from "@oatscenter/result";
import { API as HealthApi } from "./internal_sdk/generated/apis/health/mod.js";
import {
  type HealthHeartbeatSample,
  HealthHeartbeatSampleCodec,
} from "./internal_sdk/generated/types/_internal/p0.js";
import { getApiRuntime } from "./participant_runtime/participant.ts";
import { Trellis, type TrellisAuth } from "./session.ts";
import type { TrellisTransportProvider } from "./transport/generations.ts";

/** Publish the native Health descriptor through ordinary SessionEvent signing,
 * capability/catalog authorization and generation-fenced JetStream publication.
 */
export function createHealthHeartbeatPublisher(args: {
  nc: NatsConnection;
  auth: TrellisAuth;
  transport: TrellisTransportProvider;
  inboxPrefix: string;
}): (sample: HealthHeartbeatSample) => Promise<void> {
  const publisher = new Trellis(
    "native-self-health",
    args.nc,
    args.auth,
    {
      api: getApiRuntime(HealthApi),
      transport: args.transport,
    },
    args.inboxPrefix,
  );
  return async (sampleValue) => {
    const authorityLease = args.auth.authorityOwner?.own();
    if (!authorityLease)
      throw new Error("health publisher has no verified authority");
    try {
      const authority = authorityLease.authority;
      const binding = authority.binding;
      if (binding.kind !== "service" && binding.kind !== "device")
        throw new Error("health publisher is not provisioned");
      const metadata = authority.extensions["trellis.nativeParticipant.v1"];
      if (
        !metadata ||
        typeof metadata !== "object" ||
        Array.isArray(metadata) ||
        metadata.participantId !== binding.participantId ||
        typeof metadata.participantDigest !== "string"
      ) {
        throw new Error("health publisher has no signed participant metadata");
      }
      const sample = HealthHeartbeatSampleCodec.encode({
        ...sampleValue,
        participant: {
          ...sampleValue.participant,
          kind: binding.kind,
          contractId: binding.participantId,
          contractDigest: metadata.participantDigest,
          instanceId: binding.instanceId,
        },
      });
      if (!sample || typeof sample !== "object" || Array.isArray(sample)) {
        throw new Error("invalid encoded health sample");
      }
      if (
        new TextEncoder().encode(JSON.stringify(sample)).byteLength > 65_536
      ) {
        throw new Error("health heartbeat sample exceeds 64 KiB");
      }
      const result = await publisher
        .publish(
          `${HealthApi.identity}:event:Heartbeat`,
          sample as Record<string, unknown>,
        )
        .take();
      if (isErr(result)) throw result.error;
    } finally {
      authorityLease.release();
    }
  };
}
