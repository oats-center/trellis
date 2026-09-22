import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

const rootDir = dirname(fileURLToPath(import.meta.url));
const buildDir = process.env.TRELLIS_WEB_BUILD_DIR ?? "build";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    appDir: "assets/web",
    version: { name: process.env.TRELLIS_WEB_VERSION ?? "standalone" },
    paths: { base: process.env.SITE_BASE_PATH ?? "" },
    adapter: adapter({
      pages: buildDir,
      assets: buildDir,
      fallback: "200.html",
    }),
    alias: {
      "@qlever-llc/result": resolve(rootDir, "../ts/packages/result/mod.ts"),
      "@qlever-llc/trellis-svelte": resolve(
        rootDir,
        "../ts/packages/trellis-svelte/src/index.ts",
      ),
      "@qlever-llc/trellis/auth/browser": resolve(
        rootDir,
        "../ts/packages/trellis/auth/browser.ts",
      ),
      "@qlever-llc/trellis/auth": resolve(
        rootDir,
        "../ts/packages/trellis/auth.ts",
      ),
      "@qlever-llc/trellis/generated": resolve(
        rootDir,
        "../ts/packages/trellis/generated.ts",
      ),
      "@qlever-llc/trellis/device": resolve(
        rootDir,
        "../ts/packages/trellis/device.ts",
      ),
      "trellis-web-generated": resolve(rootDir, "trellis"),
      "@qlever-llc/trellis/service": resolve(
        rootDir,
        "../ts/packages/trellis/service/mod.ts",
      ),
      "@qlever-llc/trellis/telemetry": resolve(
        rootDir,
        "../ts/packages/trellis/telemetry.ts",
      ),
      "@qlever-llc/trellis": resolve(
        rootDir,
        "../ts/packages/trellis/index.ts",
      ),
    },
  },
};

export default config;
