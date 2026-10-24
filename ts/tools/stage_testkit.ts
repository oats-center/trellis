import { cp, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import {
  extractNativeArchive,
  verifyNativeArchive,
} from "../packages/trellis-testkit/src/native_distribution.ts";

const [archives, destination] = process.argv.slice(2);
if (!archives || !destination)
  throw new Error(
    "Usage: stage_testkit.ts <Check archive directory> <staging directory>",
  );
const source = "ts/packages/trellis-testkit/npm";
const { version } = z
  .object({ version: z.string() })
  .parse(JSON.parse(await readFile(join(source, "package.json"), "utf8")));
const targets: Record<string, { archive: string; sha256: string }> = {};
for (const target of ["x86_64-unknown-linux-gnu"]) {
  const archive = `trellis-${version}-${target}.tar.gz`;
  const sha256 = (
    await readFile(
      join(archives, `checksum-${version}-${target}-trellis.sha256`),
      "utf8",
    )
  )
    .trim()
    .split(/\s+/)[0];
  if (!sha256 || !/^[a-f0-9]{64}$/.test(sha256))
    throw new Error(`Invalid Check checksum for ${target}`);
  const bytes = await readFile(join(archives, archive));
  await verifyNativeArchive(bytes, sha256);
  await extractNativeArchive(bytes);
  targets[target] = { archive, sha256 };
}
await cp(source, destination, { recursive: true });
await writeFile(
  join(destination, "src/native/trellis-binaries.json"),
  JSON.stringify({ version, targets }, null, 2) + "\n",
);
