import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** Returns a fresh path suitable for a service-owned SQLite database. */
export async function tempSqlitePath(name = "test.sqlite"): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "trellis-testkit-sqlite-"));
  return join(dir, name);
}

/** Returns the SQLite in-memory URL used by service-owned tests. */
export function sqliteMemoryUrl(): string {
  return ":memory:";
}
