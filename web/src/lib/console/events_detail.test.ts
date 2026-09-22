import { assert, assertEquals } from "@std/assert";
import {
  consumerSeverityOf,
  consumerStatus,
  DetailOwnership,
  followUpIfMounted,
  projectEventsHealthLedger,
  runDeadLetterMutation,
  UNKNOWN_CONSUMER_SEVERITY,
} from "./events_detail.ts";

Deno.test("V09 a detail read from an older list key cannot publish", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("focus=all");
  const token = ownership.begin("event", "evt_1");
  assertEquals(ownership.owns(token), true);

  // A filter change ends the read that belonged to the old query.
  ownership.setListKey("focus=exceptions");
  assertEquals(ownership.owns(token), false);
  assertEquals(ownership.kind, null);
  assertEquals(ownership.selectedId, null);
});

Deno.test("V09 a late completion after a newer selection cannot repopulate old detail", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("focus=all");
  const first = ownership.begin("event", "evt_a");
  // Selecting B clears A's visible detail synchronously.
  const second = ownership.begin("event", "evt_b");
  assertEquals(ownership.owns(second), true);
  assertEquals(ownership.owns(first), false);
  assertEquals(ownership.selectedId, "evt_b");
});

Deno.test("V10 consumer A's DLQ cannot populate consumer B", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("focus=all");
  const a = ownership.begin("consumer", "consumer_a");
  const b = ownership.begin("consumer", "consumer_b");
  assertEquals(ownership.owns(a), false);
  assertEquals(ownership.owns(b), true);

  // Changing kind clears the counterpart selection.
  const event = ownership.begin("event", "evt_1");
  assertEquals(ownership.kind, "event");
  assertEquals(ownership.owns(b), false);
  assertEquals(ownership.owns(event), true);
});

Deno.test("invalidate ends every in-flight read and clears selection", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("q1");
  const token = ownership.begin("event", "evt_1");
  ownership.invalidate();
  assertEquals(ownership.owns(token), false);
  assertEquals(ownership.kind, null);
  assertEquals(ownership.selectedId, null);
});

Deno.test("a same-key refresh may retain the current selection", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("q1");
  const token = ownership.begin("consumer", "consumer_a");
  ownership.setListKey("q1");
  assertEquals(ownership.owns(token), true);
});

Deno.test("V11 an unknown consumer status stays visible and sorts neutrally", () => {
  // A valid record with an unrecognized status is retained, never converted
  // to a known value or dropped.
  const unknown = consumerStatus("paused");
  assertEquals(unknown, "unknown:paused");
  assertEquals(consumerSeverityOf(unknown), UNKNOWN_CONSUMER_SEVERITY);

  // A known status keeps its normal ordering and identity.
  assertEquals(consumerStatus("current"), "current");
  assertEquals(consumerSeverityOf("current"), 6);

  // Unknown data sorts after every known status rather than masquerading as
  // healthy or failing.
  for (
    const known of [
      "missing",
      "saturated",
      "inactive",
      "failing",
      "behind",
      "processing",
      "current",
      "orphaned",
      "unmanaged",
    ] as const
  ) {
    assert(
      consumerSeverityOf(known) < UNKNOWN_CONSUMER_SEVERITY,
      `${known} must sort before an unknown status`,
    );
  }

  // Empty and non-string-ish values remain distinguishable from known states.
  assertEquals(consumerStatus(null), "unknown:null");
  assertEquals(consumerStatus(""), "unknown:");
});

