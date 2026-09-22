import { deepEqual } from "node:assert/strict";

import {
  nextCursorPage,
  previousCursorPage,
  resetCursorHistory,
} from "./cursor_history.ts";

Deno.test("cursor history supports page two, back, and filter reset", () => {
  const pageTwo = nextCursorPage(resetCursorHistory(), "after-200");
  deepEqual(pageTwo, { cursor: "after-200", back: [""] });
  deepEqual(previousCursorPage(pageTwo), { cursor: undefined, back: [] });
  deepEqual(resetCursorHistory(), { back: [] });
});
