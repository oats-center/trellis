import { deepEqual, equal } from "node:assert/strict";

import {
  bulkExpectedCount,
  bulkTargetDetails,
  MAX_BULK_CONCURRENCY,
  pruneSelection,
  runBulk,
  toggleAll,
  toggleId,
} from "./bulk.ts";

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

Deno.test("U07 bulk mutations never exceed four concurrent targets", async () => {
  let active = 0;
  let peak = 0;
  const outcome = await runBulk(
    Array.from({ length: 11 }, (_, index) => index),
    async () => {
      active += 1;
      peak = Math.max(peak, active);
      await delay(5);
      active -= 1;
    },
  );
  equal(peak, MAX_BULK_CONCURRENCY);
  equal(peak, 4);
  equal(outcome.succeeded, 11);
  deepEqual(outcome.failed, []);
});

Deno.test("U07 each target classifies success, failure, and unknown separately", async () => {
  const outcome = await runBulk(
    ["ok-1", "fail-1", "unknown-1", "ok-2"],
    (target) => {
      if (target.startsWith("fail")) return Promise.reject(new Error("denied"));
      if (target.startsWith("unknown")) {
        return Promise.reject(new Error("transport"));
      }
      return Promise.resolve();
    },
    (error) => String(error) === "Error: transport" ? "unknown" : "failed",
  );
  equal(outcome.succeeded, 2);
  deepEqual(outcome.failed.map((item) => item.target), ["fail-1"]);
  deepEqual(outcome.unknown.map((item) => item.target), ["unknown-1"]);
  equal(outcome.outcomes.length, 4);
  equal(
    outcome.outcomes.filter((item) => item.kind === "succeeded").length,
    2,
  );
});

Deno.test("selection helpers prune stale ids and toggle exact sets", () => {
  const selected = new Set(["a", "b", "gone"]);
  pruneSelection(selected, ["a", "b", "c"]);
  deepEqual([...selected], ["a", "b"]);

  toggleId(selected, "a");
  deepEqual([...selected], ["b"]);
  toggleAll(selected, ["b", "c"]);
  deepEqual([...selected].sort(), ["b", "c"]);
  toggleAll(selected, ["b", "c"]);
  deepEqual([...selected], []);
});

Deno.test("bulk confirmation requires typing only above the threshold", () => {
  equal(bulkExpectedCount(5), undefined);
  equal(bulkExpectedCount(6), "6");
  equal(bulkTargetDetails(["a", "b"]), "a\nb");
  equal(
    bulkTargetDetails(["a", "b", "c", "d", "e", "f"]),
    "a\nb\nc\nd\ne\nand 1 more",
  );
});
