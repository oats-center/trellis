import { spawnSync } from "node:child_process";
import {
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const parent = join(root, "../crates/runtime/generated");
const destination = join(parent, "portal");
await mkdir(parent, { recursive: true });
const stage = await mkdtemp(join(parent, ".portal-build-"));
const backup = `${stage}-previous`;
let previous = false;
let promoted = false;
try {
  const command = spawnSync(
    "npm",
    ["exec", "--", "vite", "build", "--outDir", stage],
    {
      cwd: root,
      stdio: "inherit",
    },
  );
  if (command.error) throw command.error;
  if (command.status !== 0)
    throw new Error(`Portal build failed (${command.status})`);
  await writeFile(join(stage, ".trellis-portal-owned"), "portal\n");
  const info = await lstat(destination).catch((error: unknown) => {
    if (error instanceof Error && "code" in error && error.code === "ENOENT")
      return undefined;
    throw error;
  });
  if (info) {
    if (!info.isDirectory() || info.isSymbolicLink()) {
      throw new Error(
        `Refusing to replace non-directory portal output: ${destination}`,
      );
    }
    if (
      (await readFile(join(destination, ".trellis-portal-owned"), "utf8")) !==
      "portal\n"
    ) {
      throw new Error(
        `Refusing to replace unowned portal output: ${destination}`,
      );
    }
    await rename(destination, backup);
    previous = true;
  }
  try {
    await rename(stage, destination);
    promoted = true;
  } catch (error) {
    if (previous) await rename(backup, destination);
    throw error;
  }
} finally {
  if (promoted && previous) await rm(backup, { recursive: true });
  if (!promoted) await rm(stage, { recursive: true });
}
