import { deepEqual, equal, rejects } from "node:assert/strict";

import {
  CATALOG_PAGE_LIMIT,
  CursorCycleError,
  mapWithConcurrency,
  resolveExact,
  TABLE_PAGE_LIMIT,
  tablePage,
  traverseAll,
} from "./paging.ts";

Deno.test("U03 page builders emit nested page only and reset cursors", () => {
  deepEqual(tablePage(), { limit: TABLE_PAGE_LIMIT });
  deepEqual(tablePage("after-50"), {
    limit: TABLE_PAGE_LIMIT,
    cursor: "after-50",
  });
  equal(TABLE_PAGE_LIMIT, 50);
  equal(CATALOG_PAGE_LIMIT, 100);
});

Deno.test("U03 the first page alone is never reported complete", async () => {
  let call = 0;
  const first = await traverseAll<string>(() => {
    call += 1;
    if (call === 1) return Promise.resolve({ items: ["a", "b"], cursor: "p2" });
    return Promise.reject(new Error("page 2 failed"));
  });
  equal(first.complete, false);
  deepEqual(first.items, ["a", "b"]);
  equal(first.error instanceof Error, true);
  equal(call, 2);
});

Deno.test("U04 traversal reaches a target beyond the first page", async () => {
  const pages: Record<string, { items: string[]; cursor?: string }> = {
    "": { items: ["a", "b"], cursor: "p2" },
    "p2": { items: ["c", "d"], cursor: "p3" },
    "p3": { items: ["target", "e"] },
  };
  const result = await resolveExact<string>(
    (page) => Promise.resolve(pages[page.cursor ?? ""]),
    (item) => item === "target",
  );
  deepEqual(result, { item: "target", complete: true });
});

Deno.test("U04 a repeated cursor fails visibly instead of looping", async () => {
  let calls = 0;
  const result = await traverseAll<string>(() => {
    calls += 1;
    if (calls > 5) throw new Error("traversal looped");
    return Promise.resolve({ items: ["x"], cursor: "same" });
  });
  equal(result.complete, false);
  equal(result.items.length, 2);
  equal(calls, 2);
  equal(result.error instanceof CursorCycleError, true);
});

Deno.test("U04 a failed later page marks the catalog incomplete", async () => {
  let call = 0;
  const result = await traverseAll<string>(() => {
    call += 1;
    if (call === 2) return Promise.reject(new Error("page 2 failed"));
    return Promise.resolve({ items: ["a"], cursor: "p2" });
  });
  equal(result.complete, false);
  deepEqual(result.items, ["a"]);
  equal(result.error instanceof Error, true);
});

Deno.test("U05 an absent exact target never returns the first row", async () => {
  const result = await resolveExact<string>(
    () => Promise.resolve({ items: ["other-a", "other-b"] }),
    (item) => item === "missing",
  );
  equal(result.item, undefined);
  equal(result.complete, true);
});

Deno.test("U05 an unresolved traversal reports incompleteness, not a match", async () => {
  const result = await resolveExact<string>(
    () => Promise.reject(new Error("denied")),
    () => true,
  );
  equal(result.item, undefined);
  equal(result.complete, false);
});

Deno.test("bounded concurrency never exceeds its limit and keeps order", async () => {
  let active = 0;
  let peak = 0;
  const results = await mapWithConcurrency<number, number>(
    [1, 2, 3, 4, 5, 6, 7],
    4,
    async (item) => {
      active += 1;
      peak = Math.max(peak, active);
      await new Promise((resolve) => setTimeout(resolve, 1));
      active -= 1;
      return item * 2;
    },
  );
  equal(peak, 4);
  deepEqual(results, [2, 4, 6, 8, 10, 12, 14]);
  await rejects(
    () => mapWithConcurrency([1], 0, () => Promise.resolve(1)),
    RangeError,
  );
});
