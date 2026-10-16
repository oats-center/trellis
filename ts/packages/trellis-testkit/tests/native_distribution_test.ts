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

test("native extraction accepts regular commands and rejects executable symlinks", async () => {
  const root = await mkdtemp(join(tmpdir(), "native-archive-security-"));
  try {
    await writeFile(join(root, "trellis"), "cli-content");
    await writeFile(join(root, "trellis-server"), "server-content");
    await create(
      {
        cwd: root,
        file: join(root, "valid.tar.gz"),
        gzip: true,
        portable: true,
      },
      ["trellis", "trellis-server"],
    );
    const files = await extractNativeArchive(
      await readFile(join(root, "valid.tar.gz")),
    );
    assert.equal(new TextDecoder().decode(files.cli), "cli-content");
    assert.equal(new TextDecoder().decode(files.server), "server-content");
    await rm(join(root, "trellis-server"));
    await symlink("trellis", join(root, "trellis-server"));
    await create(
      {
        cwd: root,
        file: join(root, "unsafe.tar.gz"),
        gzip: true,
        portable: true,
      },
      ["trellis", "trellis-server"],
    );
    const unsafe = await readFile(join(root, "unsafe.tar.gz"));
    await assert.rejects(() => extractNativeArchive(unsafe), /regular trellis/);
  } finally {
    await rm(root, { recursive: true });
  }
});
