import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  lstatSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";

/**
 * The fingerprint recorded when the sources cannot be listed. It matches no
 * digest, so a checkout that can list them refuses the assets.
 */
export const UNCHECKED = "unchecked";

/**
 * Digests the files an embedded browser app build reads.
 *
 * The files are the ones Git lists under `pathspecs` (tracked, plus untracked
 * files that are not ignored), so generated and installed outputs never count.
 * crates/runtime/build.rs computes the same digest to detect a stale embedded
 * app; a change to this algorithm must change it there too.
 *
 * Outside a Git checkout, such as an unpacked source archive, nothing is
 * listed: like the build script, this warns and skips the check, returning
 * {@link UNCHECKED}, which no digest matches.
 *
 * @param root The repository root the pathspecs are relative to.
 * @param pathspecs The source files and directories the build reads.
 * @returns The hex SHA-256 digest of every listed path and its contents, or
 *   {@link UNCHECKED} when Git cannot list them.
 */
export function sourceFingerprint(root: string, pathspecs: string[]): string {
  let listed: Buffer;
  try {
    listed = execFileSync(
      "git",
      [
        "-C",
        root,
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        ...pathspecs,
      ],
      { maxBuffer: 64 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] },
    );
  } catch {
    console.warn(
      `cannot list the sources under ${root} with git; skipping the stale-asset fingerprint`,
    );
    return UNCHECKED;
  }
  const paths = [...new Set(listed.toString("utf8").split("\0"))]
    .filter((path) => path !== "")
    .sort((left, right) =>
      Buffer.compare(Buffer.from(left), Buffer.from(right)),
    );
  const digest = createHash("sha256");
  for (const path of paths) {
    const file = join(root, path);
    // A tracked file deleted from the working tree is no longer an input.
    if (!statSync(file, { throwIfNoEntry: false })?.isFile()) continue;
    const contents = createHash("sha256")
      .update(readFileSync(file))
      .digest("hex");
    digest.update(`${path}\0${contents}\n`);
  }
  return digest.digest("hex");
}

/**
 * Records the source fingerprint of an embedded app build at `destination`.
 *
 * The first line is the digest; the remaining lines are the pathspecs it
 * covers, which the runtime build script reads back to recompute it.
 *
 * @param fingerprint The digest {@link sourceFingerprint} returned before the
 *   build started, so edits made during the build count as changes.
 * @param pathspecs The pathspecs the digest covers.
 * @param destination The fingerprint file to write.
 */
export function writeFingerprint(
  fingerprint: string,
  pathspecs: string[],
  destination: string,
): void {
  writeFileSync(destination, `${fingerprint}\n${pathspecs.join("\n")}\n`);
}

/** Hashes a build tree including file names and contents. Missing paths and
 * symbolic links cannot match an intact generated directory. */
function treeDigest(root: string): string {
  const digest = createHash("sha256");
  function visit(relative: string): void {
    const path = join(root, relative);
    const stat = lstatSync(path, { throwIfNoEntry: false });
    digest.update(JSON.stringify(relative));
    if (stat?.isDirectory()) {
      digest.update("directory");
      for (const name of readdirSync(path).sort()) visit(join(relative, name));
    } else if (
      stat?.isFile() ||
      (stat?.isSymbolicLink() &&
        statSync(path, { throwIfNoEntry: false })?.isFile())
    ) {
      digest.update(stat.isSymbolicLink() ? "linked-file" : "file");
      digest.update(createHash("sha256").update(readFileSync(path)).digest());
    } else {
      digest.update("missing or unsupported");
    }
  }
  visit("");
  return digest.digest("hex");
}

/** Checks the embedded bundle against its sources, build environment and output
 * contents. Capture the new record from staging and write it after promotion. */
export function embeddedBuild(
  app: string,
  inputs: string[],
  generated: string[],
  destination: string,
) {
  const sources = sourceFingerprint(dirname(app), inputs);
  const sourceRecord = [sources, ...inputs, ""].join("\n");
  const env = Object.entries(process.env)
    .filter(
      ([key]) =>
        key.startsWith("VITE_") ||
        key.startsWith("PUBLIC_") ||
        ["SITE_BASE_PATH", "NODE_ENV"].includes(key),
    )
    .sort();
  const files = [
    ".env",
    ".env.local",
    ".env.production",
    ".env.production.local",
  ].map((name) => join(app, name));
  const inputKey = createHash("sha256")
    .update(
      JSON.stringify([
        sources,
        process.version,
        env,
        ...[...files, ...generated].map(treeDigest),
      ]),
    )
    .digest("hex");
  const cachePath = destination + ".build-key";
  const fingerprintPath = destination + ".fingerprint";
  function readRecord(path: string) {
    try {
      return readFileSync(path, "utf8");
    } catch (error) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT")
        return undefined;
      throw error;
    }
  }
  return {
    current:
      sources !== UNCHECKED &&
      readRecord(fingerprintPath) === sourceRecord &&
      readRecord(cachePath) === inputKey + treeDigest(destination),
    record(staged: string) {
      const key = inputKey + treeDigest(staged);
      return () => {
        writeFileSync(cachePath, key);
        writeFingerprint(sources, inputs, fingerprintPath);
      };
    },
  };
}
