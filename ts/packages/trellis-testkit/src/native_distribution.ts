import { join } from "@std/path";
import { Buffer } from "node:buffer";
import { Parser, type ReadEntry } from "tar";
import { z } from "zod";
import packageConfig from "../deno.json" with { type: "json" };
import nativeManifest from "./native/trellis-binaries.json" with {
  type: "json",
};
import type { TrellisNativeSource } from "./types.ts";

const manifestSchema = z.object({
  version: z.string(),
  targets: z.record(
    z.string(),
    z.object({
      archive: z.string(),
      sha256: z.string().regex(/^[a-f0-9]{64}$/),
    }),
  ),
});

/** Exact-version native production commands resolved by the testkit. */
export type TrellisNativeDistribution = { cli: string; server: string };

/** Maps supported host platforms to targets published by the native Check jobs. */
export function nativeTarget(os: string, arch: string): string {
  const cpu = arch === "x86_64"
    ? "x86_64"
    : arch === "aarch64" || arch === "arm64"
    ? "aarch64"
    : undefined;
  const platform = os === "linux"
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
  return Deno.env.get("TRELLIS_TEST_CACHE_DIR") ??
    join(
      Deno.env.get("HOME") ?? Deno.env.get("TMPDIR") ?? Deno.cwd(),
      ".cache",
      "trellis-testkit",
    );
}

