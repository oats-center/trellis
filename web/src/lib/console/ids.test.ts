import assert from "node:assert/strict";
import { test } from "node:test";
import { idLabels } from "./ids.ts";

test("IDs read without their workspace until two packages share a short ID", () => {
  const labels = idLabels([
    "ghcr.io/krishi-diagnostics/krishi.SherpaService",
    "ghcr.io/qlever-llc/krishi.SherpaService",
    "localhost/orders.Billing",
    "trellis.auth@v1",
  ]);
  assert.deepEqual(
    [...labels.values()],
    [
      "ghcr.io/krishi-diagnostics/krishi.SherpaService",
      "ghcr.io/qlever-llc/krishi.SherpaService",
      "orders.Billing",
      "trellis.auth@v1",
    ],
  );
});
