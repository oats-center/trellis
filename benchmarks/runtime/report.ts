import { resolve } from "@std/path";
import { z } from "zod";

const input = resolve(z.string().min(1).parse(Deno.args[0]));
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
  deno: z.object({ deno: z.string(), v8: z.string() }),
  calls: z.number(),
  samples: z.number(),
  sizes: z.array(z.number()),
  sessionCounts: z.array(z.number()),
  idleSeconds: z.number(),
  cpuTicks: z.number(),
  arrivalRate: z.number(),
  maxOutstanding: z.number(),
  lane: z.string().default("typescript"),
  providerCount: z.number().default(1),
  passwordHashParameters: z.array(z.string()),
  cpuModel: z.string(),
  logicalCpus: z.number().nullable(),
  cgroup: z.record(z.string(), z.string().nullable()),
  topology: z.string(),
  security: z.string(),
  storage: z.string(),
  telemetry: z.object({ traces: z.string(), endpointConfigured: z.boolean() }),
});
const results = resultsSchema.parse(
  JSON.parse(await Deno.readTextFile(`${input}/samples.json`)),
);
const metadata = metadataSchema.parse(
  JSON.parse(await Deno.readTextFile(`${input}/metadata.json`)),
);
const bencher: Record<string, Record<string, { value: number }>> = {};
const lines = [
  `# Trellis performance — ${metadata.revision}`,
  "",
  "HTTP is the unauthenticated lower bound, not an equivalent security stack. Transfer storage differs: JetStream vs filesystem.",
  `Lane: **${metadata.lane}**; provider replicas: **${metadata.providerCount}**.`,
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
  "HTTP is an unauthenticated reference, not an equivalent authorization or durability stack. Ratios below compare the same workload and byte count within this language and case. The complete attempts and failures remain below.",
  "",
  "| Workload | Trellis median ms | HTTP median ms | Trellis / HTTP | Trellis p95 ms | HTTP p95 ms |",
  "|---|---:|---:|---:|---:|---:|",
];
for (const row of results.summary) {
  if (!row.name.startsWith("trellis/")) continue;
  const reference = results.summary.find((candidate) =>
    candidate.name === row.name.replace(/^trellis\//, "http/")
  );
  if (!reference) continue;
  lines.push(
    `| ${row.name.slice("trellis/".length)} | ${
      row.medianMs?.toFixed(3) ?? "—"
    } | ${reference.medianMs?.toFixed(3) ?? "—"} | ${
      row.medianMs !== null && reference.medianMs !== null &&
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
const browserRows = results.summary.filter((row) =>
  row.phases.navigationReadyMs !== null &&
  row.phases.navigationReadyMs !== undefined
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
      `| ${row.name} | ${
        [
          "connectMs",
          "firstRpcMs",
          "setupMs",
          "firstByteMs",
          "cancellationMs",
          "schedulerDelayMs",
        ].map((key) => row.phases[key]?.toFixed(3) ?? "—").join(" | ")
      } |`,
    );
  }
}
lines.push(
  "",
  "## Exact workload CPU windows",
  "",
  "CPU is per **attempt**, with failure counts above. Quantization is up to two CPU ticks per process/window; short windows are noisy. Server totals include provider + NATS + Trellis for Trellis, and HTTP provider for HTTP.",
  "",
  "| Workload / process | Attempts | CPU ms / attempt | Peak endpoint PSS MiB |",
  "|---|---:|---:|---:|",
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
        value: 1000 * group.reduce((sum, window) => sum + window.calls, 0) /
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
      const before = window.before.find((entry) =>
        entry.pid === after.pid && entry.startTicks === after.startTicks
      );
      if (!before) {
        throw new Error(
          `Process identity changed inside ${name}; cannot attribute CPU`,
        );
      }
      const cpuMs = 1000 *
        ((after.userCpuSeconds - before.userCpuSeconds) +
          (after.systemCpuSeconds - before.systemCpuSeconds));
      if (cpuMs < 0) throw new Error(`CPU counters decreased inside ${name}`);
      const current = roles.get(after.role) ??
        { calls: 0, cpuMs: 0, pssKiB: 0 };
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
    const total = roles.get("combined-server") ??
      { calls: 0, cpuMs: 0, pssKiB: 0 };
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
      `| ${name} / ${role} | ${value.calls} | ${
        (value.cpuMs / value.calls).toFixed(4)
      } | ${(value.pssKiB / 1024).toFixed(2)} |`,
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
  processes: z.array(z.object({
    pid: z.number(),
    startTicks: z.number(),
    role: z.string(),
    userCpuSeconds: z.number(),
    systemCpuSeconds: z.number(),
    RssKiB: z.number(),
    PssKiB: z.number(),
    threads: z.number(),
    fds: z.number(),
  })),
});
const timeline = (await Deno.readTextFile(`${input}/resources.jsonl`)).trim()
  .split("\n").map((line) => timelineRow.parse(JSON.parse(line)));
lines.push(
  "",
  "## Idle session resource curve",
  "",
  "Whole-process measurements, not per-session allocation estimates. Baseline precedes workloads; after-disconnect includes closing the cohort plus observation, retains durable login sessions and allocator caches, and is not steady-state idle. CPU is percent of one core.",
  "",
  "| Phase / process | Observed seconds | CPU % | Endpoint PSS MiB | Endpoint RSS MiB | Threads | File descriptors |",
  "|---|---:|---:|---:|---:|---:|---:|",
);
for (
  const [phase, rows] of Map.groupBy(
    timeline.filter((row) =>
      row.phase === "baseline-idle" ||
      row.phase === "trellis/after-disconnect" ||
      row.phase.startsWith("trellis/idle-sessions/")
    ),
    (row) => row.phase,
  )
) {
  const first = rows[0];
  const last = rows.at(-1)!;
  const seconds = (last.unixMs - first.unixMs) / 1000;
  if (seconds <= 0) continue;
  for (
    const after of last.processes.filter((process) =>
      ["nats", "trellis-server", "provider", "http-provider", "load-generator"]
        .includes(process.role)
    )
  ) {
    const before = first.processes.find((process) =>
      process.pid === after.pid && process.startTicks === after.startTicks
    );
    if (!before) continue;
    const cpuPercent = 100 *
      ((after.userCpuSeconds - before.userCpuSeconds) +
        (after.systemCpuSeconds - before.systemCpuSeconds)) /
      seconds;
    lines.push(
      `| ${phase} / ${after.role} | ${seconds.toFixed(2)} | ${
        cpuPercent.toFixed(2)
      } | ${(after.PssKiB / 1024).toFixed(2)} | ${
        (after.RssKiB / 1024).toFixed(2)
      } | ${after.threads} | ${after.fds} |`,
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
if (Deno.args[1]) {
  const baseline = resolve(Deno.args[1]);
  const oldMetadata = metadataSchema.parse(
    JSON.parse(await Deno.readTextFile(`${baseline}/metadata.json`)),
  );
  for (
    const key of [
      "host",
      "deno",
      "calls",
      "samples",
      "sizes",
      "sessionCounts",
      "idleSeconds",
      "cpuTicks",
      "arrivalRate",
      "maxOutstanding",
      "passwordHashParameters",
      "cpuModel",
      "logicalCpus",
      "cgroup",
      "topology",
      "security",
      "storage",
      "telemetry",
    ] as const
  ) {
    if (JSON.stringify(metadata[key]) !== JSON.stringify(oldMetadata[key])) {
      throw new Error(
        `Cannot compare mismatched ${key}; run matched configurations`,
      );
    }
  }
  const oldResults = resultsSchema.parse(
    JSON.parse(await Deno.readTextFile(`${baseline}/samples.json`)),
  );
  lines.push(
    "",
    `## Compared with ${oldMetadata.revision}`,
    "",
    "| Workload | Median change |",
    "|---|---:|",
  );
  for (const row of results.summary) {
    const old = oldResults.summary.find((entry) => entry.name === row.name);
    if (
      !old || row.errors || old.errors || row.medianMs === null ||
      old.medianMs === null || old.medianMs === 0
    ) {
      lines.push(`| ${row.name} | Not comparable: missing samples or errors |`);
    } else {lines.push(
        `| ${row.name} | ${
          ((row.medianMs / old.medianMs - 1) * 100).toFixed(1)
        }% |`,
      );}
  }
}
await Deno.writeTextFile(
  `${input}/bencher.json`,
  JSON.stringify(bencher, null, 2),
);
await Deno.writeTextFile(`${input}/report.md`, lines.join("\n") + "\n");
console.log(lines.join("\n"));
if (results.summary.some((row) => row.errors)) Deno.exitCode = 1;
