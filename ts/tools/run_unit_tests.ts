import { spawnSync } from "node:child_process";
import { globSync, mkdirSync } from "node:fs";
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
const concurrency = process.env.TRELLIS_TEST_CONCURRENCY;
// Temporary files never go to the system /tmp: a caller's TMPDIR is kept, else
// the runner's temp in CI, else an ignored directory in the checkout.
const temp =
  process.env.TMPDIR ??
  process.env.RUNNER_TEMP ??
  resolve(import.meta.dirname, "../../target/tmp");
mkdirSync(temp, { recursive: true });
const result = spawnSync(
  process.execPath,
  [
    ...process.execArgv,
    "--test",
    ...(concurrency ? [`--test-concurrency=${concurrency}`] : []),
    ...new Set(files),
  ],
  { stdio: "inherit", env: { ...process.env, TMPDIR: temp } },
);
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
