import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/**
 * Digests the files an embedded browser app build reads.
 *
 * The files are the ones Git lists under `pathspecs` (tracked, plus untracked
 * files that are not ignored), so generated and installed outputs never count.
 * crates/runtime/build.rs computes the same digest to detect a stale embedded
 * app; a change to this algorithm must change it there too.
 *
 * @param root The repository root the pathspecs are relative to.
 * @param pathspecs The source files and directories the build reads.
 * @returns The hex SHA-256 digest of every listed path and its contents.
 */
export function sourceFingerprint(root: string, pathspecs: string[]): string {
  const listed = execFileSync(
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
    { maxBuffer: 64 * 1024 * 1024 },
  );
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
