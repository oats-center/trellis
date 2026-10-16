import assert from "node:assert/strict";
import { test } from "node:test";
import Value from "typebox/value";

import { HealthHeartbeatSampleCodec } from "../../internal_sdk/generated/types/_internal/p0.js";
import { createHealthHeartbeatSample, ServiceHealthRuntime } from "./health.ts";
import { HealthResponseSchema } from "./health_schemas.ts";

test("health heartbeat codecs round-trip duration and participant data", () => {
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

  assert.deepEqual(heartbeat.participant.publishIntervalMs, 30_000n);
  const encoded = HealthHeartbeatSampleCodec.encode(heartbeat);
  assert(encoded && typeof encoded === "object");
  const participant = Reflect.get(encoded, "participant");
  assert(participant && typeof participant === "object");
  assert.deepEqual(Reflect.get(participant, "publishIntervalMs"), "30000");
  assert.deepEqual(HealthHeartbeatSampleCodec.decode(encoded), heartbeat);
  assert.deepEqual(heartbeat.reportedStatus, "healthy");
});

test("ServiceHealth aggregates registered checks and info", async () => {
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

  assert.deepEqual(response.status, "degraded");
  assert.deepEqual(response.checks.length, 2);
  assert.deepEqual(heartbeat.participant.contractId, "trellis.audit@v1");
  assert.deepEqual(heartbeat.participant.version, "1.2.3");
  assert.deepEqual(heartbeat.reportedStatus, "degraded");
  assert.equal(heartbeat.participant.runtime, "node");
  assert.equal(heartbeat.participant.runtimeVersion, process.version);
});

test("health wire schemas accept additive fields", () => {
  assert(
    Value.Check(HealthResponseSchema, {
      status: "healthy",
      service: "activity",
      timestamp: "2026-01-01T00:00:00.000Z",
      checks: [
        {
          name: "nats",
          status: "ok",
          latencyMs: 1,
          region: "primary",
        },
      ],
      requestId: "req_123",
    }),
  );
  assert.deepEqual(
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
        runtime: "node",
        region: "primary",
      },
      reportedStatus: "healthy",
      checks: [
        {
          name: "nats",
          status: "ok",
          latencyMs: 1,
          region: "primary",
        },
      ],
      requestId: "req_123",
    }).reportedStatus,
    "healthy",
  );
});
