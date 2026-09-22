/**
 * Downloads and caches the pinned local nats-server and nsc binaries used by
 * the Trellis test harness.
 *
 * First use requires network access to the pinned GitHub release assets derived
 * from `rust/crates/local-nats/nats-binaries.json` (version + per-platform
 * sha256 only; the release URLs are derived from the version). Verified
 * archives and binaries are cached under
 * `TRELLIS_TEST_CACHE_DIR` (default `~/.cache/trellis-test`) and reused by
 * subsequent runs: the cached archive is re-hashed against the pin and the
 * cached binary is byte-compared against a fresh extraction of that verified
 * archive, so a corrupted or replaced cache entry is reinstalled from the pin
 * without trusting it. Windows is not supported yet.
 */
import { dirname, join } from "@std/path";
import { gunzipSync, unzipSync } from "fflate";
import { ulid } from "ulid";
import natsBinaries from "./nats-binaries.json" with {
  type: "json",
};

const CACHE_DIR_ENV = "TRELLIS_TEST_CACHE_DIR";
const BINARY_NAMES = ["nats-server", "nsc"] as const;
const DOWNLOAD_TIMEOUT_MS = 60_000;

/** Pinned NATS binaries managed by the Trellis test harness. */
export type NatsBinaryName = (typeof BINARY_NAMES)[number];

/** Asset platform names used by the pinned binary release files. */
export type NatsBinaryPlatform =
  | "linux-amd64"
  | "linux-arm64"
  | "darwin-amd64"
  | "darwin-arm64";

/** The pinned binaries manifest shape (version + per-platform sha256). */
export type NatsBinariesManifest = Record<
  NatsBinaryName,
  {
    readonly version: string;
    readonly sha256: Record<NatsBinaryPlatform, string>;
  }
>;

/** Maps Deno build os/arch values to the pinned asset platform names. */
export function natsBinaryPlatform(
  os: string,
  arch: string,
): NatsBinaryPlatform {
  const osName = os === "linux"
    ? "linux"
    : os === "darwin"
    ? "darwin"
    : undefined;
  if (osName === undefined) {
    throw new Error(
      `Trellis test NATS binaries are not supported on ${os} (only linux and darwin)`,
    );
  }
  const archName = arch === "x86_64"
    ? "amd64"
    : arch === "aarch64" || arch === "arm64"
    ? "arm64"
    : undefined;
  if (archName === undefined) {
    throw new Error(
      `Trellis test NATS binaries are not supported on ${os}-${arch} (only amd64 and arm64)`,
    );
  }
  return `${osName}-${archName}`;
}

/**
 * Official GitHub release URL for the pinned archive of `name` at `version`,
 * derived from the version (no URL is stored in the pin file).
 */
export function releaseUrl(
  name: NatsBinaryName,
  version: string,
  platform: NatsBinaryPlatform,
): string {
  const [osName, archName] = platform.split("-");
  if (name === "nats-server") {
    return (
      `https://github.com/nats-io/nats-server/releases/download/v${version}` +
      `/nats-server-v${version}-${osName}-${archName}.tar.gz`
    );
  }
  return `https://github.com/nats-io/nsc/releases/download/v${version}/nsc-${osName}-${archName}.zip`;
}

/** Resolves the pinned download parameters for one binary on one platform. */
export function resolveNatsBinaryAsset(
  name: NatsBinaryName,
  os: string,
  arch: string,
): {
  readonly platform: NatsBinaryPlatform;
  readonly url: string;
  readonly sha256: string;
  readonly fileName: string;
} {
  return resolveNatsBinaryAssetFrom(natsBinaries, name, os, arch);
}

/** `resolveNatsBinaryAsset` against an explicit manifest, for tests. */
function resolveNatsBinaryAssetFrom(
  manifest: NatsBinariesManifest,
  name: NatsBinaryName,
  os: string,
  arch: string,
): {
  readonly platform: NatsBinaryPlatform;
  readonly url: string;
  readonly sha256: string;
  readonly fileName: string;
} {
  const platform = natsBinaryPlatform(os, arch);
  const spec = manifest[name];
  return {
    platform,
    url: releaseUrl(name, spec.version, platform),
    sha256: spec.sha256[platform],
    fileName: `${name}-${spec.version}-${platform}`,
  };
}

function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  const buf = data.buffer;
  if (buf instanceof ArrayBuffer) {
    return buf.slice(data.byteOffset, data.byteOffset + data.byteLength);
  }
  const copy = new Uint8Array(data.byteLength);
  copy.set(data);
  return copy.buffer;
}

