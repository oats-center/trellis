import assert from "node:assert/strict";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setImmediate } from "node:timers/promises";
import { test } from "node:test";
import { ulid } from "ulid";
import { spawnTracked } from "../../ts/integration/_support/process.ts";

test("a subprocess startup failure stays observable when its owner waits on a later turn", async () => {
  const child = spawnTracked(join(tmpdir(), `trellis-missing-${ulid()}`), []);
  await setImmediate();
  await assert.rejects(child.exited, { code: "ENOENT" });
});
