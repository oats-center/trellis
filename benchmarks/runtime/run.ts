import { parseArgs } from "@std/cli/parse-args";
import { fromFileUrl, resolve, toFileUrl } from "@std/path";
import { z } from "zod";
import { createClient as openDatabase } from "@libsql/client";
import { type CallerParticipant, TrellisClient } from "@oatscenter/trellis";
import { completeLocalAuthFlow } from "../../ts/packages/trellis-testkit/src/admin/auth_flow.ts";
import { deadline, type Sample, summarize } from "./model.ts";
import { snapshotResources } from "./resources.ts";
import { TrellisTestRuntime } from "../../ts/packages/trellis-testkit/index.ts";
import { participants } from "./packages/performance-trellis/index.js";
import { rawTransfer } from "./raw_transfer.ts";

const root = fromFileUrl(new URL("../../", import.meta.url));
const args = parseArgs(Deno.args, {
  string: [
    "output",
    "server",
    "cli",
    "samples",
    "calls",
    "sizes",
    "sessions",
    "idle-seconds",
    "arrival-rate",
    "max-outstanding",
    "lane",
    "providers",
    "browser-bundle",
    "rust-bin",
    "contract-entry",
    "contract-worker",
    "warmups",
  ],
  boolean: ["keep-workdir"],
  default: {
    samples: "21",
    calls: "1000",
    sizes: "65536,1048576,8388608",
    sessions: "1,10,100",
    "idle-seconds": "5",
    "arrival-rate": "0",
    "max-outstanding": "128",
    lane: "typescript",
    providers: "1",
    warmups: "0",
  },
});
const positive = z.coerce.number().int().positive();
const samples = positive.parse(args.samples);
const calls = positive.parse(args.calls);
const warmups = z.coerce.number().int().nonnegative().parse(args.warmups);
const lane = z.enum([
  "typescript",
  "transfer",
  "lifecycle",
  "browser",
  "rust",
  "contract",
])
  .parse(
    args.lane,
  );
const providerCount = positive.parse(args.providers);
if (lane === "transfer" && providerCount !== 1) {
  throw new Error(
    "The focused transfer audit uses one provider; replica scaling belongs to the full suite",
  );
}
const arrivalRate = z.coerce.number().nonnegative().parse(args["arrival-rate"]);
const maxOutstanding = positive.parse(args["max-outstanding"]);
const sizes = z.array(positive.max(64 * 1024 * 1024)).nonempty().parse(
  args.sizes.split(","),
);
const sessionCounts = z.array(positive).nonempty().parse(
  args.sessions.split(","),
).sort((a, b) => a - b);
const idleSeconds = z.coerce.number().positive().parse(args["idle-seconds"]);
const server = resolve(
  args.server ?? Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    `${root}/target/release/trellis-server`,
);
const cli = resolve(args.cli ?? `${root}/target/release/trellis`);
const output = resolve(
  args.output ??
    `${root}/benchmarks/runtime/results/${
      new Date().toISOString().replaceAll(":", "-")
    }`,
);
const tickCommand = await new Deno.Command("getconf", { args: ["CLK_TCK"] })
  .output();
