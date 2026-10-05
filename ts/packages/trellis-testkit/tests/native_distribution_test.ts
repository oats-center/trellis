import { assertEquals, assertRejects, assertThrows } from "@std/assert";
import {
  extractNativeArchive,
  nativeTarget,
  verifyNativeArchive,
} from "../src/native_distribution.ts";
import { create } from "tar";
import { join } from "@std/path";

Deno.test("native target mapping selects the actual release architecture", () => {
  assertEquals(nativeTarget("linux", "x86_64"), "x86_64-unknown-linux-gnu");
  assertEquals(nativeTarget("darwin", "arm64"), "aarch64-apple-darwin");
  assertThrows(
    () => nativeTarget("windows", "x86_64"),
    Error,
    "do not support",
  );
});

Deno.test("native integrity rejects corrupt archives before extraction", async () => {
  const bytes = new TextEncoder().encode("a released archive");
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  const pin = Array.from(digest, (byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  await verifyNativeArchive(bytes, pin);
  bytes[0] ^= 1;
  await assertRejects(
    () => verifyNativeArchive(bytes, pin),
    Error,
    "SHA-256 mismatch",
  );
});

Deno.test("native extraction accepts regular commands and rejects executable symlinks", async () => {
  const root = await Deno.makeTempDir({ prefix: "native-archive-security-" });
  try {
    await Deno.writeTextFile(join(root, "trellis"), "cli-content");
    await Deno.writeTextFile(join(root, "trellis-server"), "server-content");
    await create({
      cwd: root,
      file: join(root, "valid.tar.gz"),
      gzip: true,
      portable: true,
    }, ["trellis", "trellis-server"]);
    const files = await extractNativeArchive(
      await Deno.readFile(join(root, "valid.tar.gz")),
    );
    assertEquals(new TextDecoder().decode(files.cli), "cli-content");
    assertEquals(new TextDecoder().decode(files.server), "server-content");
    await Deno.remove(join(root, "trellis-server"));
    await Deno.symlink("trellis", join(root, "trellis-server"));
    await create({
      cwd: root,
      file: join(root, "unsafe.tar.gz"),
      gzip: true,
      portable: true,
    }, ["trellis", "trellis-server"]);
    await assertRejects(
      () =>
        extractNativeArchive(Deno.readFileSync(join(root, "unsafe.tar.gz"))),
      Error,
      "regular trellis",
    );
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});
