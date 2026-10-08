import { resolve } from "@std/path";
import { z } from "zod";
import { summarize } from "./model.ts";

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
  samples: z.array(z.object({
    scenario: z.string(),
    transport: z.enum(["trellis", "http", "store", "nats"]),
    startedUnixMs: z.number(),
    durationMs: z.number(),
    warmup: z.boolean().optional(),
    phaseIndex: z.number().int().nonnegative().optional(),
    error: z.string().optional(),
    loadGeneratorDrop: z.boolean().optional(),
  })),
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
      startedUnixMs: z.number().optional(),
      phaseIndex: z.number().int().nonnegative().optional(),
      warmup: z.boolean().optional(),
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
  warmups: z.number().default(0),
  sizes: z.array(z.number()),
  sessionCounts: z.array(z.number()),
  idleSeconds: z.number(),
  cpuTicks: z.number(),
  arrivalRate: z.number(),
  maxOutstanding: z.number(),
  lane: z.string().default("typescript"),
  providerCount: z.number().default(1),
  providerLanguage: z.enum(["typescript", "rust"]).default("typescript"),
  clientLanguage: z.enum(["typescript", "rust"]).optional(),
  verificationWorkers: z.boolean().optional(),
  arrivalRates: z.array(z.number().positive()).optional(),
  maxVerificationWorkers: z.number().int().positive().optional(),
  rpcDelayMs: z.number().default(0),
  rpcValueBytes: z.number().default(0),
  requestLimit: z.number().optional(),
  requestByteLimit: z.number().optional(),
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
  metadata.lane === "admission"
    ? "Admission uses authenticated public Trellis clients and real NATS. No HTTP comparison is run in this lane."
    : "**Baseline: plaintext HTTP, no TLS, no authentication, and no authorization.** This is a lower bound, not a security-equivalent comparison. Transfer storage differs: Trellis uses JetStream; HTTP uses filesystem storage.",
  `Lane: **${metadata.lane}**; provider replicas: **${metadata.providerCount}**.`,
  `Warmup rounds: **${metadata.warmups}**, retained in raw samples but excluded from percentiles.`,
  metadata.lane === "transfer"
    ? "Store diagnostics stay runtime-private. Backend write excludes read-back verification; backend read includes hashing. Raw NATS is an unsigned transport lower bound on the same broker, with fixed diagnostic-only windows, not a Trellis protocol or security equivalent. Upload includes staging, the handler's second persisted store write/read-back, and Operation completion; subtracting backend write does not isolate final Operation commit."
    : "",
  metadata.lane === "browser"
    ? "Browser measurements use the generated SDK in Chromium over real WebSocket; they are not Console UI startup. Bundle/document caching is disabled to make delivery cost explicit."
    : metadata.lane === "lifecycle"
    ? "Refresh is proven by persisted contexts for the exact login session. The observation-window duration is NOT refresh issuance latency. Outages interrupt both clients and providers at the advertised TCP endpoint, not the broker/control plane."
    : metadata.lane === "rust"
    ? "Rust uses generated SDK endpoints, Axum and reqwest. Login is prepared outside measurement; first-process timing measures Rust session resume. Echo, transfer and cancellation handlers are hosted, unlike the broader TypeScript fixture."
    : "",
  "",
  ...(metadata.lane === "admission" ? [] : [
    "## Trellis vs HTTP — matched workloads",
    "",
    "**HTTP baseline: plaintext, no TLS, no authentication, no authorization.** Ratios compare the same application workload and byte count within this language and case, not equivalent security or durability. The complete attempts and failures remain below.",
    "",
    "| Workload | Trellis median ms | Plain HTTP (no auth) median ms | Trellis / HTTP | Trellis p95 ms | Plain HTTP (no auth) p95 ms |",
    "|---|---:|---:|---:|---:|---:|",
  ]),
];
if (metadata.lane === "admission") {
  const attributes = z.array(
    z.object({
      key: z.string(),
      value: z.object({ stringValue: z.string().optional() }),
    }),
  );
  const record = z.object({
    metrics: z.object({
      resourceMetrics: z.array(z.object({
        resource: z.object({ attributes }).optional(),
        scopeMetrics: z.array(z.object({
          metrics: z.array(z.object({
            name: z.string(),
            histogram: z.object({
              aggregationTemporality: z.number(),
              dataPoints: z.array(z.object({
                attributes,
                count: z.coerce.number(),
                sum: z.number(),
                max: z.number().optional(),
                startTimeUnixNano: z.string(),
                timeUnixNano: z.string(),
              })),
            }).optional(),
            sum: z.object({
              dataPoints: z.array(
                z.object({
                  attributes,
                  asInt: z.coerce.number().optional(),
                  asDouble: z.number().optional(),
                  timeUnixNano: z.string().optional(),
                }),
              ),
            }).optional(),
          })),
        })),
      })),
    }),
  });
  const peaks = new Map<string, number>();
  const admission = new Map<string, number>();
  const workerCounts = new Map<string, number>();
  const workerChanges: {
    service: string;
    kind: string;
    at: number;
    value: number;
  }[] = [];
  const verification = new Map<string, {
    service: string;
    kind: string;
    phase: string;
    count: number;
    sum: number;
    max?: number;
    at: bigint;
  }>();
  for (
    const line of (await Deno.readTextFile(`${input}/metrics.jsonl`)).trim()
      .split("\n")
  ) {
    for (
      const resource of record.parse(JSON.parse(line)).metrics.resourceMetrics
    ) {
      const service = resource.resource?.attributes.find((attr) =>
        attr.key === "service.name"
      )?.value.stringValue ?? "unknown";
      for (const scope of resource.scopeMetrics) {
        for (const metric of scope.metrics) {
          if (
            metric.name === "trellis.auth.verification.duration" &&
            metric.histogram?.aggregationTemporality === 2
          ) {
            for (const point of metric.histogram.dataPoints) {
              const phase = point.attributes.find((a) =>
                a.key === "trellis.phase"
              )?.value.stringValue ?? "";
              if (
                !phase.startsWith("worker.") && !phase.startsWith("inline.")
              ) {
                continue;
              }
              const kind = point.attributes.find((a) =>
                a.key === "trellis.kind"
              )?.value.stringValue ?? "unknown";
              const instance = resource.resource?.attributes.find((a) =>
                a.key === "service.instance.id"
              )?.value.stringValue ?? "unknown";
              const key =
                `${service}|${instance}|${point.startTimeUnixNano}|${kind}|${phase}`;
              const at = BigInt(point.timeUnixNano);
              if (at >= (verification.get(key)?.at ?? 0n)) {
                verification.set(key, {
                  service,
                  kind,
                  phase,
                  count: point.count,
                  sum: point.sum,
                  max: point.max,
                  at,
                });
              }
            }
          }
          if (
            metric.name.startsWith("trellis.service.admission.") ||
            metric.name.startsWith("trellis.auth.worker.")
          ) {
            for (const point of metric.sum?.dataPoints ?? []) {
              const dimensions = point.attributes.map((attribute) =>
                `${attribute.key}=${attribute.value.stringValue ?? "unknown"}`
              ).sort().join(", ");
              const key = `${service} | ${metric.name} | ${dimensions}`;
              if (
                metric.name === "trellis.auth.worker.active" &&
                point.timeUnixNano
              ) {
                const kind = point.attributes.find((a) =>
                  a.key === "trellis.kind"
                )?.value.stringValue ?? "unknown";
                const instance = resource.resource?.attributes.find((a) =>
                  a.key === "service.instance.id"
                )?.value.stringValue ?? "unknown";
                const workerKey = `${key}|${instance}`;
                const value = point.asInt ?? point.asDouble ?? 0;
                if (workerCounts.get(workerKey) !== value) {
                  workerCounts.set(workerKey, value);
                  workerChanges.push({
                    service,
                    kind,
                    value,
                    at: Number(BigInt(point.timeUnixNano) / 1_000_000n),
                  });
                }
              }
              admission.set(
                key,
                Math.max(
                  admission.get(key) ?? 0,
                  point.asInt ?? point.asDouble ?? 0,
                ),
              );
            }
          }
          if (metric.name !== "trellis.rpc.server.inflight") {
            continue;
          }
          for (const point of metric.sum?.dataPoints ?? []) {
            const route = point.attributes.find((attr) =>
              attr.key === "trellis.route"
            )?.value.stringValue ?? "unknown";
            const key = `${service} | ${route}`;
            peaks.set(
              key,
              Math.max(peaks.get(key) ?? 0, point.asInt ?? point.asDouble ?? 0),
            );
          }
        }
      }
    }
  }
  if (verification.size) {
    lines.push(
      "",
      "### Verification phase timings",
      "",
      "Final exported cumulative histograms per process/lifetime; averages describe recorded samples, not all offered requests. Worker execution contains WASM verification. Boundary time includes serialization and event-loop scheduling. Queue samples exclude requests whose scheduling timed out before dispatch; postchecks describe successful verification. Do not add overlapping phases or maxima.",
      "",
      "| Service | Lane | Phase | Samples | Mean ms | Maximum ms |",
      "|---|---|---|---:|---:|---:|",
    );
    for (
      const [, point] of [...verification.entries()].sort(([a], [b]) =>
        a.localeCompare(b)
      )
    ) {
      if (!point.count) continue;
      lines.push(
        `| ${point.service} | ${point.kind} | ${point.phase} | ${point.count} | ${
          (point.sum * 1000 / point.count).toFixed(3)
        } | ${
          point.max === undefined
            ? "not exported"
            : (point.max * 1000).toFixed(3)
        } |`,
      );
    }
  }
  lines.push(
    "",
    "## Admission load and production telemetry",
    "",
    `Provider: ${metadata.providerLanguage}; handler delay: ${metadata.rpcDelayMs} ms; appended ASCII value: ${metadata.rpcValueBytes} bytes; offered rate: ${
      metadata.arrivalRates?.length
        ? metadata.arrivalRates.join(" → ")
        : metadata.arrivalRate
    }/s; load-generator cap: ${metadata.maxOutstanding}.`,
    `Configured dispatch limits: ${
      metadata.requestLimit ?? "not recorded (historical run)"
    } requests; ${
      metadata.requestByteLimit ?? "not recorded (historical run)"
    } inbound bytes.`,
    `Caller: ${
      metadata.clientLanguage ?? "not recorded (historical run)"
    }; worker verification: ${
      metadata.verificationWorkers ?? "not recorded (historical run)"
    }; maximum ordinary workers: ${
      metadata.maxVerificationWorkers ?? "not recorded (historical run)"
    }. Cancellation is started on a confirmed running operation halfway through the offered burst. Failures and generator drops remain in the attempt table. No HTTP security-equivalence comparison is made for this lane.`,
    "Production OTLP exports are retained in metrics.jsonl at a requested 100 ms interval. Sampled maxima are lower bounds, not exact peaks. The existing in-flight metric counts dispatched unary RPCs, not messages waiting in the client subscription queue.",
    "",
    "| Service | Registered route | Sampled maximum in-flight RPCs |",
    "|---|---|---:|",
  );
  for (const [key, peak] of peaks) lines.push(`| ${key} | ${peak} |`);
  if (!peaks.size) {
    lines.push(
      "No in-flight points were exported; this is a measurement gap, not zero concurrency.",
    );
  }
  lines.push(
    "",
    "### Shared service admission and verifier pool",
    "",
    "In-flight and byte values are sampled occupancy maxima. Rejections are cumulative counter maxima per service and dimension set; they are not RPC timeouts. Transport buffering and decoded heap are excluded from the byte limit.",
    "",
    "| Service | Metric | Dimensions | Maximum exported value |",
    "|---|---|---|---:|",
  );
  for (const [key, maximum] of admission) lines.push(`| ${key} | ${maximum} |`);
  const firstPhaseStart = results.windows.find((w) =>
    w.phaseIndex === 0 && !w.warmup
  )?.startedUnixMs;
  if (firstPhaseStart !== undefined && workerChanges.length) {
    lines.push(
      "",
      "### Observed verifier-count changes",
      "",
      "Times are metric export timestamps relative to the first phase, not exact worker-start instants. Zero at teardown is shutdown, not automatic shrinking.",
      "",
      "| Service | Kind | Milliseconds from first phase | Ready workers |",
      "|---|---|---:|---:|",
    );
    for (const change of workerChanges.sort((a, b) => a.at - b.at)) {
      lines.push(
        `| ${change.service} | ${change.kind} | ${
          change.at - firstPhaseStart
        } | ${change.value} |`,
      );
    }
  }
  if (!admission.size) {
    lines.push(
      "No admission metrics were exported; this is a measurement gap, not zero occupancy or zero refusals.",
    );
  }
  if (metadata.arrivalRates?.length) {
    lines.push(
      "",
      "### Native caller phases on the same service",
      "",
      "Each phase drains its sent RPCs and finishes cancellation before the next begins; the provider and verifier pool remain alive. This is a stepped workload, not an instantaneous rate change with unfinished requests crossing phases.",
      "",
      "| Phase | Offered /s | Completed | Sent failures | Unsent | Median / p95 ms | Cancellation median ms / errors |",
      "|---|---:|---:|---:|---:|---:|---:|",
    );
    for (const [phase, rate] of metadata.arrivalRates.entries()) {
      const rows = summarize(
        results.samples.filter((row) =>
          row.phaseIndex === phase && !row.warmup
        ),
      );
      const echo = rows.find((row) => row.name.startsWith("trellis/echo/"));
      const cancel = rows.find((row) =>
        row.name.startsWith("trellis/admission-cancel/")
      );
      lines.push(
        `| ${phase + 1} | ${rate} | ${
          echo ? echo.attempts - echo.errors : "not recorded"
        } | ${echo ? echo.errors - echo.drops : "not recorded"} | ${
          echo?.drops ?? "not recorded"
        } | ${echo?.medianMs?.toFixed(3) ?? "—"} / ${
          echo?.p95Ms?.toFixed(3) ?? "—"
        } | ${cancel?.medianMs?.toFixed(3) ?? "—"} / ${
          cancel?.errors ?? "not recorded"
        } |`,
      );
    }
  }
}
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
  "Latency percentiles describe successful attempts; failed attempts and generator drops remain in the counts and raw samples.",
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
if (metadata.lane === "admission") {
  lines.push(
    "",
    "## Admission process memory",
    "",
    "Sampled maxima during the offered-load windows, not exact allocation peaks or queue-byte measurements. Whole-process PSS includes caches, parsed requests and replies. Baseline is the last idle sample before the load starts; maxima for PSS and RSS may occur at different times.",
    "",
    "| Process / PID | Baseline PSS MiB | Maximum sampled PSS MiB | Maximum sampled RSS MiB |",
    "|---|---:|---:|---:|",
  );
  const baseline = timeline.findLast((row) => row.phase === "baseline-idle");
  const processes = timeline.filter((row) =>
    row.phase === "trellis/echo-window"
  )
    .flatMap((row) => row.processes)
    .filter((process) =>
      ["provider", "load-generator", "nats", "trellis-server"].includes(
        process.role,
      )
    );
  for (
    const [name, samples] of Map.groupBy(
      processes,
      (process) => `${process.role} / ${process.pid}`,
    )
  ) {
    const before = baseline?.processes.find((process) =>
      process.pid === samples[0].pid &&
      process.startTicks === samples[0].startTicks
    );
    lines.push(
      `| ${name} | ${before ? (before.PssKiB / 1024).toFixed(2) : "—"} | ${
        (Math.max(...samples.map((process) => process.PssKiB)) / 1024).toFixed(
          2,
        )
      } | ${
        (Math.max(...samples.map((process) => process.RssKiB)) / 1024).toFixed(
          2,
        )
      } |`,
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
      "warmups",
      "lane",
      "providerCount",
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
    `Candidate source: **${metadata.revision}**; reference source: **${oldMetadata.revision}**. These are whole-build observations, not transfer-only causal attribution. Inspect each run's metadata and source.diff; unrelated upstream changes may affect the comparison.`,
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