if (!tickCommand.success) {
  throw new Error("Cannot determine CPU counter resolution");
}
const cpuTicks = positive.parse(
  new TextDecoder().decode(tickCommand.stdout).trim(),
);
if (Deno.build.os !== "linux") {
  throw new Error("Resource sampling currently requires Linux /proc");
}
await Deno.mkdir(output, { recursive: true });
// Exclusive creation prevents accidental reuse of stop markers or old worker results.
const marker = await Deno.open(`${output}/run.lock`, {
  createNew: true,
  write: true,
});
marker.close();
const children: Deno.ChildProcess[] = [];
const logWrites: Promise<void>[] = [];
let runtime: TrellisTestRuntime | undefined;
let sampler: Deno.ChildProcess | undefined;
let browserServer: Deno.HttpServer | undefined;
const browserSamples: Sample[] = [];
const browserSession = `trellis-benchmark-${Deno.pid}`;
async function startWorker(
  file: string,
  options: Record<string, unknown>,
  provider = false,
) {
  const native = file.startsWith("rust");
  const child = new Deno.Command(
    native ? resolve(z.string().parse(args["rust-bin"])) : Deno.execPath(),
    {
      args: native
        ? (provider
          ? ["--provider"]
          : file === "rust-http"
          ? ["--http-provider"]
          : [])
        : [
          "run",
          "--no-check",
          "-A",
          "--config",
          `${root}/ts/deno.json`,
          resolve(root, "benchmarks/runtime", file),
          ...(provider ? ["--provider"] : []),
        ],
      stdin: "piped",
      stdout: "piped",
      stderr: "piped",
    },
  ).spawn();
  children.push(child);
  const label = provider
    ? `provider-${options.providerIndex ?? 0}`
    : file === "http.ts" || file === "rust-http"
    ? "http"
    : "client";
  for (
    const [kind, stream] of [["stdout", child.stdout], [
      "stderr",
      child.stderr,
    ]] as const
  ) {
    const log = await Deno.open(`${output}/${label}.${kind}.log`, {
      createNew: true,
      write: true,
    });
    logWrites.push(stream.pipeTo(log.writable));
  }
  const writer = child.stdin.getWriter();
  await writer.write(new TextEncoder().encode(JSON.stringify(options)));
  await writer.close();
  return child;
}
try {
  const git = await new Deno.Command("git", {
    args: ["rev-parse", "HEAD"],
    cwd: root,
  }).output();
  const diff = await new Deno.Command("git", {
    args: ["diff", "HEAD"],
    cwd: root,
  }).output();
  const snapshotRevision = await Deno.readTextFile(`${root}/.source-revision`)
    .catch((error: unknown) => {
      if (error instanceof Deno.errors.NotFound) return undefined;
      throw error;
    });
  const revision = snapshotRevision === undefined
    ? new TextDecoder().decode(git.stdout).trim()
    : z.string().regex(/^[0-9a-f]{40}$/).parse(snapshotRevision.trim());
  await Deno.writeFile(
    `${output}/source.diff`,
    snapshotRevision === undefined
      ? diff.stdout
      : await Deno.readFile(`${root}/.benchmark-overlay.diff`),
  );
  const hash = await new Deno.Command("sha256sum", { args: [server] }).output();
  const cliHash = await new Deno.Command("sha256sum", { args: [cli] }).output();
  const cpuInfo = await Deno.readTextFile("/proc/cpuinfo");
  const cgroupPath = (await Deno.readTextFile("/proc/self/cgroup")).split("\n")
    .find((line) => line.startsWith("0::"))?.slice(3);
  const cgroup = Object.fromEntries(
    await Promise.all(
      ["cpu.max", "cpuset.cpus.effective", "memory.max"].map(async (
        name,
      ) => [
        name,
        cgroupPath === undefined
          ? null
          : await Deno.readTextFile(`/sys/fs/cgroup${cgroupPath}/${name}`).then(
            (value) => value.trim(),
          ).catch(() => null),
      ]),
    ),
  );
  if (!hash.success || !cliHash.success) {
    throw new Error(
      "Supply an existing matching ordinary release CLI/server pair through --cli and --server; the benchmark does not build them",
    );
  }
  await Deno.writeTextFile(
    `${output}/metadata.json`,
    JSON.stringify(
      {
        revision,
        sourceSnapshot: snapshotRevision !== undefined,
        trackedDiff: "source.diff",
        serverSha256: new TextDecoder().decode(hash.stdout).split(" ")[0],
        cliSha256: new TextDecoder().decode(cliHash.stdout).split(" ")[0],
        startedAt: new Date().toISOString(),
        deno: Deno.version,
        host: Deno.hostname(),
        os: Deno.build,
        samples,
        calls,
        warmups,
        sizes,
        sessionCounts,
        idleSeconds,
        cpuTicks,
        arrivalRate,
        maxOutstanding,
        lane,
        providerCount,
        topology:
          "loopback; native NATS TCP; plaintext HTTP/1.1 keep-alive; no compression or HTTP caching",
        setup:
          "Contracts, resources and consent policy prepared before measurement; backend warm; each fresh-login case uses a distinct key. First-process connect includes lazy SDK initialization, not module parsing; OS caches are not cleared",
        security:
          "Trellis production auth; HTTP unauthenticated lower bound, NOT security equivalent",
        storage:
          "Trellis JetStream object storage; HTTP filesystem. Persistence acknowledgement, NOT equivalent fsync guarantees",
        telemetry: {
          traces: Deno.env.get("OTEL_TRACES_SAMPLER") ?? "SDK defaults",
          endpointConfigured: Boolean(
            Deno.env.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
          ),
        },
        cpuInfo,
        cpuModel: cpuInfo.match(/^model name\s*:\s*(.+)$/m)?.[1] ?? "unknown",
        logicalCpus: cpuInfo.match(/^processor\s*:/gm)?.length ?? null,
        cgroup,
        memInfo: await Deno.readTextFile("/proc/meminfo"),
      },
      null,
      2,
    ),
  );
  sampler = new Deno.Command("python3", {
    args: [`${root}/benchmarks/runtime/resources.py`, String(Deno.pid), output],
    stdout: "inherit",
    stderr: "inherit",
  }).spawn();
  let browserOrigin: string | undefined;
  if (lane === "browser") {
    const bundle = await Deno.readFile(
      z.string().parse(args["browser-bundle"]),
    );
    browserServer = Deno.serve({
      hostname: "127.0.0.1",
      port: 0,
      onListen() {},
    }, async (request) => {
      const url = new URL(request.url);
      if (url.pathname === "/protocol_wasm/trellis_protocol_wasm_bg.wasm") {
        return new Response(
          await Deno.readFile(
            `${root}/ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm_bg.wasm`,
          ),
          {
            headers: {
              "content-type": "application/wasm",
              "cache-control": "no-store",
            },
          },
        );
      }
      if (url.pathname === "/bundle.js") {
        return new Response(bundle, {
          headers: {
            "content-type": "text/javascript",
            "cache-control": "no-store",
          },
        });
      }
      if (url.pathname === "/config") {
        const index = z.coerce.number().int().min(0).max(samples - 1).parse(
          url.searchParams.get("index"),
        );
        return Response.json({
          trellisUrl: runtime!.trellisUrl,
          seed: browserSeeds[index],
          index,
          resume: url.searchParams.get("resume") === "true",
        });
      }
      if (url.pathname === "/login" && request.method === "POST") {
        const body = z.object({ loginUrl: z.url() }).parse(
          await request.json(),
        );
        if (
          new URL(body.loginUrl).origin !== new URL(runtime!.trellisUrl).origin
        ) return new Response("Foreign issuer", { status: 400 });
        return Response.json(
          await completeLocalAuthFlow({
            trellisUrl: runtime!.trellisUrl,
            loginUrl: body.loginUrl,
            password: runtime!.adminPassword,
          }),
        );
      }
      if (url.pathname === "/results" && request.method === "POST") {
        const sample = z.object({
          scenario: z.string(),
          transport: z.literal("trellis"),
          startedUnixMs: z.number(),
          durationMs: z.number(),
          connectMs: z.number().optional(),
          firstRpcMs: z.number().optional(),
          navigationReadyMs: z.number().optional(),
          documentTtfbMs: z.number().optional(),
          transferredBytes: z.number().optional(),
          error: z.string().optional(),
        }).parse(await request.json());
        browserSamples.push(sample);
        return Response.json({ received: true });
      }
      return new Response(
        '<!doctype html><title>Trellis benchmark</title><body>Measuring real browser connection…<script type="module" src="/bundle.js"></script>',
        {
          headers: { "content-type": "text/html", "cache-control": "no-store" },
        },
      );
    });
    if (browserServer.addr.transport !== "tcp") {
      throw new Error("Browser fixture requires TCP");
    }
    browserOrigin = `http://127.0.0.1:${browserServer.addr.port}`;
  }
  runtime = await TrellisTestRuntime.start({
    keepWorkdir: Boolean(args["keep-workdir"]),
    trellis: {
      source: { kind: "path", cli, server },
      mode: "all",
    },
    adminPassword: "benchmark-isolated-password",
    timeouts: { startupMs: 60_000, waitForMs: 30_000 },
    webOrigins: browserOrigin ? [browserOrigin] : [],
    interruptibleNativeProxy: lane === "lifecycle",
    // Ordinary production policy, with a shorter signed window to observe renewal.
    authorization: lane === "lifecycle"
      ? { contextLifetimeSeconds: 120 }
      : undefined,
  });
  await runtime.ensureAdmin();
  const hashing = await new Deno.Command("python3", {
    args: [
      "-c",
      "import sqlite3,json,sys; db=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); rows=db.execute('SELECT DISTINCT password_hash FROM auth_local_credentials').fetchall(); print(json.dumps(sorted({'$'.join(row[0].split('$')[:4]) for row in rows})))",
      `${runtime.workdir}/data/trellis/platform.sqlite`,
    ],
  }).output();
  if (!hashing.success) {
    throw new Error("Cannot record the effective password-hashing profile");
  }
  const passwordHashParameters = z.array(z.string()).parse(
    JSON.parse(new TextDecoder().decode(hashing.stdout)),
  );
  const metadata = z.record(z.string(), z.unknown()).parse(
    JSON.parse(await Deno.readTextFile(`${output}/metadata.json`)),
  );
  await Deno.writeTextFile(
    `${output}/metadata.json`,
    JSON.stringify(
      {
        ...metadata,
        passwordHashParameters,
        authorization: lane === "lifecycle"
          ? {
            contextLifetimeSeconds: 120,
            refreshLeadSeconds: 60,
            refreshJitterSeconds: 15,
            minimumContextLifetimeSeconds: 76,
            allowedClockSkewSeconds: 30,
          }
          : "ordinary defaults",
      },
      null,
      2,
    ),
  );
  await Deno.writeTextFile(`${output}/runtime-workdir.txt`, runtime.workdir);
  const generatedDescriptor = z.custom<CallerParticipant>((value) =>
    z.object({
      kind: z.enum(["app", "agent", "service", "device"]),
      id: z.string(),
      uses: z.array(z.unknown()),
      implements: z.array(z.unknown()),
      packageEvidence: z.unknown(),
    }).safeParse(value).success
  );
  const selected = lane === "contract"
    ? z.object({
      participants: z.object({
        Provider: z.object({ participant: generatedDescriptor }),
        Caller: z.object({ participant: generatedDescriptor }),
      }),
    }).parse(
      await import(
        toFileUrl(resolve(z.string().parse(args["contract-entry"]))).href
      ),
    ).participants
    : participants;
  const serviceKey = await runtime.registerService({
    name: "benchmark-provider",
    contract: selected.Provider.participant,
  });
  // Prepare the ordinary portal policy through existing testkit provisioning,
  // not a bypass or pre-verified runtime context. No workload RPC is executed.
  if (lane === "contract") {
    const prepared = await runtime.connectClient({
      name: "benchmark-consent-setup",
      contract: selected.Caller.participant,
    });
    await prepared.connection.close();
  } else {
    const prepared = await runtime.connectClient({
      name: "benchmark-consent-setup",
      contract: participants.Caller.participant,
    });
    await prepared.logout();
    await prepared.connection.close();
  }
  const seeds: string[] = [];
  const browserSeeds = seeds;
  for (
    let index = 0;
    index < samples + Math.max(...sessionCounts) + 1;
    index++
  ) {
    seeds.push(
      (await runtime.registerClient({
        name: `benchmark-${index}`,
        contract: selected.Caller.participant,
      })).seed,
    );
  }
  const options = {
    cpuTicks,
    arrivalRate,
    maxOutstanding,
    output,
    trellisUrl: runtime.trellisUrl,
    password: runtime.adminPassword,
    serviceSeed: serviceKey.seed,
    seeds,
    samples,
    calls,
    sizes,
    sessionCounts,
    idleSeconds,
    warmups,
    workload: lane === "transfer" || lane === "lifecycle" ? lane : "all",
  };
  const providers: Deno.ChildProcess[] = [];
  for (let providerIndex = 0; providerIndex < providerCount; providerIndex++) {
    providers.push(
      await startWorker(
        lane === "rust"
          ? "rust"
          : lane === "contract"
          ? resolve(z.string().parse(args["contract-worker"]))
          : "worker.ts",
        {
          ...options,
          role: "provider",
          providerIndex,
        },
        true,
      ),
    );
    await runtime.waitFor(async () => {
      try {
        return JSON.parse(
          await Deno.readTextFile(`${output}/provider-${providerIndex}.json`),
        );
      } catch (error) {
        if (error instanceof Deno.errors.NotFound) return false;
        throw error;
      }
    });
  }
  const ready = await runtime.waitFor(async () => {
    try {
      return JSON.parse(await Deno.readTextFile(`${output}/provider-0.json`));
    } catch (error) {
      if (error instanceof Deno.errors.NotFound) return false;
      throw error;
    }
  });
  if (!ready) throw new Error("Provider did not become ready");
  const http = await startWorker(lane === "rust" ? "rust-http" : "http.ts", {
    ...options,
    role: lane === "rust" ? "http" : "provider",
  });
  const httpReady = await runtime.waitFor(async () => {
    try {
      return JSON.parse(await Deno.readTextFile(`${output}/http.json`));
    } catch (error) {
      if (error instanceof Deno.errors.NotFound) return false;
      throw error;
    }
  });
  await Deno.writeTextFile(`${output}/phase.txt`, "baseline-idle");
  await new Promise((resolve) => setTimeout(resolve, idleSeconds * 1000));
  const resourceRows = (await Deno.readTextFile(`${output}/resources.jsonl`))
    .trim().split("\n");
  const currentProcesses = z.object({
    processes: z.array(z.object({ pid: positive, role: z.string() })),
  }).parse(JSON.parse(resourceRows.at(-1)!));
  await Deno.writeTextFile(
    `${output}/processes.json`,
    JSON.stringify(
      currentProcesses.processes.filter(({ role }) =>
        ["provider", "http-provider", "nats", "trellis-server"].includes(role)
      ),
    ),
  );
  if (lane === "browser") {
    const browser = async (command: string[]) => {
      const result = await new Deno.Command("agent-browser", {
        args: ["--session", browserSession, ...command],
        env: { AGENT_BROWSER_DEFAULT_TIMEOUT: "60000" },
      }).output();
      if (!result.success) {
        throw new Error(new TextDecoder().decode(result.stderr));
      }
      return new TextDecoder().decode(result.stdout);
    };
    const windows = [];
    try {
      await Deno.writeTextFile(
        `${output}/browser-version.txt`,
        await browser(["--version"]),
      );
      for (let index = 0; index < samples; index++) {
        for (const resume of [false, true]) {
          await Deno.writeTextFile(
            `${output}/phase.txt`,
            `browser/${resume ? "resume" : "fresh"}`,
          );
          const before = await snapshotResources(
            output,
            cpuTicks,
            "browser-driver",
          );
          const started = performance.now();
          const count = browserSamples.length;
          try {
            await browser([
              "open",
              `${browserOrigin}/?index=${index}&resume=${resume}`,
            ]);
            await browser([
              "wait",
              "--fn",
              'document.body.dataset.complete === "true"',
            ]);
            if (browserSamples.length !== count + 1) {
              throw new Error("Browser did not submit exactly one result");
            }
          } catch (error) {
            if (browserSamples.length === count) {
              browserSamples.push({
                scenario: resume
                  ? "browser/resume-first-rpc"
                  : "browser/fresh-login-first-rpc",
                transport: "trellis",
                startedUnixMs: Date.now(),
                durationMs: performance.now() - started,
                error: String(error),
              });
            }
          }
          windows.push({
            scenario: browserSamples.at(-1)!.scenario,
            transport: "trellis",
            calls: 1,
            durationMs: performance.now() - started,
            before,
            after: await snapshotResources(output, cpuTicks, "browser-driver"),
          });
        }
      }
    } finally {
      await browser(["close"]);
    }
    await Deno.writeTextFile(
      `${output}/samples.json`,
      JSON.stringify(
        {
          samples: browserSamples,
          summary: summarize(browserSamples),
          windows,
        },
        null,
        2,
      ),
    );
    if (browserSamples.some((sample) => sample.error)) {
      throw new Error("Browser samples failed; see samples.json");
    }
  } else if (lane === "lifecycle") {
    const rows: Sample[] = [];
    const windows = [];
    const client = await runtime.connectClient({
      name: "benchmark-lifecycle",
      contract: participants.Caller.participant,
    });
    const database = openDatabase({
      url: `file:${runtime.workdir}/data/trellis/platform.sqlite`,
    });
    try {
      const sessionId = (await client.sessionsMe({}).orThrow()).session
        ?.sessionId;
      if (!sessionId) throw new Error("Lifecycle caller session missing");
      const count = async () =>
        Number(
          (await database.execute({
            sql:
              "SELECT COUNT(DISTINCT context_digest) AS count FROM auth_authorization_contexts WHERE login_session_id = ?",
            args: [sessionId],
          })).rows[0].count,
        );
      const initial = await count();
      await Deno.writeTextFile(
        `${output}/phase.txt`,
        "native/automatic-refresh-window",
      );
      const before = await snapshotResources(output, cpuTicks);
      const started = performance.now();
      const end = started + 150_000;
      while (await count() < initial + 2 && performance.now() < end) {
        const value = `refresh-${rows.length}`;
        const at = performance.now();
        const row: Sample = {
          scenario: "native/refresh-window-echo",
          transport: "trellis",
          startedUnixMs: Date.now(),
          durationMs: 0,
        };
        try {
          if (
            (await deadline(client.echo({ value }).orThrow())).value !== value
          ) throw new Error("Incorrect refresh response");
        } catch (error) {
          row.error = Deno.inspect(error, { colors: false, depth: 6 });
        }
        row.durationMs = performance.now() - at;
        rows.push(row);
        // Deliberate 10 Hz offered traffic, not a readiness sleep.
        await new Promise((resolve) =>
          setTimeout(resolve, Math.max(0, 100 - row.durationMs))
        );
      }
      const issued = await count();
      rows.push({
        scenario: "native/two-persisted-refreshes",
        transport: "trellis",
        startedUnixMs: Date.now() - (performance.now() - started),
        durationMs: performance.now() - started,
        operations: issued - initial,
        ...(issued < initial + 2
          ? { error: "Two renewed contexts were not durably issued" }
          : {}),
      });
      windows.push({
        scenario: "native/automatic-refresh-window",
        transport: "trellis",
        calls: rows.length - 1,
        durationMs: performance.now() - started,
        before,
        after: await snapshotResources(output, cpuTicks),
      });
      for (let index = 0; index < samples; index++) {
        await Deno.writeTextFile(
          `${output}/phase.txt`,
          "native/outage-reconnect-first-rpc",
        );
        const before = await snapshotResources(output, cpuTicks);
        const providerReconnects = await Promise.all(
          providers.map((_, providerIndex) =>
            Deno.readTextFile(
              `${output}/provider-${providerIndex}-connection.json`,
            )
              .then((text) =>
                z.object({ reconnects: z.number() }).parse(JSON.parse(text))
                  .reconnects
              )
          ),
        );
        const disconnected = Promise.withResolvers<number>();
        const reconnected = Promise.withResolvers<number>();
        let lost = false;
        const unsubscribe = client.connection.subscribe((status) => {
          if (status.phase === "disconnected" && !lost) {
            lost = true;
            disconnected.resolve(performance.now());
          }
          if (
            lost && status.phase === "connected" &&
            status.transport?.event === "reconnect"
          ) reconnected.resolve(performance.now());
        });
        const started = performance.now();
        const row: Sample = {
          scenario: "native/outage-reconnect-first-rpc",
          transport: "trellis",
          startedUnixMs: Date.now(),
          durationMs: 0,
        };
        try {
          runtime.interruptNativeTransport();
          const disconnectedAt = await deadline(disconnected.promise);
          runtime.restoreNativeTransport();
          const connectedAt = await deadline(reconnected.promise);
          // The proxy partitions every native peer, not just this caller.
          // A connected caller is not evidence that the provider routes are ready.
          await runtime.waitFor(async () =>
            (await Promise.all(providers.map(async (_, providerIndex) => {
              const status = z.object({
                connected: z.boolean(),
                reconnects: z.number(),
              })
                .parse(
                  JSON.parse(
                    await Deno.readTextFile(
                      `${output}/provider-${providerIndex}-connection.json`,
                    ),
                  ),
                );
              return status.connected &&
                status.reconnects > providerReconnects[providerIndex];
            }))).every(Boolean)
          );
          const value = `reconnected-${index}`;
          if (
            (await deadline(client.echo({ value }).orThrow())).value !== value
          ) throw new Error("Incorrect reconnect response");
          row.durationMs = performance.now() - disconnectedAt;
          row.connectMs = connectedAt - disconnectedAt;
          row.firstRpcMs = performance.now() - connectedAt;
        } catch (error) {
          row.durationMs = performance.now() - started;
          row.error = Deno.inspect(error, { colors: false, depth: 6 });
        } finally {
          runtime.restoreNativeTransport();
          unsubscribe();
          await Deno.writeTextFile(
            `${output}/outage-${index}-connections.json`,
            JSON.stringify(
              runtime.nativeTransportGate().connections(),
              null,
              2,
            ),
          );
        }
        rows.push(row);
        windows.push({
          scenario: row.scenario,
          transport: row.transport,
          calls: 1,
          durationMs: performance.now() - started,
          before,
          after: await snapshotResources(output, cpuTicks),
        });
      }
      await client.logout();
    } finally {
      database.close();
      await client.connection.close();
    }
    await Deno.writeTextFile(
      `${output}/samples.json`,
      JSON.stringify(
        { samples: rows, summary: summarize(rows), windows },
        null,
        2,
      ),
    );
    if (rows.some((row) => row.error)) {
      throw new Error("Lifecycle samples failed; see samples.json");
    }
  } else if (lane === "rust") {
    const seed = seeds[0];
    const prepared = await TrellisClient.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Caller.participant,
      name: "rust-login-preparation",
      auth: {
        mode: "session_key",
        sessionKeySeed: seed,
        redirectTo: `${runtime.trellisUrl}/_trellis/test/client-auth`,
      },
      onAuthRequired: (context) =>
        completeLocalAuthFlow({
          trellisUrl: runtime!.trellisUrl,
          loginUrl: context.loginUrl,
          password: runtime!.adminPassword,
        }),
    }).orThrow();
    const sessionId = (await prepared.sessionsMe({}).orThrow()).session
      ?.sessionId;
    await prepared.connection.close();
    if (!sessionId) throw new Error("Prepared Rust caller session missing");
    const before = await snapshotResources(output, cpuTicks, "coordinator");
    const started = performance.now();
    const child = await startWorker("rust", {
      ...options,
      role: "client",
      seed,
      sessionId,
      httpUrl: z.url().parse(httpReady.httpUrl),
    });
    const result = await child.status;
    const rows = z.array(z.object({
      scenario: z.string(),
      transport: z.enum(["trellis", "http"]),
      startedUnixMs: z.number(),
      durationMs: z.number(),
      bytes: z.number().optional(),
      error: z.string().optional(),
    })).parse(
      JSON.parse(await Deno.readTextFile(`${output}/native-samples.json`)),
    );
    const windows = [{
      scenario: "rust/all-workloads",
      transport: "mixed",
      calls: rows.length,
      durationMs: performance.now() - started,
      before,
      after: await snapshotResources(output, cpuTicks, "coordinator"),
    }];
    await Deno.writeTextFile(
      `${output}/samples.json`,
      JSON.stringify(
        { samples: rows, summary: summarize(rows), windows },
        null,
        2,
      ),
    );
    await Deno.writeTextFile(
      `${output}/native-binary-sha256.txt`,
      new TextDecoder().decode(
        (await new Deno.Command("sha256sum", {
          args: [resolve(z.string().parse(args["rust-bin"]))],
        }).output()).stdout,
      ),
    );
    if (!result.success || rows.some((row) => row.error)) {
      throw new Error(
        `Native benchmark exited ${result.code}; failures retained`,
      );
    }
  } else {
    const client = await startWorker(
      lane === "contract"
        ? resolve(z.string().parse(args["contract-worker"]))
        : "worker.ts",
      {
        ...options,
        role: "client",
        httpUrl: z.url().parse(httpReady.httpUrl),
      },
    );
    const result = await client.status;
    if (!result.success) {
      throw new Error(
        `Load generator exited ${result.code}; partial samples are retained in ${output}`,
      );
    }
  }
  if (lane === "transfer") {
    // Do not charge the later raw reference to the client's after-disconnect
    // observation window; it is a distinct unsigned diagnostic workload.
    await Deno.writeTextFile(`${output}/phase.txt`, "raw-core-nats-diagnostic");
    const store = z.array(z.object({
      scenario: z.string(),
      transport: z.literal("store"),
      bytes: z.number(),
      warmup: z.boolean(),
      startedUnixMs: z.number(),
      durationMs: z.number(),
      error: z.string().optional(),
    })).parse(
      JSON.parse(await Deno.readTextFile(`${output}/store-samples.json`)),
    );
    const raw = await rawTransfer({
      connect: runtime.connectNats.bind(runtime),
      sizes,
      samples,
      warmups,
    });
    const results = JSON.parse(
      await Deno.readTextFile(`${output}/samples.json`),
    );
    results.samples.push(...store, ...raw);
    results.summary = summarize(results.samples);
    await Deno.writeTextFile(
      `${output}/samples.json`,
      JSON.stringify(results, null, 2),
    );
    if (results.samples.some((sample: Sample) => sample.error)) {
      throw new Error("Transfer diagnostics failed; see samples.json");
    }
  }
  for (const provider of providers) provider.kill("SIGTERM");
  http.kill("SIGTERM");
  await Promise.all([
    ...providers.map((provider) => provider.status),
    http.status,
  ]);
  console.log(`Benchmark results: ${output}`);
} finally {
  if (browserServer) await browserServer.shutdown();
  if (runtime) {
    await Deno.writeTextFile(
      `${output}/server.log`,
      runtime.controlPlaneOutput(),
    );
  }
  for (const child of children) {
    try {
      child.kill("SIGTERM");
    } catch (error) {
      // Deno reports an already-exited child as TypeError; SIGTERM is valid.
      if (
        !(error instanceof Deno.errors.NotFound) &&
        !(error instanceof TypeError)
      ) throw error;
    }
  }
  await Promise.all(children.map((child) => child.status));
  await Promise.all(logWrites);
  if (runtime) await runtime.stop();
  await Deno.writeTextFile(`${output}/stop-sampler`, "stopped");
  if (sampler && !(await sampler.status).success) {
    throw new Error("Resource sampler failed");
  }
}
