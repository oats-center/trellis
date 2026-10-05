import { fromFileUrl, relative, resolve } from "@std/path";
import { parseArgs } from "@std/cli/parse-args";
import { z } from "zod";

// Reuse the first-party runtime runner and report, including integrity failures.
const root = fromFileUrl(new URL("../../", import.meta.url));
const args = parseArgs(Deno.args, {
  string: [
    "output",
    "server",
    "cli",
    "samples",
    "calls",
    "warmups",
    "compare",
    "source-root",
  ],
  default: { samples: "21", calls: "100", warmups: "3" },
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
await Deno.mkdir(runtimeTemp, { recursive: true });
const env = {
  TMPDIR: runtimeTemp,
  TRELLIS_TEST_CACHE_DIR: `${root}/target/transfer-testkit-cache`,
};
const run = await new Deno.Command(Deno.execPath(), {
  cwd: sourceRoot,
  env,
  args: [
    "run",
    "-A",
    "-c",
    `${sourceRoot}/ts/deno.json`,
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
  stdout: "inherit",
  stderr: "inherit",
}).output();
if (!run.success) {
  Deno.exitCode = run.code;
} else {
  const report = await new Deno.Command(Deno.execPath(), {
    cwd: root,
    args: [
      "run",
      "-A",
      "-c",
      `${root}/ts/deno.json`,
      `${root}/benchmarks/runtime/report.ts`,
      output,
      ...(args.compare ? [resolve(args.compare)] : []),
    ],
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  Deno.exitCode = report.code;
}