Deno.test("Z05 a ready empty optional read may render zero; pending and unavailable may not", () => {
  const ready = projectEventsHealthLedger({
    metricsState: "ready",
    consumersState: "ready",
    total: 0n,
    integrityExceptions: 0n,
    unresolved: 0n,
    payloadTotalLabel: "0 B",
    payloadAverageLabel: "0 B",
    windowMinutes: 60,
    attentionConsumers: 0,
    shownConsumers: 0,
    oldestLagValue: "-",
    oldestLagDetail: "no pending events",
    oldestLagDisabled: true,
    focus: "exceptions",
    attentionConsumersOnly: false,
    oldestLagActive: false,
  });
  const flow = ready.find((item) => item.id === "all");
  assertEquals(flow?.value, "0");
  assertEquals(flow?.detail.includes("0"), true);

  const pending = projectEventsHealthLedger({
    metricsState: "pending",
    consumersState: "pending",
    payloadTotalLabel: "0 B",
    payloadAverageLabel: "0 B",
    windowMinutes: 60,
    attentionConsumers: 0,
    shownConsumers: 0,
    oldestLagValue: "-",
    oldestLagDetail: "no pending events",
    oldestLagDisabled: true,
    focus: "exceptions",
    attentionConsumersOnly: false,
    oldestLagActive: false,
  });
  for (const item of pending) {
    assertEquals(
      String(item.value) === "0",
      false,
      `${item.id} pending is not zero`,
    );
    const label = String(item.value).toLowerCase();
    assertEquals(
      label.includes("unavailable") || label === "…" || label === "unknown",
      true,
      `${item.id} pending label is ${item.value}`,
    );
  }

  const unavailable = projectEventsHealthLedger({
    metricsState: "unavailable",
    consumersState: "unavailable",
    payloadTotalLabel: "0 B",
    payloadAverageLabel: "0 B",
    windowMinutes: 60,
    attentionConsumers: 0,
    shownConsumers: 0,
    oldestLagValue: "-",
    oldestLagDetail: "no pending events",
    oldestLagDisabled: true,
    focus: "all",
    attentionConsumersOnly: false,
    oldestLagActive: false,
  });
  const deniedFlow = unavailable.find((item) => item.id === "all");
  assertEquals(deniedFlow?.value, "Unavailable");
  assertEquals(deniedFlow?.detail, "Metrics unavailable");
  assertEquals(deniedFlow?.disabled, true);
  const deniedLag = unavailable.find((item) => item.id === "oldest-lag");
  assertEquals(deniedLag?.value, "Unknown");
  assertEquals(deniedLag?.detail, "Consumer health unavailable");
  const deniedConsumers = unavailable.find((item) => item.id === "consumers");
  assertEquals(deniedConsumers?.value, "Unavailable");
});

Deno.test("Z06 a denied optional read leaves the primary numeric path untouched", () => {
  const mixed = projectEventsHealthLedger({
    metricsState: "unavailable",
    consumersState: "ready",
    payloadTotalLabel: "0 B",
    payloadAverageLabel: "0 B",
    windowMinutes: 15,
    attentionConsumers: 2,
    shownConsumers: 4,
    oldestLagValue: "3s",
    oldestLagDetail: "worker-a",
    oldestLagDisabled: false,
    focus: "all",
    attentionConsumersOnly: false,
    oldestLagActive: false,
  });
  assertEquals(mixed.find((item) => item.id === "all")?.value, "Unavailable");
  assertEquals(mixed.find((item) => item.id === "all")?.disabled, true);
  assertEquals(mixed.find((item) => item.id === "consumers")?.value, 2);
  assertEquals(mixed.find((item) => item.id === "consumers")?.disabled, false);
  assertEquals(mixed.find((item) => item.id === "oldest-lag")?.value, "3s");
});

Deno.test("Z09 a successful mutation does not follow up after dispose", async () => {
  let mounted = true;
  let followUps = 0;
  let errors = 0;
  let release = () => {};
  const mutate = new Promise<void>((resolve) => {
    release = resolve;
  });

  const run = runDeadLetterMutation({
    mounted: () => mounted,
    mutate: () => mutate,
    followUp: async () => {
      followUps += 1;
    },
    onError: () => {
      errors += 1;
    },
  });

  mounted = false;
  release();
  await run;
  assertEquals(followUps, 0);
  assertEquals(errors, 0);
});

Deno.test("Z09 a successful mutation follows up while still mounted", async () => {
  let followUps = 0;
  const followed = await followUpIfMounted(
    () => true,
    async () => {
      followUps += 1;
    },
  );
  assertEquals(followed, true);
  assertEquals(followUps, 1);
});
