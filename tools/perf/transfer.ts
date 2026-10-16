import { mkdir } from "node:fs/promises";
import { relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { z } from "zod";
import { spawnTracked } from "../../ts/integration/_support/process.ts";

// Reuse the first-party runtime runner and report, including integrity failures.
const root = fileURLToPath(new URL("../../", import.meta.url));
const { values: args } = parseArgs({
  options: {
    output: { type: "string" },
    server: { type: "string" },
    cli: { type: "string" },
    samples: { type: "string", default: "21" },
    calls: { type: "string", default: "100" },
    warmups: { type: "string", default: "3" },
    compare: { type: "string" },
    "source-root": { type: "string" },
  },
});
const output = resolve(z.string().min(1).parse(args.output));
const sourceRoot = args["source-root"] ? resolve(args["source-root"]) : root;
const withinRepo = relative(root, output);
if (withinRepo === ".." || withinRepo.startsWith("../")) {
  throw new Error("Transfer audit output must be inside the repository");
}
const sourceWithinRepo = relative(root, sourceRoot);
if (sourceWithinRepo === ".." || sourceWithinRepo.startsWith("../")) {
  throw new Error("Transfer source snapshot must be inside the repository");
}
// Existing testkit infrastructure still owns lifecycle/cleanup; keep its
// temporary state and binary cache inside the designated repository as well.
const runtimeTemp = `${root}/target/transfer-runtime-temp`;
await mkdir(runtimeTemp, { recursive: true });
const env = {
  TMPDIR: runtimeTemp,
  TRELLIS_TEST_CACHE_DIR: `${root}/target/transfer-testkit-cache`,
};
const run = await spawnTracked(
  process.execPath,
  [
    "--experimental-transform-types",
    `${sourceRoot}/benchmarks/runtime/run.ts`,
    "--lane=transfer",
    "--sizes=8388608",
    "--sessions=1",
    "--idle-seconds=1",
    `--output=${output}`,
    `--samples=${args.samples}`,
    `--calls=${args.calls}`,
    `--warmups=${args.warmups}`,
    ...(args.server ? [`--server=${resolve(args.server)}`] : []),
    ...(args.cli ? [`--cli=${resolve(args.cli)}`] : []),
  ],
  { cwd: sourceRoot, env: { ...process.env, ...env }, stdio: "inherit" },
).exited;
if (run.code !== 0) {
  process.exitCode = run.code ?? 1;
} else {
  const report = await spawnTracked(
    process.execPath,
    [
      "--experimental-transform-types",
      `${root}/benchmarks/runtime/report.ts`,
      output,
      ...(args.compare ? [resolve(args.compare)] : []),
    ],
    { cwd: root, stdio: "inherit" },
  ).exited;
  process.exitCode = report.code ?? 1;
}