/** Returns the lowercase hex SHA-256 digest of `bytes`. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", toArrayBuffer(bytes));
  return Array.from(
    new Uint8Array(digest),
    (byte) => byte.toString(16).padStart(2, "0"),
  ).join("");
}

/** Throws unless `bytes` match the expected lowercase hex SHA-256 digest. */
export async function verifySha256(
  bytes: Uint8Array,
  expectedHex: string,
): Promise<void> {
  const actual = await sha256Hex(bytes);
  if (actual !== expectedHex.toLowerCase()) {
    throw new Error(
      `SHA-256 mismatch for Trellis test NATS binary: expected ${expectedHex}, got ${actual}`,
    );
  }
}

/** Path to the shared Trellis test binary cache directory. */
export function trellisTestCacheDir(): string {
  return Deno.env.get(CACHE_DIR_ENV) ??
    join(
      Deno.env.get("HOME") ?? Deno.env.get("TMPDIR") ?? "/tmp",
      ".cache",
      "trellis-test",
    );
}

/** @internal Extracts one regular file entry from a gzip-compressed ustar tar archive. */
export function extractTarEntry(
  bytes: Uint8Array,
  wantedName: string,
): Uint8Array {
  const tar = gunzipSync(bytes);
  const decoder = new TextDecoder();
  let offset = 0;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    const name = decoder.decode(header.subarray(0, 100)).split("\0", 1)[0];
    if (name === "") break; // end-of-archive marker
    const size = Number.parseInt(
      decoder.decode(header.subarray(124, 136)).split("\0", 1)[0].trim(),
      8,
    );
    if (!Number.isSafeInteger(size) || size < 0) {
      throw new Error(
        `corrupt tar.gz archive: invalid size for entry ${name}`,
      );
    }
    const dataStart = offset + 512;
    const dataEnd = dataStart + size;
    if (dataEnd > tar.length) {
      throw new Error(
        `corrupt tar.gz archive: entry ${name} exceeds the archive size`,
      );
    }
    const typeflag = String.fromCharCode(header[156]);
    if (
      (typeflag === "0" || typeflag === "\0") &&
      (name === wantedName || name.endsWith(`/${wantedName}`))
    ) {
      return tar.slice(dataStart, dataEnd);
    }
    const next = dataStart + Math.ceil(size / 512) * 512;
    if (next <= offset) {
      throw new Error(
        `corrupt tar.gz archive: entry ${name} does not advance the offset`,
      );
    }
    offset = next;
  }
  throw new Error(`binary ${wantedName} not found in the tar.gz archive`);
}

/** @internal Extracts one file entry from a zip archive. */
export function extractZipEntry(
  bytes: Uint8Array,
  wantedName: string,
): Uint8Array {
  const files = unzipSync(bytes);
  const entry = Object.entries(files).find(([name]) =>
    name === wantedName || name.endsWith(`/${wantedName}`)
  );
  if (entry === undefined) {
    throw new Error(`binary ${wantedName} not found in the zip archive`);
  }
  return entry[1];
}

/** @internal Creates the cache directory with owner-only permissions, refusing unsafe existing dirs. */
export async function ensureCacheDir(cacheDir: string): Promise<void> {
  let stat: Deno.FileInfo | undefined;
  try {
    stat = await Deno.lstat(cacheDir);
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  }
  if (stat === undefined) {
    await Deno.mkdir(cacheDir, { recursive: true, mode: 0o700 });
    stat = await Deno.lstat(cacheDir);
  }
  if (stat.isSymlink) {
    throw new Error(
      `Trellis test cache path ${cacheDir} is a symlink; refusing to use it`,
    );
  }
  if (!stat.isDirectory) {
    throw new Error(`Trellis test cache path ${cacheDir} is not a directory`);
  }
  const mode = stat.mode ?? 0;
  if ((mode & 0o077) !== 0) {
    throw new Error(
      `Trellis test cache directory ${cacheDir} allows group or world access (mode ${
        mode.toString(8)
      }); refusing to use it`,
    );
  }
  const uid = stat.uid;
  if (uid !== null && Deno.uid() !== null && uid !== Deno.uid()) {
    throw new Error(
      `Trellis test cache directory ${cacheDir} is owned by uid ${uid}; refusing to use it`,
    );
  }
}

