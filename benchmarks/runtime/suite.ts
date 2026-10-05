import { parseArgs } from "@std/cli/parse-args";
import { fromFileUrl, resolve } from "@std/path";
import { z } from "zod";

const root = fromFileUrl(new URL("../../", import.meta.url));
const args = parseArgs(Deno.args, {
  boolean: ["report-only"],
  string: [
    "output",
    "server",
    "cli",
    "rust-bin",
    "browser-bundle",
    "repeats",
    "samples",
    "calls",
    "sizes",
    "sessions",
    "idle-seconds",
    "provider-counts",
    "actions",
    "compare",
  ],
  default: {
    repeats: "3",
    samples: "7",
    calls: "1000",
    sizes: "65536,1048576,8388608",
    sessions: "1,10,100,500",
    "idle-seconds": "5",
    "provider-counts": "1,4",
    actions: "1,32,128",
  },
});
const output = resolve(z.string().parse(args.output));
const positive = z.coerce.number().int().positive();
const repeats = positive.parse(args.repeats);
const providers = z.array(positive).nonempty().parse(
  args["provider-counts"].split(","),
);
const actions = z.array(positive).nonempty().parse(args.actions.split(","));
const binaries = {
  server: resolve(
    z.string().parse(args.server ?? (args["report-only"] ? "" : undefined)),
  ),
  cli: resolve(
    z.string().parse(args.cli ?? (args["report-only"] ? "" : undefined)),
  ),
  rust: resolve(
    z.string().parse(
      args["rust-bin"] ?? (args["report-only"] ? "" : undefined),
    ),
  ),
  browser: resolve(
    z.string().parse(
      args["browser-bundle"] ?? (args["report-only"] ? "" : undefined),
    ),
  ),
};
if (!args["report-only"]) {
  for (const file of Object.values(binaries)) {
    if (!(await Deno.stat(file)).isFile) {
      throw new Error(`Missing producer artifact: ${file}`);
    }
  }
  await Deno.mkdir(output, { recursive: true });
  const lock = await Deno.open(`${output}/suite.lock`, {
    createNew: true,
    write: true,
  });
  lock.close();
}
const config = {
  repeats,
  samples: positive.parse(args.samples),
  calls: positive.parse(args.calls),
  sizes: args.sizes,
  sessions: args.sessions,
  idleSeconds: Number(args["idle-seconds"]),
  providers,
  actions,
  host: Deno.hostname(),
};
if (!args["report-only"]) {
  await Deno.writeTextFile(
    `${output}/suite-metadata.json`,
    JSON.stringify(config, null, 2),
  );
}
const execute = async (script: string, flags: string[], log: string) => {
  const process = new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "-A",
      "-c",
      `${root}/ts/deno.json`,
      `${root}/benchmarks/runtime/${script}`,
      ...flags,
    ],
    cwd: root,
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const file = await Deno.open(log, {
    create: true,
    write: true,
    truncate: true,
  });
  // Drain both pipes while running; do not buffer a long suite in memory.
  const writes = [
    process.stdout.pipeTo(file.writable, { preventClose: true }),
    process.stderr.pipeTo(
      new WritableStream({
        async write(chunk) {
          await file.write(chunk);
        },
      }),
    ),
  ];
  const result = await process.status;
  await Promise.all(writes);
  file.close();
  return result.success;
};
if (
  !args["report-only"] && !await execute("contracts.ts", [
    "--cli",
    binaries.cli,
    "--output",
    `${output}/generated`,
    "--actions",
    args.actions,
  ], `${output}/generation.log`)
) throw new Error("Contract generation failed; see generation.log");
const cases = [
  ...providers.map((count) => ({
    key: `typescript-providers-${count}`,
    flags: ["--lane", "typescript", "--providers", String(count)],
  })),
  { key: "rust", flags: ["--lane", "rust", "--rust-bin", binaries.rust] },
  {
    key: "browser",
    flags: ["--lane", "browser", "--browser-bundle", binaries.browser],
  },
  { key: "lifecycle", flags: ["--lane", "lifecycle"] },
  ...actions.map((count) => ({
    key: `contract-actions-${count}`,
    flags: [
      "--lane",
      "contract",
      "--contract-entry",
      `${output}/generated/contracts-${count}/packages/scale/index.js`,
      "--contract-worker",
      `${output}/generated/contracts-${count}/worker.ts`,
    ],
  })),
];
const rowSchema = z.object({
  name: z.string(),
  attempts: z.number(),
  errors: z.number(),
  medianMs: z.number().nullable(),
  p95Ms: z.number().nullable(),
  phases: z.record(z.string(), z.number().nullable()).default({}),
});
const rows: {
  case: string;
  repeat: number;
  report: string;
  success: boolean;
  summary: z.infer<typeof rowSchema>[];
}[] = [];
if (args["report-only"]) {
  rows.push(
    ...z.array(
      z.object({
        case: z.string(),
        repeat: positive,
        report: z.string(),
        success: z.boolean(),
        summary: z.array(rowSchema),
      }),
    ).parse(JSON.parse(await Deno.readTextFile(`${output}/suite-runs.json`))),
  );
  for (const run of rows) {
    try {
      run.summary = z.object({ summary: z.array(rowSchema) }).parse(
        JSON.parse(
          await Deno.readTextFile(
            `${output}/${run.report.replace(/report\.md$/, "samples.json")}`,
          ),
        ),
      ).summary;
    } catch (error) {
      if (!(error instanceof Deno.errors.NotFound)) throw error;
    }
  }
}
if (!args["report-only"]) {
  for (let repeat = 1; repeat <= repeats; repeat++) {
    // Rotate order deterministically; never run measured cases concurrently.
    const ordered = [
      ...cases.slice((repeat - 1) % cases.length),
      ...cases.slice(0, (repeat - 1) % cases.length),
    ];
    for (const item of ordered) {
      const dir = `${output}/repeat-${repeat}/${item.key}`;
      await Deno.mkdir(`${output}/repeat-${repeat}`, { recursive: true });
      console.log(`Repeat ${repeat}/${repeats}: ${item.key}`);
      const success = await execute("run.ts", [
        "--server",
        binaries.server,
        "--output",
        dir,
        "--samples",
        args.samples,
        "--calls",
        args.calls,
        "--sizes",
        args.sizes,
        "--sessions",
        item.key.startsWith("typescript") ? args.sessions : "1",
        "--idle-seconds",
        args["idle-seconds"],
        ...item.flags,
      ], `${output}/repeat-${repeat}/${item.key}.log`);
      let summary: z.infer<typeof rowSchema>[] = [];
      try {
        summary = z.object({ summary: z.array(rowSchema) }).parse(
          JSON.parse(await Deno.readTextFile(`${dir}/samples.json`)),
        ).summary;
      } catch (error) {
        if (!(error instanceof Deno.errors.NotFound)) throw error;
      }
      const reportSuccess = summary.length > 0 &&
        await execute(
          "report.ts",
          [dir],
          `${output}/repeat-${repeat}/${item.key}-report.log`,
        );
      rows.push({
        case: item.key,
        repeat,
        report: `repeat-${repeat}/${item.key}/report.md`,
        success: success && reportSuccess,
        summary,
      });
      await Deno.writeTextFile(
        `${output}/suite-runs.json`,
        JSON.stringify(rows, null, 2),
      );
    }
  }
}
const groups = new Map<
  string,
  {
    medians: number[];
    p95s: number[];
    attempts: number;
    errors: number;
    failures: number;
    runs: number;
    phases: Record<string, number[]>;
  }
