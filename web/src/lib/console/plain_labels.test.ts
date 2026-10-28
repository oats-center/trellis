import assert from "node:assert/strict";
import { test } from "node:test";
import {
  apiLabel,
  friendlyDuration,
  deliveryStatus,
  errorText,
  eventCheckLabel,
  jobStateLabel,
  sentenceName,
} from "./plain_labels.ts";

test("names read as short sentences", () => {
  assert.equal(sentenceName("Connections.Opened"), "Connections opened");
  assert.equal(sentenceName("StatusChanged"), "Status changed");
  assert.equal(sentenceName("Heartbeat"), "Heartbeat");
  assert.equal(sentenceName("keyed_work"), "Keyed work");
  assert.equal(sentenceName("documents.Documents"), "Documents");
});

test("API IDs show their short name", () => {
  assert.equal(apiLabel("trellis.health@v1"), "Health");
  assert.equal(apiLabel("runtime-trellis.events@v1"), "Events");
  assert.equal(apiLabel(undefined), "Unknown");
});

test("delivery states say what is wrong", () => {
  assert.deepEqual(deliveryStatus("current").attention, false);
  assert.deepEqual(deliveryStatus("failing").label, "Retrying");
  assert.deepEqual(deliveryStatus("inactive").attention, true);
  assert.deepEqual(deliveryStatus("someNewState").label, "Some new state");
});

test("event checks name the problem", () => {
  assert.deepEqual(eventCheckLabel("verified", "resolved"), {
    label: "Verified",
    ok: true,
  });
  assert.deepEqual(eventCheckLabel("missing-session", "resolved"), {
    label: "Sender not signed in",
    ok: false,
  });
  assert.deepEqual(eventCheckLabel("verified", "unresolved").ok, false);
});

test("job states are plain words", () => {
  assert.equal(jobStateLabel({ state: "dead" }).label, "Failed for good");
  assert.equal(
    jobStateLabel({ state: "active", waitingOn: [{}] }).label,
    "Waiting",
  );
  assert.equal(
    jobStateLabel({ state: "active", runtimeBand: "slow" }).label,
    "Slow",
  );
  assert.equal(jobStateLabel({ state: "pending" }).label, "Queued");
});

test("serialized errors show their cause", () => {
  const raw = JSON.stringify({
    id: "01M5N1W3RMBDAQ5QV4S5PMPQN6",
    type: "UnexpectedError",
    message: "An unexpected error has occurred",
    context: { causeMessage: "Could not reach the invoice store" },
  });
  assert.equal(errorText(raw), "Could not reach the invoice store");
  assert.equal(
    errorText(
      JSON.stringify({ type: "RetryJobError", message: "Retry requested" }),
    ),
    "Retry requested",
  );
  assert.equal(errorText("plain failure"), "plain failure");
  assert.equal(errorText(undefined), undefined);
});

test("durations read the way people say them", () => {
  assert.equal(friendlyDuration(359), "359ms");
  assert.equal(friendlyDuration(2_184), "2.2s");
  assert.equal(friendlyDuration(33_483), "33s");
  assert.equal(friendlyDuration(252_000), "4m 12s");
  assert.equal(friendlyDuration(3 * 3_600_000 + 300_000), "3h 5m");
  assert.equal(friendlyDuration(52 * 3_600_000), "2d 4h");
});

test("system messages are put plainly", () => {
  assert.equal(
    errorText("broker max deliveries exceeded"),
    "Gave up after the most tries allowed",
  );
});

test("capabilities group by API area", async () => {
  const { capabilityArea, groupByArea } = await import("./capability_areas.ts");
  assert.deepEqual(capabilityArea("trellis.auth.v1:users.edit"), {
    id: "trellis.auth.v1:users",
    title: "Users",
    api: "Auth",
  });
  assert.deepEqual(capabilityArea("trellis.auth.v1:account").title, "Account");
  const groups = groupByArea(
    [
      "trellis.auth.v1:users.edit",
      "trellis.auth.v1:users.read",
      "trellis.events.v1:deadLetters.read",
    ],
    (id) => id,
  );
  assert.deepEqual(
    groups.map((group) => [group.title, group.items.length]),
    [
      ["Dead letters", 1],
      ["Users", 2],
    ],
  );
});
