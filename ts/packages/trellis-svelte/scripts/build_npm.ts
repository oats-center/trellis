import { copyFile, writeFile } from "node:fs/promises";
import config from "../package.json" with { type: "json" };
import { runtimeDependencies } from "../../../tools/package_build/runtime_dependencies.ts";
import {
  resolveInternalNpmDependenciesForBuild,
  resolvePackageBuildVersion,
} from "../../../tools/package_build/release_build_version.ts";

const version = resolvePackageBuildVersion(config.version);
await writeFile(
  "npm/package.json",
  JSON.stringify(
    {
      name: config.name,
      version,
      description:
        "Svelte components and state helpers for Trellis browser applications.",
      type: "module",
      license: config.license,
      publishConfig: { access: "public" },
      repository: {
        type: "git",
        url: "git+https://github.com/oats-center/trellis.git",
      },
      exports: {
        ".": {
          types: "./index.d.ts",
          svelte: "./index.js",
          default: "./index.js",
        },
      },
      files: [
        "**/*.js",
        "**/*.d.ts",
        "**/*.svelte",
        "README.md",
        "!**/*_test.*",
        "!**/*.test.*",
        "!**/*.spec.*",
      ],
      dependencies: resolveInternalNpmDependenciesForBuild(
        {
          ...config.dependencies,
          ...runtimeDependencies(["@nats-io/nats-core", "typebox", "ulid"]),
        },
        version,
      ),
      peerDependencies: config.peerDependencies,
    },
    null,
    2,
  ) + "\n",
);
await copyFile("README.md", "npm/README.md");
