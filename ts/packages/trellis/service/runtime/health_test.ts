import { assert, assertEquals } from "@std/assert";
import Value from "typebox/value";

import { healthHeartbeatSubject } from "../../health_transport.ts";
import { HealthHeartbeatSampleCodec } from "../../internal_sdk/generated/types/_internal/p0.js";
import { createHealthHeartbeatSample, ServiceHealthRuntime } from "./health.ts";
import { HealthResponseSchema } from "./health_schemas.ts";

Deno.test("createHealthHeartbeatSample includes participant metadata", () => {
  const heartbeat = createHealthHeartbeatSample({
    serviceName: "activity",
    instanceId: "instance-1",
    contractId: "trellis.audit@v1",
    contractDigest: "digest",
    startedAt: "2026-01-01T00:00:00.000Z",
    publishIntervalMs: 30_000,
    checks: [{ name: "nats", status: "ok", latencyMs: 1 }],
    info: {
      version: "1.2.3",
      info: { build: "abc123" },
    },
  });

  assertEquals(heartbeat.participant.name, "activity");
  assertEquals(heartbeat.participant.instanceId, "instance-1");
  assertEquals(heartbeat.participant.version, "1.2.3");
  assertEquals(heartbeat.participant.publishIntervalMs, 30_000n);
  const encoded = HealthHeartbeatSampleCodec.encode(heartbeat);
  assert(encoded && typeof encoded === "object");
  const participant = Reflect.get(encoded, "participant");
  assert(participant && typeof participant === "object");
  assertEquals(
    Reflect.get(participant, "publishIntervalMs"),
    "30000",
  );
  assertEquals(heartbeat.reportedStatus, "healthy");
});

Deno.test("healthHeartbeatSubject encodes authoritative identity tokens", () => {
  assertEquals(
    healthHeartbeatSubject({
      sessionKey: "session_key",
      participantKind: "service",
      contractId: "trellis.jobs@v1",
      contractDigest: "digest-alpha",
      deploymentId: "jobs.default",
      instanceId: "rust-1",
    }),
    "health.v1.heartbeat.service.dHJlbGxpcy5qb2JzQHYx.ZGlnZXN0LWFscGhh.am9icy5kZWZhdWx0.cnVzdC0x.session_key",
  );
});

Deno.test("ServiceHealth aggregates registered checks and info", async () => {
  const health = new ServiceHealthRuntime({
    serviceName: "activity",
    contractId: "trellis.audit@v1",
    contractDigest: "digest",
    publishIntervalMs: 30_000,
  });

  health.setInfo({
    version: "1.2.3",
    info: { build: "abc123" },
  });
  health.add("nats", () => ({ status: "ok" }));
  health.add("db", () => ({
    status: "failed",
    summary: "connection timeout",
  }));

  const response = await health.response();
  const heartbeat = await health.sample();

  assertEquals(response.status, "degraded");
  assertEquals(response.checks.length, 2);
  assertEquals(heartbeat.participant.contractId, "trellis.audit@v1");
  assertEquals(heartbeat.participant.version, "1.2.3");
  assertEquals(heartbeat.reportedStatus, "degraded");
});

Deno.test("health wire schemas accept additive fields", () => {
  assert(Value.Check(HealthResponseSchema, {
    status: "healthy",
    service: "activity",
    timestamp: "2026-01-01T00:00:00.000Z",
    checks: [{
      name: "nats",
      status: "ok",
      latencyMs: 1,
      region: "primary",
    }],
    requestId: "req_123",
  }));
  assertEquals(
    HealthHeartbeatSampleCodec.decode({
      sample: {
        id: "01J00000000000000000000000",
        time: "2026-01-01T00:00:00.000Z",
        source: "activity",
      },
      participant: {
        name: "activity",
        kind: "service",
        instanceId: "instance-1",
        contractId: "trellis.audit@v1",
        contractDigest: "digest",
        startedAt: "2026-01-01T00:00:00.000Z",
        publishIntervalMs: "30000",
        runtime: "deno",
        region: "primary",
      },
      reportedStatus: "healthy",
      checks: [{
        name: "nats",
        status: "ok",
        latencyMs: 1,
        region: "primary",
      }],
      requestId: "req_123",
    }).reportedStatus,
    "healthy",
  );
});
