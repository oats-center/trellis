import { assertEquals, assertRejects, assertThrows } from "@std/assert";
import { gzipSync, zipSync } from "fflate";
import { join } from "@std/path";
import {
  ensureCacheDir,
  ensureNatsBinaries,
  extractTarEntry,
  extractZipEntry,
  isSafeCachedBinary,
  type NatsBinariesManifest,
  type NatsBinaryPlatform,
  natsBinaryPlatform,
  releaseUrl,
  resolveNatsBinaryAsset,
  sha256Hex,
  verifySha256,
} from "../src/nats_binaries.ts";

function concat(blocks: readonly Uint8Array[]): Uint8Array {
  const size = blocks.reduce((sum, block) => sum + block.length, 0);
  const out = new Uint8Array(size);
  let offset = 0;
  for (const block of blocks) {
    out.set(block, offset);
    offset += block.length;
  }
  return out;
}

function ustarArchive(
  entries: readonly {
    name: string;
    content: Uint8Array;
  }[],
): Uint8Array {
  const blocks: Uint8Array[] = [];
  const encoder = new TextEncoder();
  for (const entry of entries) {
    const header = new Uint8Array(512);
    header.set(encoder.encode(entry.name), 0);
    const size = entry.content.length.toString(8).padStart(11, "0") + "\0";
    header.set(encoder.encode(size), 124);
    header[156] = 48; // regular file typeflag '0'
    blocks.push(header, entry.content);
    const padding = (512 - (entry.content.length % 512)) % 512;
    if (padding > 0) blocks.push(new Uint8Array(padding));
  }
  blocks.push(new Uint8Array(1024)); // end-of-archive marker
  return concat(blocks);
}

Deno.test("nats binary platform maps build os/arch to pinned asset names", () => {
  assertEquals(natsBinaryPlatform("linux", "x86_64"), "linux-amd64");
  assertEquals(natsBinaryPlatform("linux", "aarch64"), "linux-arm64");
  assertEquals(natsBinaryPlatform("darwin", "x86_64"), "darwin-amd64");
  assertEquals(natsBinaryPlatform("darwin", "aarch64"), "darwin-arm64");
  assertEquals(natsBinaryPlatform("darwin", "arm64"), "darwin-arm64");
});

Deno.test("nats binary platform rejects unsupported platforms clearly", () => {
  assertThrows(
    () => natsBinaryPlatform("windows", "x86_64"),
    Error,
    "not supported on windows",
  );
  assertThrows(
    () => natsBinaryPlatform("linux", "riscv64"),
    Error,
    "not supported on linux-riscv64",
  );
});

Deno.test("nats binary asset resolution pins the released URL and sha256", () => {
  const server = resolveNatsBinaryAsset("nats-server", "linux", "x86_64");
  assertEquals(server.platform, "linux-amd64");
  assertEquals(
    server.url,
    "https://github.com/nats-io/nats-server/releases/download/v2.14.4/nats-server-v2.14.4-linux-amd64.tar.gz",
  );
  assertEquals(
    server.sha256,
    "20f9d6a199560f243610908bcccea2e27e9f47213242d1c609ca46d1d73e91ea",
  );
  assertEquals(server.fileName, "nats-server-2.14.4-linux-amd64");

  const nsc = resolveNatsBinaryAsset("nsc", "darwin", "aarch64");
  assertEquals(nsc.platform, "darwin-arm64");
  assertEquals(
    nsc.url,
    "https://github.com/nats-io/nsc/releases/download/v2.15.0/nsc-darwin-arm64.zip",
  );
  assertEquals(
    nsc.sha256,
    "18ef004eded116886607c3797aa7170624b38ab7eb4b0bce0586c30a5ab811c5",
  );
  assertEquals(nsc.fileName, "nsc-2.15.0-darwin-arm64");
});

Deno.test("release urls are derived from the pinned version, not stored", () => {
  assertEquals(
    releaseUrl("nats-server", "2.99.0", "linux-amd64"),
    "https://github.com/nats-io/nats-server/releases/download/v2.99.0/nats-server-v2.99.0-linux-amd64.tar.gz",
  );
  assertEquals(
    releaseUrl("nsc", "2.99.0", "linux-arm64"),
    "https://github.com/nats-io/nsc/releases/download/v2.99.0/nsc-linux-arm64.zip",
  );
});

Deno.test("sha256 verification rejects mismatched bytes", async () => {
  const bytes = new TextEncoder().encode("trellis test binary");
  const digest = await sha256Hex(bytes);
  assertEquals(digest.length, 64);
  await verifySha256(bytes, digest);
  await assertRejects(
    () => verifySha256(bytes, "0".repeat(64)),
    Error,
    "SHA-256 mismatch",
  );
});

