import assert from "node:assert/strict";
import { test } from "node:test";

import { withTrellisRuntime } from "./_support/runtime.ts";

test("API search finds an API by its title wherever it falls in the catalog", async () => {
  await withTrellisRuntime(async (runtime) => {
    const list = async (search?: string, limit = 50) => {
      const found = [];
      let cursor: string | undefined;
      do {
        const page = await runtime.callAdminRpc("authApisList", {
          page: { cursor, limit },
          ...(search === undefined ? {} : { search }),
        });
        found.push(...page.items);
        cursor = page.page.nextCursor;
      } while (cursor);
      return found;
    };
    const catalog = await list();
    // The last API in the catalog whose title isn't part of its id, so only a
    // search of what people read finds it.
    const target = catalog.findLast(
      (api) =>
        api.title !== "" &&
        !api.apiId.toLowerCase().includes(api.title.toLowerCase()),
    );
    assert(target, "the catalog has an API titled apart from its id");
    const term = target.title.toUpperCase();
    const expected = catalog
      .filter((api) =>
        `${api.title} ${api.description} ${api.apiId}`
          .toLowerCase()
          .includes(term.toLowerCase()),
      )
      .map((api) => api.apiId);

    // One API per page, so the earlier pages hold nothing that matches.
    const matched = (await list(term, 1)).map((api) => api.apiId);
    assert.deepEqual(matched, expected);
    assert(matched.includes(target.apiId));
  });
});
