import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { z } from "zod";

const input = resolve(z.string().min(1).parse(process.argv[2]));
const resource = z.object({
  pid: z.number(),
  startTicks: z.number(),
  role: z.string(),
  userCpuSeconds: z.number(),
  systemCpuSeconds: z.number(),
  rssKiB: z.number(),
  pssKiB: z.number(),
});
const resultsSchema = z.object({
  summary: z.array(
    z.object({
      name: z.string(),
      attempts: z.number(),
      errors: z.number(),
      submitted: z.number(),
      drops: z.number(),
      phases: z.record(z.string(), z.number().nullable()),
      medianMs: z.number().nullable(),
      p95Ms: z.number().nullable(),
      p99Ms: z.number().nullable(),
    }),
  ),
  windows: z.array(
    z.object({
      scenario: z.string(),
      transport: z.string(),
      calls: z.number(),
      bytes: z.number().optional(),
      sessions: z.number().optional(),
      durationMs: z.number(),
      before: z.array(resource),
      after: z.array(resource),
    }),
  ),
});
const metadataSchema = z.object({
  revision: z.string(),
  host: z.string(),
  node: z.object({ node: z.string(), v8: z.string() }),
  calls: z.number(),
  samples: z.number(),
  warmups: z.number().default(0),
  sizes: z.array(z.number()),
  sessionCounts: z.array(z.number()),
  idleSeconds: z.number(),
  cpuTicks: z.number(),
  arrivalRate: z.number(),
  maxOutstanding: z.number(),
  lane: z.string().default("typescript"),
  providerCount: z.number().default(1),
  diagnostics: z.boolean().default(false),
  cpuModel: z.string(),
  logicalCpus: z.number().nullable(),
  cgroup: z.record(z.string(), z.string().nullable()),
  topology: z.string(),
  security: z.string(),
  storage: z.string(),
  telemetry: z.object({ traces: z.string(), endpointConfigured: z.boolean() }),
});
const results = resultsSchema.parse(
  JSON.parse(await readFile(`${input}/samples.json`, "utf8")),
);
const metadata = metadataSchema.parse(
  JSON.parse(await readFile(`${input}/metadata.json`, "utf8")),
);
const bencher: Record<string, Record<string, { value: number }>> = {};
const lines = [
  `# Trellis performance — ${metadata.revision}`,
  "",
  "**Baseline: plaintext HTTP, no TLS, no authentication, and no authorization.** This is a lower bound, not a security-equivalent comparison. Transfer storage differs: Trellis uses JetStream; HTTP uses filesystem storage.",
  `Lane: **${metadata.lane}**; provider replicas: **${metadata.providerCount}**.`,
  `Warmup rounds: **${metadata.warmups}**, retained in raw samples but excluded from percentiles.`,
  metadata.diagnostics
    ? "**Passive broker observer enabled: extra payload copies affect throughput and CPU. Compare diagnostic runs only with matching diagnostic runs.**"
    : "Broker observer disabled for throughput measurement.",
  metadata.lane === "transfer"
    ? "Store diagnostics stay runtime-private. Backend write excludes read-back verification; backend read includes hashing. Raw NATS is an unsigned transport lower bound on the same broker, with fixed diagnostic-only windows, not a Trellis protocol or security equivalent. Upload includes staging, the handler's second persisted store write/read-back, and Operation completion; subtracting backend write does not isolate final Operation commit."
    : "",
  metadata.lane === "browser"
    ? "Browser measurements use the generated SDK in Chromium over real WebSocket; they are not Console UI startup. Bundle/document caching is disabled to make delivery cost explicit."
    : metadata.lane === "lifecycle"
      ? "Refresh is proven by persisted contexts for the exact login session. The observation-window duration is NOT refresh issuance latency. Outages interrupt both clients and providers at the advertised TCP endpoint, not the broker/control plane."
      : metadata.lane === "rust"
        ? "Rust uses generated SDK endpoints, Axum and reqwest. Login is prepared outside measurement; first-process timing measures Rust session resume. Only Echo and transfer handlers are hosted, unlike the broader TypeScript fixture."
        : "",
  "",
  "## Trellis vs HTTP — matched workloads",
  "",
  "**HTTP baseline: plaintext, no TLS, no authentication, no authorization.** Ratios compare the same application workload and byte count within this language and case, not equivalent security or durability. The complete attempts and failures remain below.",
  "",
  "| Workload | Trellis median ms | Plain HTTP (no auth) median ms | Trellis / HTTP | Trellis p95 ms | Plain HTTP (no auth) p95 ms |",
  "|---|---:|---:|---:|---:|---:|",
];
for (const row of results.summary) {
  if (!row.name.startsWith("trellis/")) continue;
  const reference = results.summary.find(
    (candidate) => candidate.name === row.name.replace(/^trellis\//, "http/"),
  );
  if (!reference) continue;
  lines.push(
    `| ${row.name.slice("trellis/".length)} | ${
      row.medianMs?.toFixed(3) ?? "—"
    } | ${reference.medianMs?.toFixed(3) ?? "—"} | ${
      row.medianMs !== null &&
      reference.medianMs !== null &&
      reference.medianMs > 0
        ? (row.medianMs / reference.medianMs).toFixed(2) + "×"
        : "—"
    } | ${row.p95Ms?.toFixed(3) ?? "—"} | ${
      reference.p95Ms?.toFixed(3) ?? "—"
    } |`,
  );
}
lines.push(
  "",
  "## Streaming throughput and completion boundaries",
  "",
  "Upload total includes staging, the second service store write, read-back verification and durable Operation completion. Committed is the caller's authenticated Transfer success; source exhausted is NOT delivery or durability. Download total includes EOF and digest verification. HTTP has no Transfer/Operation commit and does not fsync.",
  "",
  "| Workload | MiB | Total MiB/s | Source exhausted ms | Transfer committed ms | Total ms |",
  "|---|---:|---:|---:|---:|---:|",
);
for (const row of results.summary) {
  const bytes = Number(row.name.split("/").at(-2));
  if (!bytes || row.medianMs === null) continue;
  lines.push(
    `| ${row.name} | ${(bytes / 1048576).toFixed(2)} | ${(bytes / 1048576 / (row.medianMs / 1000)).toFixed(2)} | ${row.phases.payloadSuppliedMs?.toFixed(3) ?? "—"} | ${row.phases.committedMs?.toFixed(3) ?? "—"} | ${row.medianMs.toFixed(3)} |`,
  );
}
lines.push(
  "",
  "## All workloads and failures",
  "",
  "| Workload | Attempts / errors | Median ms | p95 ms | p99 ms |",
  "|---|---:|---:|---:|---:|",
);
for (const row of results.summary) {
  lines.push(
    `| ${row.name} | ${row.attempts} / ${row.errors} | ${
      row.medianMs?.toFixed(3) ?? "—"
    } | ${row.p95Ms?.toFixed(3) ?? "—"} | ${row.p99Ms?.toFixed(3) ?? "—"} |`,
  );
  const measures: Record<string, { value: number }> = {
    errors: { value: row.errors },
  };
  measures.load_generator_drops = { value: row.drops };
  if (!row.errors) {
    if (row.medianMs !== null) measures.latency_ms = { value: row.medianMs };
    if (row.p95Ms !== null) measures.p95_latency_ms = { value: row.p95Ms };
    if (row.p99Ms !== null) measures.p99_latency_ms = { value: row.p99Ms };
    for (const [phase, value] of Object.entries(row.phases)) {
      if (value !== null) {
        measures[
          phase.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`)
        ] = { value };
      }
    }
  }
  bencher[row.name] = measures;
}
const browserRows = results.summary.filter(
  (row) =>
    row.phases.navigationReadyMs !== null &&
    row.phases.navigationReadyMs !== undefined,
);
if (browserRows.length) {
  lines.push(
    "",
    "## Browser navigation (medians)",
    "",
    "| Workload | Document TTFB ms | Navigation → first authorized RPC ms | Transferred bytes |",
    "|---|---:|---:|---:|",
  );
  for (const row of browserRows) {
    lines.push(
      `| ${row.name} | ${row.phases.documentTtfbMs?.toFixed(3) ?? "—"} | ${
        row.phases.navigationReadyMs?.toFixed(3) ?? "—"
      } | ${row.phases.transferredBytes?.toFixed(0) ?? "—"} |`,
    );
  }
}
lines.push(
  "",
  "## Readiness and transfer phases (median ms)",
  "",
  "| Workload | Connect | First RPC | Setup | First payload byte | Cancellation | Scheduler delay |",
  "|---|---:|---:|---:|---:|---:|---:|",
);
for (const row of results.summary) {
  if (Object.values(row.phases).some((value) => value !== null)) {
    lines.push(
      `| ${row.name} | ${[
        "connectMs",
        "firstRpcMs",
        "setupMs",
        "firstByteMs",
        "cancellationMs",
        "schedulerDelayMs",
      ]
        .map((key) => row.phases[key]?.toFixed(3) ?? "—")
        .join(" | ")} |`,
    );
  }
}
lines.push(
  "",
  "## Exact workload CPU windows",
  "",
  "CPU is per **attempt**, with failure counts above. Quantization is up to two CPU ticks per process/window; short windows are noisy. Server totals include provider + NATS + Trellis for Trellis, and HTTP provider for HTTP.",
  "",
  "| Workload / process | Attempts | CPU ms / attempt | CPU % of one core | Peak endpoint PSS MiB |",
  "|---|---:|---:|---:|---:|",
);
const windows = Map.groupBy(
  results.windows,
  (window) =>
    `${window.transport}/${window.scenario}/${window.bytes ?? 0}/${
      window.sessions ?? 1
    }`,
);
for (const [name, group] of windows) {
  if (group[0].scenario === "echo") {
    const completed = results.summary.find((row) => row.name === name);
    if (completed && !completed.errors) {
      bencher[name].completed_calls_per_second = {
        value:
          (1000 * group.reduce((sum, window) => sum + window.calls, 0)) /
          group.reduce((sum, window) => sum + window.durationMs, 0),
      };
    }
  }
  const roles = new Map<
    string,
    { calls: number; cpuMs: number; pssKiB: number }
  >();
  for (const window of group) {
    const countedRoles = new Set<string>();
    const combined = { cpuMs: 0, beforePssKiB: 0, afterPssKiB: 0 };
    for (const after of window.after) {
      const before = window.before.find(
        (entry) =>
          entry.pid === after.pid && entry.startTicks === after.startTicks,
      );
      if (!before) {
        throw new Error(
          `Process identity changed inside ${name}; cannot attribute CPU`,
        );
      }
      const cpuMs =
        1000 *
        (after.userCpuSeconds -
          before.userCpuSeconds +
          (after.systemCpuSeconds - before.systemCpuSeconds));
      if (cpuMs < 0) throw new Error(`CPU counters decreased inside ${name}`);
      const current = roles.get(after.role) ?? {
        calls: 0,
        cpuMs: 0,
        pssKiB: 0,
      };
      if (!countedRoles.has(after.role)) {
        current.calls += window.calls;
        countedRoles.add(after.role);
      }
      current.cpuMs += cpuMs;
      current.pssKiB = Math.max(current.pssKiB, before.pssKiB, after.pssKiB);
      roles.set(after.role, current);
      if (
        (window.transport === "http" && after.role === "http-provider") ||
        (window.transport === "trellis" &&
          ["provider", "nats", "trellis-server"].includes(after.role))
      ) {
        combined.cpuMs += cpuMs;
        combined.beforePssKiB += before.pssKiB;
        combined.afterPssKiB += after.pssKiB;
      }
    }
    if (window.transport === "mixed") continue;
    const total = roles.get("combined-server") ?? {
      calls: 0,
      cpuMs: 0,
      pssKiB: 0,
    };
    total.calls += window.calls;
    total.cpuMs += combined.cpuMs;
    total.pssKiB = Math.max(
      total.pssKiB,
      combined.beforePssKiB,
      combined.afterPssKiB,
    );
    roles.set("combined-server", total);
  }
  for (const [role, value] of roles) {
    lines.push(
      `| ${name} / ${role} | ${value.calls} | ${(
        value.cpuMs / value.calls
      ).toFixed(
        4,
      )} | ${((100 * value.cpuMs) / group.reduce((sum, window) => sum + window.durationMs, 0)).toFixed(2)} | ${(value.pssKiB / 1024).toFixed(2)} |`,
    );
    bencher[`resources/${name}/${role}`] = {
      cpu_ms_per_attempt: { value: Math.max(0, value.cpuMs / value.calls) },
      pss_mib: { value: value.pssKiB / 1024 },
    };
  }
}
const timelineRow = z.object({
  unixMs: z.number(),
  phase: z.string(),
  processes: z.array(
    z.object({
      pid: z.number(),
      startTicks: z.number(),
      role: z.string(),
      userCpuSeconds: z.number(),
      systemCpuSeconds: z.number(),
      RssKiB: z.number(),
      PssKiB: z.number(),
      threads: z.number(),
      fds: z.number(),
    }),
  ),
});
const timeline = (await readFile(`${input}/resources.jsonl`, "utf8"))
  .trim()
  .split("\n")
  .map((line) => timelineRow.parse(JSON.parse(line)));
lines.push(
  "",
  "## Sampled transfer memory peaks",
  "",
  "Whole-process peaks observed at 250 ms intervals; brief peaks may be missed. Allocator caches and earlier setup remain included. Not per-transfer allocation measurements.",
  "",
  "| Phase / process | Peak sampled RSS MiB | Peak sampled PSS MiB |",
  "|---|---:|---:|",
);
for (const [phase, rows] of Map.groupBy(
  timeline.filter((row) => /\/(?:rust\/)?(?:download|upload)/.test(row.phase)),
  (row) => row.phase,
)) {
  for (const [role, processes] of Map.groupBy(
    rows.flatMap((row) => row.processes),
    (process) => process.role,
  )) {
    lines.push(
      `| ${phase} / ${role} | ${(Math.max(...processes.map((process) => process.RssKiB)) / 1024).toFixed(2)} | ${(Math.max(...processes.map((process) => process.PssKiB)) / 1024).toFixed(2)} |`,
    );
  }
}
lines.push(
  "",
  "## Idle session resource curve",
  "",
  "Whole-process measurements, not per-session allocation estimates. Baseline precedes workloads; after-disconnect includes closing the cohort plus observation, retains durable login sessions and allocator caches, and is not steady-state idle. CPU is percent of one core.",
  "",
  "| Phase / process | Observed seconds | CPU % | Endpoint PSS MiB | Endpoint RSS MiB | Threads | File descriptors |",
  "|---|---:|---:|---:|---:|---:|---:|",
);
for (const [phase, rows] of Map.groupBy(
  timeline.filter(
    (row) =>
      row.phase === "baseline-idle" ||
      row.phase === "trellis/after-disconnect" ||
      row.phase.startsWith("trellis/idle-sessions/"),
  ),
  (row) => row.phase,
)) {
  const first = rows[0];
  const last = rows.at(-1)!;
  const seconds = (last.unixMs - first.unixMs) / 1000;
  if (seconds <= 0) continue;
  for (const after of last.processes.filter((process) =>
    [
      "nats",
      "trellis-server",
      "provider",
      "http-provider",
      "load-generator",
    ].includes(process.role),
  )) {
    const before = first.processes.find(
      (process) =>
        process.pid === after.pid && process.startTicks === after.startTicks,
    );
    if (!before) continue;
    const cpuPercent =
      (100 *
        (after.userCpuSeconds -
          before.userCpuSeconds +
          (after.systemCpuSeconds - before.systemCpuSeconds))) /
      seconds;
    lines.push(
      `| ${phase} / ${after.role} | ${seconds.toFixed(2)} | ${cpuPercent.toFixed(
        2,
      )} | ${(after.PssKiB / 1024).toFixed(2)} | ${(
        after.RssKiB / 1024
      ).toFixed(2)} | ${after.threads} | ${after.fds} |`,
    );
    bencher[`idle/${phase}/${after.role}`] = {
      cpu_percent: { value: cpuPercent },
      pss_mib: { value: after.PssKiB / 1024 },
      rss_mib: { value: after.RssKiB / 1024 },
      threads: { value: after.threads },
      file_descriptors: { value: after.fds },
    };
  }
}
if (process.argv[3]) {
  const baseline = resolve(process.argv[3]);
  const oldMetadata = metadataSchema.parse(
    JSON.parse(await readFile(`${baseline}/metadata.json`, "utf8")),
  );
  for (const key of [
    "host",
    "node",
    "calls",
    "samples",
    "warmups",
    "lane",
    "providerCount",
    "diagnostics",
    "sizes",
    "sessionCounts",
    "idleSeconds",
    "cpuTicks",
    "arrivalRate",
    "maxOutstanding",
    "cpuModel",
    "logicalCpus",
    "cgroup",
    "topology",
    "security",
    "storage",
    "telemetry",
  ] as const) {
    if (JSON.stringify(metadata[key]) !== JSON.stringify(oldMetadata[key])) {
      throw new Error(
        `Cannot compare mismatched ${key}; run matched configurations`,
      );
    }
  }
  const oldResults = resultsSchema.parse(
    JSON.parse(await readFile(`${baseline}/samples.json`, "utf8")),
  );
  lines.push(
    "",
    `## Compared with ${oldMetadata.revision}`,
    "",
    `Candidate source: **${metadata.revision}**; reference source: **${oldMetadata.revision}**. These are whole-build observations, not transfer-only causal attribution. Inspect each run's metadata and source.diff; unrelated upstream changes may affect the comparison.`,
    "",
    "| Workload | Median change |",
    "|---|---:|",
  );
  for (const row of results.summary) {
    const old = oldResults.summary.find((entry) => entry.name === row.name);
    if (
      !old ||
      row.errors ||
      old.errors ||
      row.medianMs === null ||
      old.medianMs === null ||
      old.medianMs === 0
    ) {
      lines.push(`| ${row.name} | Not comparable: missing samples or errors |`);
    } else {
      lines.push(
        `| ${row.name} | ${((row.medianMs / old.medianMs - 1) * 100).toFixed(
          1,
        )}% |`,
      );
    }
  }
}
await writeFile(`${input}/bencher.json`, JSON.stringify(bencher, null, 2));
await writeFile(`${input}/report.md`, lines.join("\n") + "\n");
console.log(lines.join("\n"));
if (results.summary.some((row) => row.errors)) process.exitCode = 1;
