import { deepEqual, equal } from "node:assert/strict";
import { test } from "node:test";

import { instanceStatus, type ReportedInstance } from "./instance_status.ts";

const report = (observedAt: string, phase?: string): ReportedInstance => ({
  contractDigest: "new-build",
  effectiveStatus: "healthy",
  observedAt,
  latestSample: {
    ...(phase ? { phase } : {}),
    checks: [],
    participant: { version: "2.0.0" },
    sample: { time: observedAt },
  },
});

test("a heartbeat from before the current connection does not say what the instance does now", () => {
  // The process restarted with a build that waits for approval; its previous
  // process last reported serving.
  const status = instanceStatus(
    {
      connectedAt: "2026-11-19T10:05:00Z",
      participantDigest: "old-build",
      waiting: {
        update: { updateId: "u", reviewUrl: "https://t/review" },
        missingSettings: [],
      },
    },
    report("2026-11-19T10:04:00Z", "serving"),
    "old-build",
  );
  equal(status.state, "waitingForApproval");
  equal(status.reviewUrl, "https://t/review");
  deepEqual(status.build, {
    digest: "old-build",
    version: undefined,
    current: true,
  });
});

test("a current heartbeat gives the phase and the build it runs", () => {
  const status = instanceStatus(
    { connectedAt: "2026-11-19T10:05:00Z", participantDigest: "new-build" },
    report("2026-11-19T10:05:01Z", "stopping"),
    "newer-build",
  );
  equal(status.state, "stopping");
  deepEqual(status.build, {
    digest: "new-build",
    version: "2.0.0",
    current: false,
  });
});

test("a disconnected instance is offline whatever it last reported", () => {
  equal(
    instanceStatus({}, report("2026-11-19T10:05:01Z", "serving"), "x").state,
    "offline",
  );
  equal(
    instanceStatus(
      {
        connectedAt: "2026-11-19T10:05:00Z",
        waiting: { missingSettings: ["endpoint"] },
      },
      undefined,
      "x",
    ).state,
    "waitingForSettings",
  );
});

test("a connected instance that has not started serving reads as starting", () => {
  // Connected and heartbeating, but `wait()` has not attached its intake yet.
  equal(
    instanceStatus(
      { connectedAt: "2026-11-19T10:05:00Z" },
      report("2026-11-19T10:05:01Z", "connecting"),
      "x",
    ).state,
    "starting",
  );
  // Connected, no heartbeat yet, nothing to wait for.
  equal(
    instanceStatus({ connectedAt: "2026-11-19T10:05:00Z" }, undefined, "x")
      .state,
    "starting",
  );
});
