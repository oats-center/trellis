import {
  deepStrictEqual,
  notStrictEqual,
  strictEqual,
} from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

test("failed compilation and unowned output cannot replace the last-good portal bundle", async () => {
  const root = await mkdtemp(join(tmpdir(), "trellis-portal-build-"));
  try {
    const app = join(root, "portal");
    await mkdir(join(app, "scripts"), { recursive: true });
    await copyFile(
      new URL("./build_embedded.ts", import.meta.url),
      join(app, "scripts/build_embedded.ts"),
    );
    await symlink(
      new URL("../../node_modules", import.meta.url),
      join(app, "node_modules"),
      "dir",
    );
    await writeFile(
      join(app, "package.json"),
      JSON.stringify({
        name: "portal-build-fixture",
        private: true,
        type: "module",
      }),
    );
    await writeFile(
      join(app, "index.html"),
      '<script type="module" src="/main.js"></script>',
    );
    await writeFile(
      join(app, "main.js"),
      'document.body.textContent = "last-good";',
    );

    const built = spawnSync(process.execPath, ["scripts/build_embedded.ts"], {
      cwd: app,
      encoding: "utf8",
    });
    strictEqual(built.status, 0, built.stderr + built.stdout);
    const output = join(root, "crates/runtime/generated/portal");
    const entrypoint = await readFile(join(output, "index.html"));
    const assets = await readdir(join(output, "assets"));
    const javascript = await readFile(join(output, "assets", assets[0]));

    await writeFile(join(app, "main.js"), "export const = ;");
    const failed = spawnSync(process.execPath, ["scripts/build_embedded.ts"], {
      cwd: app,
      encoding: "utf8",
    });
    notStrictEqual(failed.status, 0);
    deepStrictEqual(await readFile(join(output, "index.html")), entrypoint);
    deepStrictEqual(
      await readFile(join(output, "assets", assets[0])),
      javascript,
    );

    await writeFile(
      join(app, "main.js"),
      'document.body.textContent = "replacement";',
    );
    await rm(join(output, ".trellis-portal-owned"));
    const refused = spawnSync(process.execPath, ["scripts/build_embedded.ts"], {
      cwd: app,
      encoding: "utf8",
    });
    notStrictEqual(refused.status, 0);
    deepStrictEqual(await readFile(join(output, "index.html")), entrypoint);
    deepStrictEqual(
      await readFile(join(output, "assets", assets[0])),
      javascript,
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
