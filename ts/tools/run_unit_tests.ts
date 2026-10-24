import { spawnSync } from "node:child_process";
import { globSync } from "node:fs";
import { resolve } from "node:path";

const files = process.argv.slice(2).flatMap((directory) =>
  [
    ...globSync(["**/*_test.ts", "**/*.test.ts"], {
      cwd: directory,
      exclude: [
        "**/node_modules/**",
        "**/npm/**",
        "**/*.live_test.ts",
        "**/*.live.test.ts",
      ],
    }),
  ].map((file) => resolve(directory, file)),
);
if (files.length === 0) throw new Error("No unit tests discovered");
const result = spawnSync(
  process.execPath,
  [
    ...process.execArgv,
    "--experimental-transform-types",
    "--test",
    ...new Set(files),
  ],
  { stdio: "inherit" },
);
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
