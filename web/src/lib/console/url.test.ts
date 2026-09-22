import { equal } from "node:assert/strict";

import { buildConsoleUrl, queryString } from "./url.ts";

const BASE = "/console";

Deno.test("U13 URL helper round-trips opaque IDs exactly once", () => {
  const id = "participant@v1::with/slash and space";
  const url = buildConsoleUrl(BASE, "/admin/grants/new", {
    query: { portalId: "builtin", participantId: id },
  });
  const parsed = new URL(url, "http://console.local");
  equal(parsed.searchParams.get("portalId"), "builtin");
  equal(parsed.searchParams.get("participantId"), id);
  equal(url.includes("%253A"), false, "IDs are not double-encoded");
});

Deno.test("U13 path params and a configured base path are preserved", () => {
  equal(
    buildConsoleUrl("/base/console", "/admin/services/[deploymentId]", {
      params: { deploymentId: "dep_1" },
    }),
    "/base/console/admin/services/dep_1",
  );
  equal(
    buildConsoleUrl("/base/console", "/(app)/admin/users"),
    "/base/console/admin/users",
  );
});

Deno.test("U13 query encoding drops absent fields and keeps exact bigints", () => {
  equal(queryString({ a: "1", b: undefined, c: null, d: 42n }), "a=1&d=42");
  equal(queryString({}), "");
});

Deno.test("U13 consoleUrl appends only a populated query string", () => {
  equal(buildConsoleUrl(BASE, "/admin/users/edit").includes("?"), false);
  equal(
    buildConsoleUrl(BASE, "/admin/users/edit", {
      query: { userId: "usr_1" },
    }).endsWith("?userId=usr_1"),
    true,
  );
});