>();
for (const run of rows) {
  for (const row of run.summary) {
    const key = `${run.case}/${row.name}`;
    const group = groups.get(key) ??
      {
        medians: [],
        p95s: [],
        attempts: 0,
        errors: 0,
        failures: 0,
        runs: 0,
        phases: {},
      };
    group.attempts += row.attempts;
    group.errors += row.errors;
    group.runs++;
    if (!run.success || row.errors) group.failures++;
    else {
      for (const [phase, value] of Object.entries(row.phases)) {
        if (value !== null) (group.phases[phase] ??= []).push(value);
      }
      if (row.medianMs !== null) group.medians.push(row.medianMs);
      if (row.p95Ms !== null) group.p95s.push(row.p95Ms);
    }
    groups.set(key, group);
  }
}
const median = (values: number[]) => {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted.length ? sorted[Math.floor(sorted.length / 2)] : null;
};
const summary = Object.fromEntries(
  [...groups].map((
    [name, group],
  ) => [name, {
    ...group,
    medianOfMediansMs: median(group.medians),
    medianOfP95sMs: median(group.p95s),
  }]),
);
const savedConfig = z.object({
  repeats: positive,
  providers: z.array(positive),
  actions: z.array(positive),
}).parse(JSON.parse(await Deno.readTextFile(`${output}/suite-metadata.json`)));
const expectedRuns = savedConfig.repeats *
  (savedConfig.providers.length + savedConfig.actions.length + 3);
