import { assert, assertEquals } from "@std/assert";
import { basename } from "@std/path";
import { sqliteMemoryUrl, tempSqlitePath } from "../src/temp.ts";

Deno.test("sqliteMemoryUrl returns in-memory SQLite URL", () => {
  assertEquals(sqliteMemoryUrl(), ":memory:");
});

Deno.test("tempSqlitePath creates a path with requested basename", async () => {
  const path = await tempSqlitePath("service.sqlite");

  assertEquals(basename(path), "service.sqlite");
  assert(path.includes("trellis-test-sqlite-"));
});
