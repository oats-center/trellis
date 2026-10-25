import { execFile } from "node:child_process";
import * as fs from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { Buffer } from "node:buffer";
import { Parser, type ReadEntry } from "tar";
import { z } from "zod";
import { ulid } from "ulid";
import type { TrellisNativeSource } from "./types.ts";

const execFileAsync = promisify(execFile);

const releases = "https://github.com/oats-center/trellis/releases/download";
/** Unauthenticated GitHub API requests are rate limited per address. */
const latestReleaseTtlMs = 3_600_000;

const githubReleases = z.array(
  z.object({
    tag_name: z.string(),
    draft: z.boolean(),
    prerelease: z.boolean(),
  }),
);
const latestRecord = z.object({ version: z.string(), checkedMs: z.number() });

/** Native production commands resolved by the testkit. */
export type TrellisNativeDistribution = { cli: string; server: string };

/** Maps supported host platforms to targets published by the native Check jobs. */
export function nativeTarget(os: string, arch: string): string {
  const cpu =
    arch === "x64" ? "x86_64" : arch === "arm64" ? "aarch64" : undefined;
  const platform =
    os === "linux"
      ? "unknown-linux-gnu"
      : os === "darwin"
        ? "apple-darwin"
        : undefined;
  if (!cpu || !platform) {
    throw new Error(
      `Trellis native distributions do not support ${os}-${arch}`,
    );
  }
  return `${cpu}-${platform}`;
}

/** Shared cache for verified immutable native distributions and managed NATS. */
export function trellisTestCacheDir(): string {
  return (
    process.env.TRELLIS_TEST_CACHE_DIR ??
    join(
      process.env.HOME ?? process.env.TMPDIR ?? process.cwd(),
      ".cache",
      "trellis-testkit",
    )
  );
}

/** Verify archive integrity against the checksum published with its release. */
export async function verifyNativeArchive(
  bytes: Uint8Array,
  expected: string,
): Promise<void> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new Uint8Array(bytes).buffer,
  );
  const actual = Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
  if (actual !== expected) {
    throw new Error("Trellis native archive SHA-256 mismatch");
  }
}

/** Decode an archive's one regular root-level executable, without extracting any path to disk. */
export function extractNativeArchive(
  bytes: Uint8Array,
  name: string,
): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    let file: Uint8Array | undefined;
    const parser = new Parser({ strict: true });
    parser.on("error", reject);
    parser.on("meta", () =>
      parser.abort(new Error("Unexpected native archive metadata")),
    );
    parser.on("ignoredEntry", () =>
      parser.abort(new Error("Unexpected native archive entry")),
    );
    parser.on("entry", (entry: ReadEntry) => {
      if (
        entry.type !== "File" ||
        entry.path !== name ||
        file !== undefined ||
        entry.size <= 0 ||
        entry.size > 1_073_741_824
      ) {
        parser.abort(
          new Error(
            `Native archive must contain only a regular ${name} executable`,
          ),
        );
        entry.resume();
        return;
      }
      const chunks: Uint8Array[] = [];
      file = new Uint8Array();
      entry.on("data", (chunk: Uint8Array) => chunks.push(chunk));
      entry.on("end", () => (file = Buffer.concat(chunks)));
      entry.resume();
    });
    parser.on("end", () => {
      if (!file?.length) reject(new Error(`Native archive is missing ${name}`));
      else resolve(file);
    });
    parser.end(Buffer.from(bytes));
  });
}

let announced = false;

async function verifyDistribution(
  paths: TrellisNativeDistribution,
  expected?: string,
): Promise<TrellisNativeDistribution> {
  const canonical = { cli: "", server: "" };
  for (const name of ["cli", "server"] as const) {
    const info = await fs.lstat(paths[name]);
    if (!info.isFile() || info.isSymbolicLink() || (info.mode & 0o111) === 0)
      throw new Error(`Trellis ${name} must be a regular executable file`);
    canonical[name] = await fs.realpath(paths[name]);
    const output = await execFileAsync(
      canonical[name],
      name === "cli" ? ["--format", "json", "version"] : ["--version"],
      { encoding: "utf8" },
    ).catch(() => {
      throw new Error(`Cannot verify Trellis ${name} version`);
    });
    const text = output.stdout.trim();
    // Build provenance is not part of SemVer identity; checkout builds add
    // +local.<commit>.dirty.
    const version = (
      name === "cli"
        ? z.object({ version: z.string() }).parse(JSON.parse(text)).version
        : text.replace(/^trellis-server /, "")
    ).split("+", 1)[0];
    if (expected && version !== expected) {
      throw new Error(
        `Trellis ${name} reports ${version}, not release ${expected}`,
      );
    }
    if (name === "server" && !announced) {
      announced = true;
      console.error(
        `trellis-testkit: testing against trellis-server ${version}`,
      );
    }
  }
  return canonical;
}

/**
 * The release to test against: `TRELLIS_TEST_VERSION`, or the latest stable
 * release, falling back to the newest prerelease before the first stable one.
 * A recent answer is reused, and an unreachable GitHub falls back to the last.
 */
