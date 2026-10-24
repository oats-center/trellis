import assert from "node:assert/strict";
import { test } from "node:test";
import {
  consumerSeverityOf,
  consumerStatus,
  DetailOwnership,
  followUpIfMounted,
  projectEventsHealthLedger,
  runDeadLetterMutation,
  UNKNOWN_CONSUMER_SEVERITY,
} from "./events_detail.ts";

test("V09 a detail read from an older list key cannot publish", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("focus=all");
  const token = ownership.begin("event", "evt_1");
  assert.deepEqual(ownership.owns(token), true);

  // A filter change ends the read that belonged to the old query.
  ownership.setListKey("focus=exceptions");
  assert.deepEqual(ownership.owns(token), false);
  assert.deepEqual(ownership.kind, null);
  assert.deepEqual(ownership.selectedId, null);
});

test("V09 a late completion after a newer selection cannot repopulate old detail", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("focus=all");
  const first = ownership.begin("event", "evt_a");
  // Selecting B clears A's visible detail synchronously.
  const second = ownership.begin("event", "evt_b");
  assert.deepEqual(ownership.owns(second), true);
  assert.deepEqual(ownership.owns(first), false);
  assert.deepEqual(ownership.selectedId, "evt_b");
});

test("V10 consumer A's DLQ cannot populate consumer B", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("focus=all");
  const a = ownership.begin("consumer", "consumer_a");
  const b = ownership.begin("consumer", "consumer_b");
  assert.deepEqual(ownership.owns(a), false);
  assert.deepEqual(ownership.owns(b), true);

  // Changing kind clears the counterpart selection.
  const event = ownership.begin("event", "evt_1");
  assert.deepEqual(ownership.kind, "event");
  assert.deepEqual(ownership.owns(b), false);
  assert.deepEqual(ownership.owns(event), true);
});

test("invalidate ends every in-flight read and clears selection", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("q1");
  const token = ownership.begin("event", "evt_1");
  ownership.invalidate();
  assert.deepEqual(ownership.owns(token), false);
  assert.deepEqual(ownership.kind, null);
  assert.deepEqual(ownership.selectedId, null);
});

test("a same-key refresh may retain the current selection", () => {
  const ownership = new DetailOwnership();
  ownership.setListKey("q1");
  const token = ownership.begin("consumer", "consumer_a");
  ownership.setListKey("q1");
  assert.deepEqual(ownership.owns(token), true);
});

test("V11 an unknown consumer status stays visible and sorts neutrally", () => {
  // A valid record with an unrecognized status is retained, never converted
  // to a known value or dropped.
  const unknown = consumerStatus("paused");
  assert.deepEqual(unknown, "unknown:paused");
  assert.deepEqual(consumerSeverityOf(unknown), UNKNOWN_CONSUMER_SEVERITY);

  // A known status keeps its normal ordering and identity.
  assert.deepEqual(consumerStatus("current"), "current");
  assert.deepEqual(consumerSeverityOf("current"), 6);

  // Unknown data sorts after every known status rather than masquerading as
  // healthy or failing.
  for (const known of [
    "missing",
    "saturated",
    "inactive",
    "failing",
    "behind",
    "processing",
    "current",
    "orphaned",
    "unmanaged",
  ] as const) {
    assert(
      consumerSeverityOf(known) < UNKNOWN_CONSUMER_SEVERITY,
      `${known} must sort before an unknown status`,
    );
  }

  // Empty and non-string-ish values remain distinguishable from known states.
  assert.deepEqual(consumerStatus(null), "unknown:null");
  assert.deepEqual(consumerStatus(""), "unknown:");
});

test("Z05 a ready empty optional read may render zero; pending and unavailable may not", () => {
  const base = {
    windowMinutes: 60,
    attentionConsumers: 0,
    shownConsumers: 0,
    focus: "exceptions",
    attentionConsumersOnly: false,
    failedActive: false,
  };
  const ready = projectEventsHealthLedger({
    ...base,
    metricsState: "ready",
    consumersState: "ready",
    total: 0n,
    integrityExceptions: 0n,
    failedDeliveries: 0n,
  });
  const flow = ready.find((item) => item.id === "all");
  assert.deepEqual(flow?.value, "0");
  assert.deepEqual(flow?.detail.includes("0"), true);
  assert.deepEqual(ready.find((item) => item.id === "failed")?.value, "0");

  const pending = projectEventsHealthLedger({
    ...base,
    metricsState: "pending",
    consumersState: "pending",
  });
  for (const item of pending) {
    assert.deepEqual(
      String(item.value) === "0",
      false,
      `${item.id} pending is not zero`,
    );
    const label = String(item.value).toLowerCase();
    assert.deepEqual(
      label.includes("unavailable") || label === "…",
      true,
      `${item.id} pending label is ${item.value}`,
    );
  }

  const unavailable = projectEventsHealthLedger({
    ...base,
    metricsState: "unavailable",
    consumersState: "unavailable",
    focus: "all",
  });
  const deniedFlow = unavailable.find((item) => item.id === "all");
  assert.deepEqual(deniedFlow?.value, "Unavailable");
  assert.deepEqual(deniedFlow?.detail, "Unavailable right now");
  assert.deepEqual(deniedFlow?.disabled, true);
  const deniedConsumers = unavailable.find((item) => item.id === "consumers");
  assert.deepEqual(deniedConsumers?.value, "Unavailable");
  assert.deepEqual(deniedConsumers?.disabled, true);
});

test("Z06 a denied optional read leaves the primary numeric path untouched", () => {
  const mixed = projectEventsHealthLedger({
    metricsState: "unavailable",
    consumersState: "ready",
    windowMinutes: 15,
    attentionConsumers: 2,
    shownConsumers: 4,
    focus: "all",
    attentionConsumersOnly: false,
    failedActive: false,
  });
  assert.deepEqual(
    mixed.find((item) => item.id === "all")?.value,
    "Unavailable",
  );
  assert.deepEqual(mixed.find((item) => item.id === "all")?.disabled, true);
  assert.deepEqual(mixed.find((item) => item.id === "consumers")?.value, 2);
  assert.deepEqual(
    mixed.find((item) => item.id === "consumers")?.disabled,
    false,
  );
});

test("Z09 a successful mutation does not follow up after dispose", async () => {
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
  assert.deepEqual(followUps, 0);
  assert.deepEqual(errors, 0);
});

test("Z09 a successful mutation follows up while still mounted", async () => {
  let followUps = 0;
  const followed = await followUpIfMounted(
    () => true,
    async () => {
      followUps += 1;
    },
  );
  assert.deepEqual(followed, true);
  assert.deepEqual(followUps, 1);
});