await Deno.writeTextFile(
  `${output}/suite-summary.json`,
  JSON.stringify(summary, null, 2),
);
const lines = [
  "# Trellis performance suite",
  "",
  `Completed cases: **${rows.length}/${expectedRuns}**. ${
    rows.length < expectedRuns
      ? "PARTIAL — no complete baseline is claimed."
      : rows.some((run) => !run.success)
      ? "FAILED — not a clean baseline."
      : "All cases passed."
  }`,
  "",
  "Sequential isolated runtimes; independent repeats. Between-run medians are not pooled request percentiles. A failing run never contributes a clean score. HTTP is an unauthenticated same-language reference with different storage durability.",
  "",
  "## Trellis vs HTTP — matched workloads",
  "",
  "Trellis/HTTP is a latency ratio, not a claim of equivalent security or storage. Transfers include integrity verification on both sides. Trellis persists through JetStream; HTTP uses filesystem storage. Four-provider Trellis cases still use one HTTP reference provider. Failed case runs are excluded from both scores, with their failures retained below.",
  "",
  "| Case | Workload | Bytes | Parallel requests | Trellis median ms | HTTP median ms | Trellis / HTTP | Trellis p95 ms | HTTP p95 ms |",
  "|---|---|---:|---:|---:|---:|---:|---:|---:|",
];
for (const [name, group] of Object.entries(summary)) {
  if (!name.includes("/trellis/")) continue;
  const reference = summary[name.replace("/trellis/", "/http/")];
  if (!reference) continue;
  const [caseName, _transport, ...parts] = name.split("/");
  const parallel = parts.pop();
  const bytes = parts.pop();
  lines.push(
    `| ${caseName} | ${parts.join("/")} | ${bytes} | ${parallel} | ${
      group.medianOfMediansMs?.toFixed(3) ?? "—"
    } | ${reference.medianOfMediansMs?.toFixed(3) ?? "—"} | ${
      group.medianOfMediansMs !== null &&
        reference.medianOfMediansMs !== null && reference.medianOfMediansMs > 0
        ? (group.medianOfMediansMs / reference.medianOfMediansMs).toFixed(2) +
          "×"
        : "—"
    } | ${group.medianOfP95sMs?.toFixed(3) ?? "—"} | ${
      reference.medianOfP95sMs?.toFixed(3) ?? "—"
    } |`,
  );
}
lines.push(
  "",
  "## All workloads and failures",
  "",
  "| Case / workload | Runs / failed | Requests / errors | Median of medians ms | Run-median range ms | Median of run p95s ms |",
  "|---|---:|---:|---:|---:|---:|",
);
for (const [name, group] of Object.entries(summary)) {
  lines.push(
    `| ${name} | ${group.runs} / ${group.failures} | ${group.attempts} / ${group.errors} | ${
      group.medianOfMediansMs?.toFixed(3) ?? "—"
    } | ${
      group.medians.length
        ? `${Math.min(...group.medians).toFixed(3)}–${
          Math.max(...group.medians).toFixed(3)
        }`
        : "—"
    } | ${group.medianOfP95sMs?.toFixed(3) ?? "—"} |`,
  );
}
lines.push(
  "",
  "## Readiness and delivery — medians of run medians",
  "",
  "All durations are ms; transferred bytes are separate. Failed runs do not contribute phase medians.",
  "",
  "| Case / workload | Connect | First RPC | First payload byte | Document TTFB | Navigation → first authorized RPC | Transferred bytes |",
  "|---|---:|---:|---:|---:|---:|---:|",
);
for (const [name, group] of groups) {
  if (Object.values(group.phases).some((values) => values.length > 0)) {
    lines.push(
      `| ${name} | ${
        [
          "connectMs",
          "firstRpcMs",
          "firstByteMs",
          "documentTtfbMs",
          "navigationReadyMs",
          "transferredBytes",
        ].map((phase) => median(group.phases[phase] ?? [])?.toFixed(3) ?? "—")
          .join(" | ")
      } |`,
    );
  }
}
if (args.compare) {
  const previous = resolve(args.compare);
  const oldConfig: unknown = JSON.parse(
    await Deno.readTextFile(`${previous}/suite-metadata.json`),
  );
  const candidateConfig: unknown = JSON.parse(
    await Deno.readTextFile(`${output}/suite-metadata.json`),
  );
  if (JSON.stringify(oldConfig) !== JSON.stringify(candidateConfig)) {
    throw new Error(
      "Comparison requires identical workload configuration, repeat count and host",
    );
  }
  const comparisonKeys = [
    "host",
    "deno",
    "cpuModel",
    "logicalCpus",
    "cgroup",
    "cpuTicks",
    "samples",
    "calls",
    "sizes",
    "sessionCounts",
    "idleSeconds",
    "arrivalRate",
    "maxOutstanding",
    "lane",
    "providerCount",
    "passwordHashParameters",
    "topology",
    "security",
    "storage",
    "telemetry",
  ];
  for (const run of rows.filter((run) => run.repeat === 1)) {
    const metadataPath = run.report.replace(/report\.md$/, "metadata.json");
    const current = z.record(z.string(), z.unknown()).parse(
      JSON.parse(await Deno.readTextFile(`${output}/${metadataPath}`)),
    );
    const previousMetadata = z.record(z.string(), z.unknown()).parse(
      JSON.parse(await Deno.readTextFile(`${previous}/${metadataPath}`)),
    );
    const mismatches = comparisonKeys.filter((key) =>
      JSON.stringify(current[key]) !== JSON.stringify(previousMetadata[key])
    );
    if (mismatches.length) {
      throw new Error(
        `Cannot compare ${run.case}: ${mismatches.join(", ")} differ`,
      );
    }
  }
  const prior = z.record(
    z.string(),
    z.object({
      medianOfMediansMs: z.number().nullable(),
      failures: z.number(),
    }),
  ).parse(
    JSON.parse(await Deno.readTextFile(`${previous}/suite-summary.json`)),
  );
  lines.push(
    "",
    "## Matched previous suite",
    "",
    "Negative changes are faster. Failed/missing baselines are not compared.",
    "",
    "| Workload | Previous ms | Current ms | Change |",
    "|---|---:|---:|---:|",
  );
  for (const [name, group] of Object.entries(summary)) {
    const old = prior[name];
    if (
      old && !old.failures && !group.failures && old.medianOfMediansMs &&
      group.medianOfMediansMs !== null
    ) {
      lines.push(
        `| ${name} | ${old.medianOfMediansMs.toFixed(3)} | ${
          group.medianOfMediansMs.toFixed(3)
        } | ${
          ((group.medianOfMediansMs / old.medianOfMediansMs - 1) * 100).toFixed(
            1,
          )
        }% |`,
      );
    }
  }
}
lines.push(
  "",
  "## Detailed resource and phase reports",
  "",
  "CPU/PSS/RSS curves and raw 250 ms process samples are retained separately for every case. Browser fixtures measure the SDK, not Console. Lifecycle counts persisted issuance; its window is not issuance latency. Native uploads include persisted read-back verification on both sides.",
  "",
);
for (const run of rows) {
  lines.push(
    `- [Repeat ${run.repeat}: ${run.case}](${run.report}) — ${
      run.success ? "passed" : "FAILED; see sibling log and raw failures"
    }`,
  );
}
await Deno.writeTextFile(`${output}/report.md`, lines.join("\n") + "\n");
console.log(`Suite report: ${output}/report.md`);
if (rows.length !== expectedRuns || rows.some((run) => !run.success)) {
  Deno.exitCode = 1;
}
