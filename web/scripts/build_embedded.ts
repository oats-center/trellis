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

const webDir = dirname(dirname(fileURLToPath(import.meta.url)));
const destination = join(webDir, "../crates/runtime/generated/web");

const parent = dirname(destination);
await mkdir(parent, { recursive: true });
const buildDir = await mkdtemp(join(parent, ".web-build-"));
const backup = `${buildDir}-previous`;
let previous = false;
let promoted = false;
try {
  const build = spawnSync("npm", ["run", "build:prepared"], {
    cwd: webDir,
    env: {
      ...process.env,
      TRELLIS_WEB_BUILD_DIR: buildDir,
      TRELLIS_WEB_VERSION: "embedded",
    },
    stdio: "inherit",
  });
  if (build.error) throw build.error;
  if (build.status !== 0)
    throw new Error(`Console build failed (${build.status})`);

  const fallbackPath = join(buildDir, "200.html");
  let fallback = await readFile(fallbackPath, "utf8");
  const head = fallback.slice(0, fallback.indexOf("</head>"));
  const inlineScripts = [...head.matchAll(/<script>([\s\S]*?)<\/script>/g)];
  if (inlineScripts.length !== 2) {
    throw new Error(
      `expected route restoration and theme scripts, found ${inlineScripts.length}`,
    );
  }
  for (const [index, script] of inlineScripts.entries()) {
    const name = index === 0 ? "restore-route.js" : "theme.js";
    await writeFile(join(buildDir, `assets/web/${name}`), script[1]);
    fallback = fallback.replace(
      script[0],
      `<script src="/assets/web/${name}"></script>`,
    );
  }
  const bootstrapScripts = [
    ...fallback.matchAll(/<script>([\s\S]*?)<\/script>/g),
  ];
  if (bootstrapScripts.length !== 1) {
    throw new Error(
      `expected SvelteKit bootstrap script, found ${bootstrapScripts.length}`,
    );
  }
  await writeFile(
    join(buildDir, "assets/web/bootstrap.js"),
    bootstrapScripts[0][1],
  );
  fallback = fallback.replace(
    bootstrapScripts[0][0],
    '<script src="/assets/web/bootstrap.js"></script>',
  );
  await writeFile(fallbackPath, fallback);
  await writeFile(
    join(buildDir, "assets/web/runtime-config.js"),
    "globalThis.__TRELLIS_RUNTIME_CONFIG__ = { authUrl: globalThis.location.origin };\n",
  );

  await writeFile(join(buildDir, ".trellis-web-owned"), "web\n");
  const info = await lstat(destination).catch((error: unknown) => {
    if (error instanceof Error && "code" in error && error.code === "ENOENT")
      return undefined;
    throw error;
  });
  if (info) {
    if (
      !info.isDirectory() ||
      info.isSymbolicLink() ||
      (await readFile(join(destination, ".trellis-web-owned"), "utf8")) !==
        "web\n"
    ) {
      throw new Error(
        `Refusing to replace unowned Console output: ${destination}`,
      );
    }
    await rename(destination, backup);
    previous = true;
  }
  try {
    await rename(buildDir, destination);
    promoted = true;
  } catch (error) {
    if (previous) await rename(backup, destination);
    throw error;
  }
} finally {
  if (promoted && previous) await rm(backup, { recursive: true });
  if (!promoted) await rm(buildDir, { recursive: true });
}
