import { copy } from "@std/fs";
import { join } from "@std/path";
import { z } from "zod";
import {
  extractNativeArchive,
  verifyNativeArchive,
} from "../packages/trellis-testkit/src/native_distribution.ts";

const [archives, destination] = Deno.args;
if (!archives || !destination) {
  throw new Error(
    "Usage: stage_testkit.ts <Check archive directory> <staging directory>",
  );
}
const configSchema = z.object({
  version: z.string(),
  compilerOptions: z.record(z.string(), z.unknown()).optional(),
}).passthrough();
const source = "ts/packages/trellis-testkit";
const config = configSchema.parse(
  JSON.parse(await Deno.readTextFile(join(source, "deno.json"))),
);
const root = configSchema.omit({ version: true }).parse(
  JSON.parse(await Deno.readTextFile("deno.json")),
);
const targets: Record<string, { archive: string; sha256: string }> = {};
for (
  const target of ["x86_64-unknown-linux-gnu"]
) {
  const archive = `trellis-${config.version}-${target}.tar.gz`;
  const sha256 = (await Deno.readTextFile(
    join(archives, `checksum-${config.version}-${target}-trellis.sha256`),
  )).trim().split(/\s+/)[0];
  if (!/^[a-f0-9]{64}$/.test(sha256)) {
    throw new Error(`Invalid Check checksum for ${target}`);
  }
  const bytes = await Deno.readFile(join(archives, archive));
  await verifyNativeArchive(bytes, sha256);
  await extractNativeArchive(bytes);
  targets[target] = { archive, sha256 };
}
await copy(source, destination);
delete config.extends;
delete config.tasks;
config.compilerOptions = { ...root.compilerOptions, ...config.compilerOptions };
await Deno.writeTextFile(
  join(destination, "deno.json"),
  JSON.stringify(config, null, 2) + "\n",
);
await Deno.writeTextFile(
  join(destination, "src/native/trellis-binaries.json"),
  JSON.stringify({ version: config.version, targets }, null, 2) + "\n",
);