async function releaseVersion(cache: string): Promise<string> {
  const pinned = process.env.TRELLIS_TEST_VERSION;
  if (pinned) return pinned.replace(/^v/, "");
  const record = join(cache, "latest-release.json");
  const cached = await fs
    .readFile(record, "utf8")
    .then((text) => latestRecord.parse(JSON.parse(text)))
    .catch(() => undefined);
  if (cached && Date.now() - cached.checkedMs < latestReleaseTtlMs) {
    return cached.version;
  }
  try {
    const token = process.env.GITHUB_TOKEN;
    const response = await fetch(
      "https://api.github.com/repos/oats-center/trellis/releases?per_page=50",
      {
        headers: token ? { authorization: `Bearer ${token}` } : {},
        signal: AbortSignal.timeout(60_000),
      },
    );
    if (!response.ok) {
      throw new Error(`GitHub releases lookup failed (${response.status})`);
    }
    // GitHub lists releases newest first.
    const published = githubReleases
      .parse(await response.json())
      .filter((release) => !release.draft);
    const release =
      published.find((candidate) => !candidate.prerelease) ?? published[0];
    if (!release)
      throw new Error("oats-center/trellis has no published release");
    const version = release.tag_name.replace(/^v/, "");
    await fs.writeFile(
      record,
      JSON.stringify({ version, checkedMs: Date.now() }),
    );
    return version;
  } catch (error) {
    if (cached) return cached.version;
    throw error;
  }
}

/**
 * Resolve production binaries: the requested source, else both
 * `TRELLIS_TEST_CLI_BIN` and `TRELLIS_TEST_SERVER_BIN`, else a release download.
 * Explicit paths never fall back to downloads.
 */
export async function resolveNativeDistribution(
  requested?: TrellisNativeSource,
): Promise<TrellisNativeDistribution> {
  const cli = process.env.TRELLIS_TEST_CLI_BIN;
  const server = process.env.TRELLIS_TEST_SERVER_BIN;
  if (!requested && Boolean(cli) !== Boolean(server)) {
    throw new Error(
      "Set both TRELLIS_TEST_CLI_BIN and TRELLIS_TEST_SERVER_BIN",
    );
  }
  const source: TrellisNativeSource =
    requested ??
    (cli && server ? { kind: "path", cli, server } : { kind: "release" });
  if (source.kind === "path") return await verifyDistribution(source);
  const target = nativeTarget(process.platform, process.arch);
  const cache = trellisTestCacheDir();
  await fs.mkdir(cache, { recursive: true, mode: 0o700 });
  const version = await releaseVersion(cache);
  const parent = join(cache, "trellis", version, target);
  await fs.mkdir(parent, { recursive: true, mode: 0o700 });
  return await verifyDistribution(
    {
      cli: await installExecutable(parent, version, target, "trellis"),
      server: await installExecutable(
        parent,
        version,
        target,
        "trellis-server",
      ),
    },
    version,
  );
}

/** Install one release executable, checked against its published checksum. */
async function installExecutable(
  parent: string,
  version: string,
  target: string,
  name: string,
): Promise<string> {
  // Release assets are immutable, so a fetched checksum is kept.
  const recorded = join(parent, `${name}.sha256`);
  let sha256 = await fs.readFile(recorded, "utf8").catch(() => undefined);
  if (!sha256) {
    const response = await fetch(
      `${releases}/v${version}/checksum-${version}-${target}-${name}.sha256`,
      { signal: AbortSignal.timeout(60_000) },
    );
    if (!response.ok) {
      throw new Error(
        `Trellis ${version} has no ${target} ${name} checksum (${response.status})`,
      );
    }
    sha256 = (await response.text()).trim().split(/\s+/)[0] ?? "";
    await fs.writeFile(recorded, sha256);
  }
  if (!/^[a-f0-9]{64}$/.test(sha256)) {
    throw new Error(
      `Invalid published ${name} checksum for Trellis ${version}`,
    );
  }
  const installed = join(parent, sha256);
  const path = join(installed, name);
  const matches = async (expected: Uint8Array) => {
    const info = await fs.lstat(path);
    if (!info.isFile() || info.isSymbolicLink()) return false;
    const file = await fs.readFile(path);
    return (
      file.length === expected.length &&
      file.every((byte, index) => byte === expected[index])
    );
  };
  try {
    const info = await fs.lstat(installed);
    if (!info.isDirectory() || info.isSymbolicLink()) {
      throw new Error("Unsafe native cache entry");
    }
    const archive = await fs.readFile(join(installed, "archive.tar.gz"));
    await verifyNativeArchive(archive, sha256);
    if (await matches(await extractNativeArchive(archive, name))) return path;
  } catch {
    // Missing or damaged: download again below.
  }
  const response = await fetch(
    `${releases}/v${version}/${name}-${version}-${target}.tar.gz`,
    { signal: AbortSignal.timeout(60_000) },
  );
  if (!response.ok) {
    throw new Error(
      `Trellis ${name} archive download failed (${response.status})`,
    );
  }
  const archive = new Uint8Array(await response.arrayBuffer());
  await verifyNativeArchive(archive, sha256);
  const executable = await extractNativeArchive(archive, name);
  const staging = await fs.mkdtemp(join(parent, ".install-"));
  let quarantine: string | undefined;
  try {
    await fs.writeFile(join(staging, name), executable, { mode: 0o700 });
    await fs.writeFile(join(staging, "archive.tar.gz"), archive, {
      mode: 0o600,
    });
    try {
      await fs.rename(staging, installed);
    } catch (error) {
      const installedInfo = await fs.lstat(installed).catch(() => undefined);
      if (!installedInfo) throw error;
      // A concurrent installer may have completed while we downloaded. Keep a
      // valid winner, which may be in use; replace only a damaged one.
      const valid = await matches(executable).catch(() => false);
      if (!valid) {
        quarantine = join(parent, `.corrupt-${ulid()}`);
        await fs.rename(installed, quarantine);
        await fs.rename(staging, installed);
      }
    }
    return path;
  } finally {
    await fs.rm(staging, { recursive: true }).catch(() => undefined);
    if (quarantine) await fs.rm(quarantine, { recursive: true });
  }
}
