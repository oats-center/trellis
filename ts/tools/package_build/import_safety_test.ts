import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../../", import.meta.url));

for (const subpath of ["", "/telemetry"]) {
  test(
    subpath
      ? "published telemetry imports without telemetry SDK packages"
      : "published SDK imports without project-local API packages",
    async () => {
      // Use producer-built packages, never rebuild them inside execution checks.
      const source = join(root, "ts/packages/trellis/npm");
      const metadata: { dependencies: Record<string, string> } = JSON.parse(
        await readFile(join(source, "package.json"), "utf8"),
      );
      const directory = await mkdtemp(join(tmpdir(), "trellis-import-"));
      try {
        const modules = join(directory, "node_modules");
        await cp(source, join(modules, "@oatscenter/trellis"), {
          recursive: true,
        });
        for (const name of Object.keys(metadata.dependencies)) {
          if (
            subpath &&
            name.startsWith("@opentelemetry/") &&
            name !== "@opentelemetry/api"
          )
            continue;
          const destination = join(modules, name);
          await mkdir(dirname(destination), { recursive: true });
          if (name === "@oatscenter/result") {
            await cp(join(root, "ts/packages/result/npm"), destination, {
              recursive: true,
            });
          } else {
            await symlink(
              join(root, "node_modules", name),
              destination,
              "junction",
            );
          }
        }
        const child = spawnSync(
          process.execPath,
          [
            "--input-type=module",
            "--eval",
            `await import(${JSON.stringify(`@oatscenter/trellis${subpath}`)});`,
          ],
          { cwd: directory, encoding: "utf8" },
        );
        if (child.error) throw child.error;
        assert.equal(child.status, 0, child.stderr);
      } finally {
        await rm(directory, { recursive: true, force: true });
      }
    },
  );
}
