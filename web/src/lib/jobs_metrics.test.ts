import { AsyncResult, UnexpectedError } from "@qlever-llc/result";
import { deepEqual } from "node:assert/strict";
import { type apis } from "trellis-web-generated";

import { loadJobsMetrics } from "./jobs_metrics.ts";

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};

const input: apis.jobs.MetricsInput = {
  groupBy: "type",
  step: "1m",
  window: "1h",
};

for (
  const [name, cause, message] of [
    [
      "bound-subject permission",
      'nats: Permissions Violation for Publish to "rpc.v1.dHJlbGxpcy5qb2JzQHYx.am9icy1ydW50aW1l.Metrics"',
      "Your current session is not approved for Jobs.Metrics. Sign out and sign back in to refresh permissions.",
    ],
    [
      "bound-subject no responder",
      "no responders available for request 'rpc.v1.dHJlbGxpcy5qb2JzQHYx.am9icy1ydW50aW1l.Metrics'",
      "Jobs metrics are not currently reachable.",
    ],
  ] as const
) {
  Deno.test(`loadJobsMetrics normalizes ${name}`, async () => {
    const payload = await loadJobsMetrics({
      metrics: () =>
        AsyncResult.err(
          new UnexpectedError({ cause: new Error(cause) }),
        ),
    }, input);

    deepEqual(payload, { available: false, message });
  });
}
