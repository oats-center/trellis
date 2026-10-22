import { readdir, readFile, writeFile } from "node:fs/promises";
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
  httpChunkBytes: z.number().optional(),
  httpTls: z.boolean().default(false),
});
const results = resultsSchema.parse(
  JSON.parse(await readFile(`${input}/samples.json`, "utf8")),
);
const metadata = metadataSchema.parse(
  JSON.parse(await readFile(`${input}/metadata.json`, "utf8")),
);
const bencher: Record<string, Record<string, { value: number }>> = {};
/** Processes whose CPU is charged to each transport's server side. */
const serverRoles: Record<string, string[]> = {
  trellis: ["provider", "nats", "trellis-server"],
  http: ["http-provider", "nats"],
};
const lines = [
  `# Trellis performance — ${metadata.revision}`,
  "",
  metadata.httpTls
    ? `**HTTP: mutual TLS plus per-request Ed25519 signatures.** ${metadata.security}.`
    : "**Baseline: plaintext HTTP, no TLS, no authentication, and no authorization.** This is a lower bound, not a security-equivalent comparison.",
  `Storage: ${metadata.storage}.`,
  metadata.httpChunkBytes
    ? `HTTP body chunk policy: **${metadata.httpChunkBytes} bytes** in both clients and servers.`
    : "",
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
  "## Trellis vs HTTP — same-named workloads",
  "",
  "Ratios compare the same application workload within this language. Transfers are compared in the workload matrix below. The complete attempts and failures remain below.",
  "",
  "| Workload | Trellis median ms | HTTP median ms | Trellis / HTTP | Trellis p95 ms | HTTP p95 ms |",
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
  "`upload-persisted` includes staging, the handler's second store write, read-back verification and durable Operation completion. `upload-staged` completes with the runtime-verified staged digest. Committed is the caller's authenticated Transfer success; source exhausted is NOT delivery or durability. Verified is when the receiver checked size and digest; durable is when the required durable write completed. HTTP raw closes its file without fsync.",
  "",
  "| Workload | MiB | Total MiB/s | Source exhausted ms | Transfer committed ms | Verified ms | Durable ms | Total ms |",
  "|---|---:|---:|---:|---:|---:|---:|---:|",
);
for (const row of results.summary) {
  const bytes = Number(row.name.split("/").at(-2));
  if (!bytes || row.medianMs === null) continue;
  lines.push(
    `| ${row.name} | ${(bytes / 1048576).toFixed(2)} | ${(bytes / 1048576 / (row.medianMs / 1000)).toFixed(2)} | ${row.phases.payloadSuppliedMs?.toFixed(3) ?? "—"} | ${row.phases.committedMs?.toFixed(3) ?? "—"} | ${row.phases.verifiedMs?.toFixed(3) ?? "—"} | ${row.phases.durableMs?.toFixed(3) ?? "—"} | ${row.medianMs.toFixed(3)} |`,
  );
}
// Server CPU per GiB moved, from exact per-attempt resource windows.
const cpuPerGiB = new Map<string, number>();
for (const [name, group] of Map.groupBy(
  results.windows.filter((window) => window.bytes),
  (window) =>
    `${window.transport}/${window.scenario}/${window.bytes ?? 0}/${window.sessions ?? 1}`,
)) {
  let cpuMs = 0;
  let bytes = 0;
  for (const window of group) {
    bytes += window.bytes! * window.calls;
    for (const after of window.after) {
      if (!serverRoles[window.transport]?.includes(after.role)) continue;
      const before = window.before.find(
        (entry) =>
          entry.pid === after.pid && entry.startTicks === after.startTicks,
      );
      if (!before) continue;
      cpuMs +=
        1000 *
        (after.userCpuSeconds -
          before.userCpuSeconds +
          (after.systemCpuSeconds - before.systemCpuSeconds));
    }
  }
  if (bytes) cpuPerGiB.set(name, cpuMs / 1000 / (bytes / 2 ** 30));
}
const categories = [
  {
    title: "Stream → stream",
    note: "No storage. Trellis `upload-stream` hashes and discards in a `stream` Operation handler; `download-stream` serves generated bytes with declared metadata. Raw NATS is an unsigned transport lower bound.",
    scenarios: ["download-stream", "upload-stream"],
    raw: true,
  },
  {
    title: "Stream → store",
    note: "One durable NATS Object Store write or read. Trellis download reads the provider store; `upload-staged` completes at the staged commit; `upload-stream-store` writes a `stream` Operation's bytes once into the service store; backend rows are direct Object Store calls.",
    scenarios: [
      "download",
      "upload-staged",
      "upload-stream-store",
      "download-store",
      "upload-store",
      "backend-write",
      "backend-read",
    ],
    raw: false,
  },
  {
    title: "Complete application workflow",
    note: "Persist, then read back and verify. Trellis also stages before the handler's own store write; HTTP raw uses a local file without fsync.",
    scenarios: ["upload-persisted", "upload-raw", "download-raw"],
    raw: false,
  },
];
const parsed = results.summary.flatMap((row) => {
  const parts = row.name.split("/");
  const bytes = Number(parts.at(-2));
  if (!bytes || row.medianMs === null || row.errors) return [];
  return [
    {
      row,
      transport: parts[0],
      scenario: parts
        .slice(1, -2)
        .join("/")
        .replace(/^rust\//, ""),
      bytes,
      rate: bytes / 1048576 / (row.medianMs / 1000),
    },
  ];
});
lines.push(
  "",
  "## Workload matrix",
  "",
  "Rates use median elapsed time. CPU is server-side core-seconds per GiB from exact attempt windows. `× raw NATS` divides the fastest raw Core NATS stream rate at the same size by this row's rate.",
);
for (const category of categories) {
  lines.push(
    "",
    `### ${category.title}`,
    "",
    category.note,
    "",
    "| Workload | MiB | MiB/s | Verified ms | Durable ms | Server CPU s/GiB | × raw NATS |",
    "|---|---:|---:|---:|---:|---:|---:|",
  );
  for (const entry of parsed) {
    const inCategory =
      category.scenarios.includes(entry.scenario) ||
      (category.raw && entry.transport === "nats");
    if (!inCategory) continue;
    const raw = Math.max(
      0,
      ...parsed
        .filter(
          (candidate) =>
            candidate.transport === "nats" && candidate.bytes === entry.bytes,
        )
        .map((candidate) => candidate.rate),
    );
    lines.push(
      `| ${entry.row.name} | ${(entry.bytes / 1048576).toFixed(2)} | ${entry.rate.toFixed(2)} | ${entry.row.phases.verifiedMs?.toFixed(1) ?? "—"} | ${entry.row.phases.durableMs?.toFixed(1) ?? "—"} | ${cpuPerGiB.get(entry.row.name)?.toFixed(2) ?? "—"} | ${raw ? (raw / entry.rate).toFixed(2) + "×" : "—"} |`,
    );
  }
}
// Protocol byte overhead from production counters, cumulative over the run.
const counterRow = z.object({
  name: z.string(),
  attributes: z.record(z.string(), z.unknown()),
  value: z.number(),
});
const counters = new Map<string, number>();
for (const file of (await readdir(input)).filter((name) =>
  /^metrics-(provider|client)(-\d+)?\.json$/.test(name),
)) {
  const json: unknown = JSON.parse(await readFile(`${input}/${file}`, "utf8"));
  const rows = Array.isArray(json) ? json : [];
  const flattened = rows.flatMap((entry: unknown) => {
    // Rust rows are flat; TypeScript rows are OpenTelemetry scope metrics.
    const rust = counterRow.safeParse(entry);
    if (rust.success) return [rust.data];
    const scope = z
      .object({
        metrics: z.array(
          z.object({
            descriptor: z.object({ name: z.string() }),
            dataPoints: z.array(
              z.object({
                attributes: z.record(z.string(), z.unknown()),
                value: z.unknown(),
              }),
            ),
          }),
        ),
      })
      .safeParse(entry);
    if (!scope.success) return [];
    return scope.data.metrics.flatMap((metric) =>
      metric.dataPoints.flatMap((point) =>
        typeof point.value === "number"
          ? [
              {
                name: metric.descriptor.name,
                attributes: point.attributes,
                value: point.value,
              },
            ]
          : [],
      ),
    );
  });
  for (const point of flattened) {
    if (!point.name.startsWith("trellis.transfer.")) continue;
    const key = `${point.attributes["trellis.side"]}/${point.attributes["trellis.direction"]}/${point.name}`;
    counters.set(key, (counters.get(key) ?? 0) + point.value);
  }
}
if (counters.size) {
  lines.push(
    "",
    "## Transfer protocol byte overhead",
    "",
    "Cumulative production counters over the whole run, warmups included. Wire is serialized DATA (NATS header block plus payload); control is every other Transfer message. Overhead = (wire + control − payload) / payload. Broker protocol lines and TCP framing are excluded.",
    "",
    "| Endpoint / direction | Payload MiB | Wire MiB | Control KiB | Frames | Overhead |",
    "|---|---:|---:|---:|---:|---:|",
  );
  for (const side of ["client", "provider"]) {
    for (const direction of ["upload", "download"]) {
      const get = (name: string) =>
        counters.get(`${side}/${direction}/trellis.transfer.${name}`) ?? 0;
      const payload = get("payload.bytes");
      if (!payload) continue;
      const wire = get("wire.bytes");
      const control = get("control.bytes");
      lines.push(
        `| ${side} / ${direction} | ${(payload / 1048576).toFixed(2)} | ${(wire / 1048576).toFixed(2)} | ${(control / 1024).toFixed(1)} | ${get("frames")} | ${((100 * (wire + control - payload)) / payload).toFixed(3)}% |`,
      );
    }
  }
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
  "CPU is per **attempt**, with failure counts above. Quantization is up to two CPU ticks per process/window; short windows are noisy. Server totals include provider + NATS + Trellis for Trellis, and HTTP provider + NATS for HTTP (store mode writes through NATS).",
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
      if (serverRoles[window.transport]?.includes(after.role)) {
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
