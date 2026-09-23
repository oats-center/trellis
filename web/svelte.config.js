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
      "@oats-center/result": resolve(rootDir, "../ts/packages/result/mod.ts"),
      "@oats-center/trellis-svelte": resolve(
        rootDir,
        "../ts/packages/trellis-svelte/src/index.ts",
      ),
      "@oats-center/trellis/auth/browser": resolve(
        rootDir,
        "../ts/packages/trellis/auth/browser.ts",
      ),
      "@oats-center/trellis/auth": resolve(
        rootDir,
        "../ts/packages/trellis/auth.ts",
      ),
      "@oats-center/trellis/generated": resolve(
        rootDir,
        "../ts/packages/trellis/generated.ts",
      ),
      "@oats-center/trellis/device": resolve(
        rootDir,
        "../ts/packages/trellis/device.ts",
      ),
      "trellis-web-generated": resolve(rootDir, "trellis"),
      "@oats-center/trellis/service": resolve(
        rootDir,
        "../ts/packages/trellis/service/mod.ts",
      ),
      "@oats-center/trellis/telemetry": resolve(
        rootDir,
        "../ts/packages/trellis/telemetry.ts",
      ),
      "@oats-center/trellis": resolve(
        rootDir,
        "../ts/packages/trellis/index.ts",
      ),
    },
  },
};

export default config;
