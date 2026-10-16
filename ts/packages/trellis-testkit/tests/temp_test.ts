import assert from "node:assert/strict";
import { rm } from "node:fs/promises";
import { dirname } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { tempSqlitePath } from "../src/temp.ts";

test("a temporary SQLite path supports a durable database round trip", async () => {
  const path = await tempSqlitePath("service.sqlite");
  try {
    const writer = new DatabaseSync(path);
    try {
      writer.exec("CREATE TABLE records(value TEXT NOT NULL)");
      writer.prepare("INSERT INTO records VALUES (?)").run("retained");
    } finally {
      writer.close();
    }
    const reader = new DatabaseSync(path);
    try {
      assert.equal(
        reader.prepare("SELECT value FROM records").get()?.value,
        "retained",
      );
    } finally {
      reader.close();
    }
  } finally {
    await rm(dirname(path), { recursive: true });
  }
});