Deno.test("tar.gz extraction finds the wanted binary entry", () => {
  const binary = new TextEncoder().encode("nats-server-bytes");
  const archive = gzipSync(ustarArchive([
    {
      name: "nats-server-v2.14.4-linux-amd64/README.md",
      content: new TextEncoder().encode("readme"),
    },
    { name: "nats-server-v2.14.4-linux-amd64/nats-server", content: binary },
  ]));
  assertEquals(extractTarEntry(archive, "nats-server"), binary);
  assertThrows(
    () => extractTarEntry(archive, "missing"),
    Error,
    "not found in the tar.gz archive",
  );
});

Deno.test("zip extraction finds the wanted binary entry", () => {
  const binary = new TextEncoder().encode("nsc-bytes");
  const archive = zipSync({ "nsc": binary });
  assertEquals(extractZipEntry(archive, "nsc"), binary);
  const nested = zipSync({ "nsc-linux-amd64/nsc": binary });
  assertEquals(extractZipEntry(nested, "nsc"), binary);
  assertThrows(
    () => extractZipEntry(archive, "missing"),
    Error,
    "not found in the zip archive",
  );
});

function ustarHeader(name: string, sizeField: string): Uint8Array {
  const header = new Uint8Array(512);
  header.set(new TextEncoder().encode(name), 0);
  header.set(new TextEncoder().encode(sizeField), 124);
  header[156] = 48; // regular file typeflag '0'
  return header;
}

Deno.test("tar.gz extraction rejects negative entry sizes", () => {
  const archive = gzipSync(concat([
    ustarHeader("nats-server", "-0000000001\0"),
    new Uint8Array(1024),
  ]));
  assertThrows(
    () => extractTarEntry(archive, "nats-server"),
    Error,
    "invalid size",
  );
});

Deno.test("tar.gz extraction rejects entry sizes beyond the archive", () => {
  const archive = gzipSync(concat([
    ustarHeader("nats-server", "77777777777\0"), // 8 GiB claim
    new Uint8Array(100),
    new Uint8Array(1024),
  ]));
  assertThrows(
    () => extractTarEntry(archive, "nats-server"),
    Error,
    "exceeds the archive size",
  );
});

Deno.test("tar.gz extraction rejects truncated archives", () => {
  const archive = gzipSync(concat([
    ustarHeader("nats-server", "00000000400\0"), // claims 256 bytes
    new Uint8Array(100), // only 100 bytes present
  ]));
  assertThrows(
    () => extractTarEntry(archive, "nats-server"),
    Error,
    "exceeds the archive size",
  );
});

Deno.test("tar.gz extraction returns zero-length bodies", () => {
  const archive = gzipSync(concat([
    ustarHeader("nats-server", "00000000000\0"),
    new Uint8Array(1024),
  ]));
  assertEquals(extractTarEntry(archive, "nats-server"), new Uint8Array(0));
});