/** @internal True when the cached binary is a regular executable file safe to reuse. */
export async function isSafeCachedBinary(path: string): Promise<boolean> {
  let stat: Deno.FileInfo;
  try {
    stat = await Deno.lstat(path);
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return false;
    throw error;
  }
  // A symlink, directory, or non-executable file is an invalid cache entry:
  // treat it as missing so the download path reinstalls the verified binary.
  if (stat.isSymlink || !stat.isFile) return false;
  const mode = stat.mode ?? 0;
  return (mode & 0o111) !== 0;
}

async function ensureBinaryLink(
  cacheDir: string,
  name: string,
  target: string,
): Promise<void> {
  const linkPath = join(cacheDir, name);
  const current = await Deno.readLink(linkPath).catch(() => undefined);
  if (current === target) return;
  try {
    // Replace a stale or wrong alias; a concurrent first-use process may create
    // the same alias between our read and our write.
    await Deno.remove(linkPath).catch(() => undefined);
    await Deno.symlink(target, linkPath);
  } catch (error) {
    if (error instanceof Deno.errors.AlreadyExists) {
      // Another process created the alias first: accept it when it already
      // resolves to the expected versioned binary, fail otherwise.
      const existing = await Deno.readLink(linkPath).catch(() => undefined);
      if (existing === target) return;
    }
    throw error;
  }
}

async function ensureBinary(
  name: NatsBinaryName,
  cacheDir: string,
  manifest: NatsBinariesManifest,
): Promise<string> {
  const asset = resolveNatsBinaryAssetFrom(
    manifest,
    name,
    Deno.build.os,
    Deno.build.arch,
  );
  const finalPath = join(cacheDir, asset.fileName);
  const archivePath = join(cacheDir, `${asset.fileName}.archive`);

  // The cached archive is the root of trust for reuse: it must still match the pin.
  let archive: Uint8Array | undefined;
  try {
    const cached = await Deno.readFile(archivePath);
    if ((await sha256Hex(cached)) === asset.sha256) {
      archive = cached;
    } else {
      await Deno.remove(archivePath).catch(() => undefined);
    }
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  }
  if (archive === undefined) {
    archive = await downloadArchive(asset, archivePath);
  }

  // Reuse the cached binary only when it is byte-identical to a fresh extraction
  // of the verified archive; a corrupted or replaced binary is reinstalled.
  const freshBinary = name === "nats-server"
    ? extractTarEntry(archive, "nats-server")
    : extractZipEntry(archive, "nsc");
  if (await isSafeCachedBinary(finalPath)) {
    const current = await Deno.readFile(finalPath);
    if (bytesEqual(current, freshBinary)) {
      await ensureBinaryLink(cacheDir, name, asset.fileName);
      return finalPath;
    }
  }
  const tempPath = join(
    cacheDir,
    `${asset.fileName}.${ulid()}.tmp`,
  );
  await Deno.writeFile(tempPath, freshBinary);
  try {
    await Deno.chmod(tempPath, 0o755);
    await Deno.rename(tempPath, finalPath);
    // `nsc` is spawned by the bootstrap script through PATH lookup.
    await ensureBinaryLink(cacheDir, name, asset.fileName);
  } catch (error) {
    await Deno.remove(tempPath).catch(() => undefined);
    throw error;
  }
  return finalPath;
}

/** Downloads, verifies, and caches the pinned archive for `asset`. */
async function downloadArchive(
  asset: ReturnType<typeof resolveNatsBinaryAsset>,
  archivePath: string,
): Promise<Uint8Array> {
  const response = await fetch(asset.url, {
    signal: AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS),
  });
  if (!response.ok) {
    throw new Error(
      `failed to download ${asset.fileName} from ${asset.url}: ${response.status} ${response.statusText}`,
    );
  }
  const archive = new Uint8Array(await response.arrayBuffer());
  await verifySha256(archive, asset.sha256);
  const tempPath = join(
    dirname(archivePath),
    `${asset.fileName}.archive.${ulid()}.tmp`,
  );
  await Deno.writeFile(tempPath, archive);
  try {
    await Deno.rename(tempPath, archivePath);
  } catch (error) {
    await Deno.remove(tempPath).catch(() => undefined);
    throw error;
  }
  return archive;
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/** Ensures the pinned nats-server and nsc binaries are cached and executable. */
export async function ensureNatsBinaries(
  manifest: NatsBinariesManifest = natsBinaries,
): Promise<{
  natsServer: string;
  nsc: string;
}> {
  const cacheDir = trellisTestCacheDir();
  await ensureCacheDir(cacheDir);
  const [natsServer, nsc] = await Promise.all([
    ensureBinary("nats-server", cacheDir, manifest),
    ensureBinary("nsc", cacheDir, manifest),
  ]);
  return { natsServer, nsc };
}
