import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  extractNativeArchive,
  nativeTarget,
  verifyNativeArchive,
} from "../src/native_distribution.ts";
import { create } from "tar";

test("native target mapping selects the actual release architecture", () => {
  assert.equal(nativeTarget("linux", "x64"), "x86_64-unknown-linux-gnu");
  assert.equal(nativeTarget("darwin", "arm64"), "aarch64-apple-darwin");
  assert.throws(() => nativeTarget("win32", "x64"), /do not support/);
});

test("native integrity rejects corrupt archives before extraction", async () => {
  const bytes = new TextEncoder().encode("a released archive");
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  const pin = Array.from(digest, (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
  await verifyNativeArchive(bytes, pin);
  bytes[0] ^= 1;
  await assert.rejects(
    () => verifyNativeArchive(bytes, pin),
    /SHA-256 mismatch/,
  );
});

test("native extraction accepts the one regular command and rejects symlinks or extras", async () => {
  const root = await mkdtemp(join(tmpdir(), "native-archive-security-"));
  const pack = async (file: string, entries: string[]) => {
    await create(
      { cwd: root, file: join(root, file), gzip: true, portable: true },
      entries,
    );
    return await readFile(join(root, file));
  };
  try {
    await writeFile(join(root, "trellis-server"), "server-content");
    await writeFile(join(root, "trellis"), "cli-content");
    const server = await extractNativeArchive(
      await pack("valid.tar.gz", ["trellis-server"]),
      "trellis-server",
    );
    assert.equal(new TextDecoder().decode(server), "server-content");
    const extra = await pack("extra.tar.gz", ["trellis-server", "trellis"]);
    await assert.rejects(
      () => extractNativeArchive(extra, "trellis-server"),
      /only a regular trellis-server/,
    );
    await rm(join(root, "trellis-server"));
    await symlink("trellis", join(root, "trellis-server"));
    const unsafe = await pack("unsafe.tar.gz", ["trellis-server"]);
    await assert.rejects(
      () => extractNativeArchive(unsafe, "trellis-server"),
      /only a regular trellis-server/,
    );
  } finally {
    await rm(root, { recursive: true });
  }
});