Deno.test("cache dir is created with owner-only permissions", async () => {
  const parent = await Deno.makeTempDir({ prefix: "trellis-cache-parent-" });
  try {
    const cacheDir = join(parent, "cache");
    await ensureCacheDir(cacheDir);
    const stat = await Deno.stat(cacheDir);
    assertEquals((stat.mode ?? 0) & 0o022, 0);
  } finally {
    await Deno.remove(parent, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("cache dir rejects group or world accessible directories", async () => {
  const cacheDir = await Deno.makeTempDir({ prefix: "trellis-cache-wide-" });
  try {
    // Any group/other permission bit is rejected, not only writable bits.
    await Deno.chmod(cacheDir, 0o777);
    await assertRejects(
      () => ensureCacheDir(cacheDir),
      Error,
      "group or world access",
    );
    await Deno.chmod(cacheDir, 0o755);
    await assertRejects(
      () => ensureCacheDir(cacheDir),
      Error,
      "group or world access",
    );
  } finally {
    await Deno.remove(cacheDir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("cache dir rejects symlinked paths", async () => {
  const parent = await Deno.makeTempDir({ prefix: "trellis-cache-symlink-" });
  try {
    const real = join(parent, "real");
    await Deno.mkdir(real);
    await Deno.chmod(real, 0o700);
    const link = join(parent, "cache-link");
    await Deno.symlink(real, link);
    await assertRejects(() => ensureCacheDir(link), Error, "symlink");
  } finally {
    await Deno.remove(parent, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("cache dir rejects non-directory cache paths", async () => {
  const parent = await Deno.makeTempDir({ prefix: "trellis-cache-file-" });
  try {
    const cacheFile = join(parent, "cache-file");
    await Deno.writeTextFile(cacheFile, "x");
    await assertRejects(
      () => ensureCacheDir(cacheFile),
      Error,
      "not a directory",
    );
  } finally {
    await Deno.remove(parent, { recursive: true }).catch(() => undefined);
  }
});

function withCacheDir(
  cacheDir: string,
  fn: () => Promise<void>,
): Promise<void> {
  const previous = Deno.env.get("TRELLIS_TEST_CACHE_DIR");
  Deno.env.set("TRELLIS_TEST_CACHE_DIR", cacheDir);
  return fn().finally(() => {
    if (previous === undefined) Deno.env.delete("TRELLIS_TEST_CACHE_DIR");
    else Deno.env.set("TRELLIS_TEST_CACHE_DIR", previous);
  });
}

Deno.test("cached binary fast path requires an executable regular file", async () => {
  const dir = await Deno.makeTempDir({ prefix: "trellis-cache-exec-" });
  try {
    const binary = join(dir, "binary");
    // A non-executable regular file is invalid: the fast path must refuse it
    // and reinstall through the download path.
    await Deno.writeTextFile(binary, "stale");
    await Deno.chmod(binary, 0o644);
    assertEquals(await isSafeCachedBinary(binary), false);
    await Deno.chmod(binary, 0o755);
    assertEquals(await isSafeCachedBinary(binary), true);

    const link = join(dir, "link");
    await Deno.symlink(binary, link);
    assertEquals(await isSafeCachedBinary(link), false);

    const directory = join(dir, "directory");
    await Deno.mkdir(directory);
    await Deno.chmod(directory, 0o755);
    assertEquals(await isSafeCachedBinary(directory), false);

    assertEquals(await isSafeCachedBinary(join(dir, "missing")), false);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("cached archives and binaries are reused without downloading", async () => {
  const cacheDir = await Deno.makeTempDir({ prefix: "trellis-cache-hit-" });
  try {
    await withCacheDir(cacheDir, async () => {
      // A fixture manifest whose URL would fail if any download were attempted, so
      // a successful reuse proves no re-download happened.
      const manifest = await fixtureManifest();
      const platform = natsBinaryPlatform(Deno.build.os, Deno.build.arch);
      for (const name of ["nats-server", "nsc"] as const) {
        const binary = new TextEncoder().encode(`binary-${name}`);
        await Deno.writeFile(
          join(
            cacheDir,
            `${name}-${manifest[name].version}-${platform}.archive`,
          ),
          archiveFor(name, binary),
        );
        const binaryPath = join(
          cacheDir,
          `${name}-${manifest[name].version}-${platform}`,
        );
        await Deno.writeFile(binaryPath, binary);
        await Deno.chmod(binaryPath, 0o755);
      }
      const binaries = await ensureNatsBinaries(manifest);
      assertEquals(
        binaries.natsServer,
        join(
          cacheDir,
          `nats-server-${manifest["nats-server"].version}-${platform}`,
        ),
      );
      assertEquals(
        binaries.nsc,
        join(cacheDir, `nsc-${manifest.nsc.version}-${platform}`),
      );
    });
  } finally {
    await Deno.remove(cacheDir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("a replaced cached binary is reinstalled from the verified archive", async () => {
  const cacheDir = await Deno.makeTempDir({ prefix: "trellis-cache-replace-" });
  try {
    await withCacheDir(cacheDir, async () => {
      const manifest = await fixtureManifest();
      const platform = natsBinaryPlatform(Deno.build.os, Deno.build.arch);
      // Seed valid archives and binaries for both tools so only the nats-server
      // binary replacement is exercised.
      for (const name of ["nats-server", "nsc"] as const) {
        const binary = new TextEncoder().encode(`binary-${name}`);
        await Deno.writeFile(
          join(
            cacheDir,
            `${name}-${manifest[name].version}-${platform}.archive`,
          ),
          archiveFor(name, binary),
        );
        const binaryPath = join(
          cacheDir,
          `${name}-${manifest[name].version}-${platform}`,
        );
        await Deno.writeFile(binaryPath, binary);
        await Deno.chmod(binaryPath, 0o755);
      }
      // A regular executable file whose bytes do not match the verified archive.
      const binaryPath = join(
        cacheDir,
        `nats-server-${manifest["nats-server"].version}-${platform}`,
      );
      await Deno.writeFile(binaryPath, new TextEncoder().encode("tampered"));

      const binaries = await ensureNatsBinaries(manifest);
      assertEquals(
        await Deno.readFile(binaries.natsServer),
        new TextEncoder().encode("binary-nats-server"),
        "the replaced binary must be replaced by a fresh extraction of the archive",
      );
    });
  } finally {
    await Deno.remove(cacheDir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("binary alias links are corrected or accepted by target", async () => {
  const cacheDir = await Deno.makeTempDir({ prefix: "trellis-cache-alias-" });
  try {
    await withCacheDir(cacheDir, async () => {
      const manifest = await fixtureManifest();
      const platform = natsBinaryPlatform(Deno.build.os, Deno.build.arch);
      for (const name of ["nats-server", "nsc"] as const) {
        const binary = new TextEncoder().encode(`binary-${name}`);
        await Deno.writeFile(
          join(
            cacheDir,
            `${name}-${manifest[name].version}-${platform}.archive`,
          ),
          archiveFor(name, binary),
        );
        const binaryPath = join(
          cacheDir,
          `${name}-${manifest[name].version}-${platform}`,
        );
        await Deno.writeFile(binaryPath, binary);
        await Deno.chmod(binaryPath, 0o755);
      }
      const natsAlias = join(cacheDir, "nats-server");
      const natsTarget = `nats-server-${
        manifest["nats-server"].version
      }-${platform}`;
      // A stale alias pointing at the wrong versioned binary is corrected.
      await Deno.symlink("nats-server-9.9.9-linux-amd64", natsAlias);
      await ensureNatsBinaries(manifest);
      assertEquals(await Deno.readLink(natsAlias), natsTarget);
      // A correct pre-existing alias (as a concurrent process would leave it)
      // is accepted without being recreated.
      const nscAlias = join(cacheDir, "nsc");
      const nscTarget = `nsc-${manifest.nsc.version}-${platform}`;
      await Deno.remove(nscAlias).catch(() => undefined);
      await Deno.symlink(nscTarget, nscAlias);
      const binaries = await ensureNatsBinaries(manifest);
      assertEquals(await Deno.readLink(nscAlias), nscTarget);
      assertEquals(binaries.nsc, join(cacheDir, nscTarget));
    });
  } finally {
    await Deno.remove(cacheDir, { recursive: true }).catch(() => undefined);
  }
});

Deno.test("a corrupt cached archive is re-downloaded, not trusted", async () => {
  const cacheDir = await Deno.makeTempDir({ prefix: "trellis-cache-corrupt-" });
  try {
    await withCacheDir(cacheDir, async () => {
      const manifest = await fixtureManifest();
      const platform = natsBinaryPlatform(Deno.build.os, Deno.build.arch);
      const archivePath = join(
        cacheDir,
        `nats-server-${manifest["nats-server"].version}-${platform}.archive`,
      );
      await Deno.writeFile(archivePath, new TextEncoder().encode("tampered"));

      // The fixture version's URL does not exist: the download attempt fails, which
      // proves the corrupt archive was re-downloaded rather than trusted.
      await assertRejects(
        () => ensureNatsBinaries(manifest),
        Error,
        "failed to download",
      );
    });
  } finally {
    await Deno.remove(cacheDir, { recursive: true }).catch(() => undefined);
  }
});

/**
 * A fixture manifest whose version has no real release URL, so any accidental
 * download fails fast and proves cache reuse decisions without network access.
 * The sha256 entries are the real digests of the fixture archives.
 */
async function fixtureManifest(): Promise<NatsBinariesManifest> {
  const version = "0.0.0-fixture";
  const digest = (name: "nats-server" | "nsc") =>
    sha256Hex(archiveFor(name, new TextEncoder().encode(`binary-${name}`)));
  const platformKeys: NatsBinaryPlatform[] = [
    "linux-amd64",
    "linux-arm64",
    "darwin-amd64",
    "darwin-arm64",
  ];
  const sha256For = async (name: "nats-server" | "nsc") => {
    const digest_ = await digest(name);
    return Object.fromEntries(
      platformKeys.map((platform) => [platform, digest_]),
    ) as Record<NatsBinaryPlatform, string>;
  };
  return {
    "nats-server": { version, sha256: await sha256For("nats-server") },
    nsc: { version, sha256: await sha256For("nsc") },
  };
}

function archiveFor(
  name: "nats-server" | "nsc",
  binary: Uint8Array,
): Uint8Array {
  if (name === "nats-server") {
    return gzipSync(ustarArchive([
      { name: `nats-server-v0.0.0-fixture/nats-server`, content: binary },
    ]));
  }
  return zipSync({ "nsc": binary }, { mtime: new Date(2000, 0, 1) });
}
