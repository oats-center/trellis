import { equal } from "node:assert/strict";
import { test } from "node:test";

import { parseWebhookSetting, showWebhookSetting } from "./webhook_settings.ts";

test("a webhook setting takes only its own units", () => {
  equal(parseWebhookSetting("512KiB", "bytes"), 524_288n);
  equal(parseWebhookSetting("5m", "ms"), 300_000n);
  equal(parseWebhookSetting(" 7 d ", "ms"), 604_800_000n);
  equal(parseWebhookSetting("50", "count"), 50n);
  // A duration is not a size, a size is not a duration, and a count takes
  // no unit.
  equal(parseWebhookSetting("5m", "bytes"), null);
  equal(parseWebhookSetting("2h", "bytes"), null);
  equal(parseWebhookSetting("512KiB", "ms"), null);
  equal(parseWebhookSetting("1B", "ms"), null);
  equal(parseWebhookSetting("5s", "count"), null);
});

test("a shown webhook setting parses back to the same value", () => {
  for (const [value, unit] of [
    [262_144n, "bytes"],
    [1000n, "bytes"],
    [86_400_000n, "ms"],
    [1500n, "ms"],
    [20n, "count"],
  ] as const) {
    equal(parseWebhookSetting(showWebhookSetting(value, unit), unit), value);
  }
});
