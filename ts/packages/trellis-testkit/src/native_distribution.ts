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

/** Decode only the two regular root-level executables, without extracting any path to disk. */
export function extractNativeArchive(
  bytes: Uint8Array,
): Promise<TrellisNativeDistributionBytes> {
  return new Promise((resolve, reject) => {
    const files = new Map<string, Uint8Array>();
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
        !["trellis", "trellis-server"].includes(entry.path) ||
        files.has(entry.path) ||
        entry.size <= 0 ||
        entry.size > 1_073_741_824
      ) {
        parser.abort(
          new Error(
            "Native archive must contain only regular trellis and trellis-server executables",
          ),
        );
        entry.resume();
        return;
      }
      const chunks: Uint8Array[] = [];
      files.set(entry.path, new Uint8Array());
      entry.on("data", (chunk: Uint8Array) => chunks.push(chunk));
      entry.on("end", () => files.set(entry.path, Buffer.concat(chunks)));
      entry.resume();
    });
    parser.on("end", () => {
      const cli = files.get("trellis");
      const server = files.get("trellis-server");
      if (!cli?.length || !server?.length) {
        reject(new Error("Native archive is missing production executables"));
      } else resolve({ cli, server });
    });
    parser.end(Buffer.from(bytes));
  });
}

type TrellisNativeDistributionBytes = { cli: Uint8Array; server: Uint8Array };

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
  // Release assets are immutable, so a fetched checksum is kept.
  const recorded = join(parent, "archive.sha256");
  let sha256 = await fs.readFile(recorded, "utf8").catch(() => undefined);
  if (!sha256) {
    const response = await fetch(
      `${releases}/v${version}/checksum-${version}-${target}-trellis.sha256`,
      { signal: AbortSignal.timeout(60_000) },
    );
    if (!response.ok) {
      throw new Error(
        `Trellis ${version} has no ${target} checksum (${response.status})`,
      );
    }
    sha256 = (await response.text()).trim().split(/\s+/)[0] ?? "";
    await fs.writeFile(recorded, sha256);
  }
  if (!/^[a-f0-9]{64}$/.test(sha256)) {
    throw new Error(`Invalid published checksum for Trellis ${version}`);
  }
  const asset = { archive: `trellis-${version}-${target}.tar.gz`, sha256 };
  const installed = join(parent, asset.sha256);
  const paths = {
    cli: join(installed, "trellis"),
    server: join(installed, "trellis-server"),
  };
  let archive: Uint8Array | undefined;
  try {
    const info = await fs.lstat(installed);
    if (!info.isDirectory() || info.isSymbolicLink()) {
      throw new Error("Unsafe native cache entry");
    }
    archive = await fs.readFile(join(installed, "archive.tar.gz"));
    await verifyNativeArchive(archive, asset.sha256);
    const extracted = await extractNativeArchive(archive);
    for (const name of ["cli", "server"] as const) {
      const info = await fs.lstat(paths[name]);
      if (!info.isFile() || info.isSymbolicLink()) {
        throw new Error("Unsafe cached native executable");
      }
      const cached = await fs.readFile(paths[name]);
      if (
        cached.length !== extracted[name].length ||
        cached.some((byte, index) => byte !== extracted[name][index])
      ) {
        throw new Error(
          "Cached native executable differs from the verified archive",
        );
      }
    }
    return await verifyDistribution(paths, version);
  } catch {
    archive = undefined;
  }
  const response = await fetch(`${releases}/v${version}/${asset.archive}`, {
    signal: AbortSignal.timeout(60_000),
  });
  if (!response.ok) {
    throw new Error(
      `Trellis native archive download failed (${response.status})`,
    );
  }
  archive = new Uint8Array(await response.arrayBuffer());
  await verifyNativeArchive(archive, asset.sha256);
  const extracted = await extractNativeArchive(archive);
  const staging = await fs.mkdtemp(join(parent, ".install-"));
  let quarantine: string | undefined;
  try {
    await fs.writeFile(join(staging, "trellis"), extracted.cli, {
      mode: 0o700,
    });
    await fs.writeFile(join(staging, "trellis-server"), extracted.server, {
      mode: 0o700,
    });
    await fs.writeFile(join(staging, "archive.tar.gz"), archive, {
      mode: 0o600,
    });
    await verifyDistribution(
      {
        cli: join(staging, "trellis"),
        server: join(staging, "trellis-server"),
      },
      version,
    );
    try {
      await fs.rename(staging, installed);
    } catch (error) {
      const installedInfo = await fs.lstat(installed).catch(() => undefined);
      if (!installedInfo) throw error;
      // A concurrent installer may have completed while we downloaded. Compare
      // the winner with the pin, never overwrite a valid executable in use.
      const existing =
        installedInfo.isDirectory() && !installedInfo.isSymbolicLink()
          ? await fs
              .readFile(join(installed, "archive.tar.gz"))
              .catch(() => new Uint8Array())
          : new Uint8Array();
      let valid = false;
      try {
        await verifyNativeArchive(existing, asset.sha256);
        valid = true;
        for (const name of ["cli", "server"] as const) {
          const info = await fs.lstat(paths[name]);
          const file =
            info.isFile() && !info.isSymbolicLink()
              ? await fs.readFile(paths[name])
              : new Uint8Array();
          if (
            file.length !== extracted[name].length ||
            file.some((byte, index) => byte !== extracted[name][index])
          )
            valid = false;
        }
      } catch {
        valid = false;
      }
      if (!valid) {
        quarantine = join(parent, `.corrupt-${ulid()}`);
        await fs.rename(installed, quarantine);
        await fs.rename(staging, installed);
      }
    }
    return await verifyDistribution(paths, version);
  } finally {
    await fs.rm(staging, { recursive: true }).catch(() => undefined);
    if (quarantine) await fs.rm(quarantine, { recursive: true });
  }
}