/** Verify archive integrity against its release-generated pin. */
export async function verifyNativeArchive(
  bytes: Uint8Array,
  expected: string,
): Promise<void> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new Uint8Array(bytes).buffer,
  );
  const actual = Array.from(
    new Uint8Array(digest),
    (byte) => byte.toString(16).padStart(2, "0"),
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
    parser.on(
      "meta",
      () => parser.abort(new Error("Unexpected native archive metadata")),
    );
    parser.on(
      "ignoredEntry",
      () => parser.abort(new Error("Unexpected native archive entry")),
    );
    parser.on("entry", (entry: ReadEntry) => {
      if (
        entry.type !== "File" ||
        !["trellis", "trellis-server"].includes(entry.path) ||
        files.has(entry.path) || entry.size <= 0 || entry.size > 1_073_741_824
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

async function verifyDistribution(
  paths: TrellisNativeDistribution,
): Promise<TrellisNativeDistribution> {
  const canonical = { cli: "", server: "" };
  for (const name of ["cli", "server"] as const) {
    const info = await Deno.lstat(paths[name]);
    if (
      !info.isFile || info.isSymlink ||
      info.mode !== null && (info.mode & 0o111) === 0
    ) throw new Error(`Trellis ${name} must be a regular executable file`);
    canonical[name] = await Deno.realPath(paths[name]);
    const output = await new Deno.Command(canonical[name], {
      args: name === "cli" ? ["--format", "json", "version"] : ["--version"],
      stdin: "null",
      stdout: "piped",
      stderr: "piped",
    }).output();
    if (!output.success) {
      throw new Error(`Cannot verify Trellis ${name} version`);
    }
    const text = new TextDecoder().decode(output.stdout).trim();
    if (name === "cli") {
      // Build provenance is not part of SemVer identity; checkout CLI builds add
      // +local.<commit>.dirty while retaining the exact package/prerelease version.
      const version = z.object({
        version: z.string().refine((value) =>
          value.split("+", 1)[0] === packageConfig.version
        ),
      }).safeParse(JSON.parse(text));
      if (!version.success) {
        throw new Error(
          `Trellis CLI must match testkit version ${packageConfig.version}`,
        );
      }
    } else if (text !== `trellis-server ${packageConfig.version}`) {
      throw new Error(
        `Trellis server must match testkit version ${packageConfig.version}`,
      );
    }
  }
  return canonical;
}

/** Resolve matching production binaries; explicit paths never fall back to downloads. */
export async function resolveNativeDistribution(
  source: TrellisNativeSource = { kind: "release" },
): Promise<TrellisNativeDistribution> {
  if (source.kind === "path") return await verifyDistribution(source);
  const manifest = manifestSchema.parse(nativeManifest);
  if (manifest.version !== packageConfig.version) {
    throw new Error(
      "Native manifest does not match the testkit package version",
    );
  }
  const target = nativeTarget(Deno.build.os, Deno.build.arch);
  const asset = manifest.targets[target];
  if (
    !asset ||
    asset.archive !== `trellis-${packageConfig.version}-${target}.tar.gz`
  ) {
    throw new Error(
      `No accepted native ${target} archive in this package; repository tests must use an explicit path source`,
    );
  }
  const parent = join(
    trellisTestCacheDir(),
    "trellis",
    packageConfig.version,
    target,
  );
  await Deno.mkdir(parent, { recursive: true, mode: 0o700 });
  const installed = join(parent, asset.sha256);
  const paths = {
    cli: join(installed, "trellis"),
    server: join(installed, "trellis-server"),
  };
  let archive: Uint8Array | undefined;
  try {
    const info = await Deno.lstat(installed);
    if (!info.isDirectory || info.isSymlink) {
      throw new Error("Unsafe native cache entry");
    }
    archive = await Deno.readFile(join(installed, "archive.tar.gz"));
    await verifyNativeArchive(archive, asset.sha256);
    const extracted = await extractNativeArchive(archive);
    for (const name of ["cli", "server"] as const) {
      const info = await Deno.lstat(paths[name]);
      if (!info.isFile || info.isSymlink) {
        throw new Error("Unsafe cached native executable");
      }
      const cached = await Deno.readFile(paths[name]);
      if (
        cached.length !== extracted[name].length ||
        cached.some((byte, index) => byte !== extracted[name][index])
      ) {
        throw new Error(
          "Cached native executable differs from the verified archive",
        );
      }
    }
    return await verifyDistribution(paths);
  } catch {
    archive = undefined;
  }
  const response = await fetch(
    `https://github.com/oats-center/trellis/releases/download/v${packageConfig.version}/${asset.archive}`,
    { signal: AbortSignal.timeout(60_000) },
  );
  if (!response.ok) {
    throw new Error(
      `Trellis native archive download failed (${response.status})`,
    );
  }
  archive = new Uint8Array(await response.arrayBuffer());
  await verifyNativeArchive(archive, asset.sha256);
  const extracted = await extractNativeArchive(archive);
  const staging = await Deno.makeTempDir({ dir: parent, prefix: ".install-" });
  let quarantine: string | undefined;
  try {
    await Deno.writeFile(join(staging, "trellis"), extracted.cli, {
      mode: 0o700,
    });
    await Deno.writeFile(join(staging, "trellis-server"), extracted.server, {
      mode: 0o700,
    });
    await Deno.writeFile(join(staging, "archive.tar.gz"), archive, {
      mode: 0o600,
    });
    await verifyDistribution({
      cli: join(staging, "trellis"),
      server: join(staging, "trellis-server"),
    });
    try {
      await Deno.rename(staging, installed);
    } catch (error) {
      const installedInfo = await Deno.lstat(installed).catch(() => undefined);
      if (!installedInfo) throw error;
      // A concurrent installer may have completed while we downloaded. Compare
      // the winner with the pin, never overwrite a valid executable in use.
      const existing = installedInfo.isDirectory && !installedInfo.isSymlink
        ? await Deno.readFile(join(installed, "archive.tar.gz")).catch(() =>
          new Uint8Array()
        )
        : new Uint8Array();
      let valid = false;
      try {
        await verifyNativeArchive(existing, asset.sha256);
        valid = true;
        for (const name of ["cli", "server"] as const) {
          const info = await Deno.lstat(paths[name]);
          const file = info.isFile && !info.isSymlink
            ? await Deno.readFile(paths[name])
            : new Uint8Array();
          if (
            file.length !== extracted[name].length ||
            file.some((byte, index) => byte !== extracted[name][index])
          ) valid = false;
        }
      } catch {
        valid = false;
      }
      if (!valid) {
        quarantine = join(parent, `.corrupt-${crypto.randomUUID()}`);
        await Deno.rename(installed, quarantine);
        await Deno.rename(staging, installed);
      }
    }
    return await verifyDistribution(paths);
  } finally {
    await Deno.remove(staging, { recursive: true }).catch(() => undefined);
    if (quarantine) await Deno.remove(quarantine, { recursive: true });
  }
}
