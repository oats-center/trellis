import { execFile } from "node:child_process";
import { once } from "node:events";
import { createWriteStream } from "node:fs";
import { mkdir, open, readdir, readFile, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { hostname } from "node:os";
import { resolve } from "node:path";
import { Readable } from "node:stream";
import { text } from "node:stream/consumers";
import { pipeline } from "node:stream/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
import { inspect, parseArgs, promisify } from "node:util";
import { chromium } from "playwright";
import { ulid } from "ulid";
import { z } from "zod";
import type {
  TrellisTestClientParticipant,
  TrellisTestParticipantLike,
} from "@oatscenter/trellis-testkit";
import {
  spawnTracked,
  type TrackedProcess,
} from "../../ts/integration/_support/process.ts";
import { authorizeRustNativeLogin } from "../../ts/integration/_support/runtime.ts";
import { deadline, type Sample, summarize } from "./model.ts";
import { snapshotResources } from "./resources.ts";
import { TrellisTestRuntime } from "../../ts/packages/trellis-testkit/index.ts";
import { apps, participants } from "./packages/performance-trellis/index.js";
import { rawTransfer } from "./raw_transfer.ts";
import { generateHttpTls } from "./http_security.ts";
import { observeTransfer } from "./observe.ts";

const root = fileURLToPath(new URL("../../", import.meta.url));
const exec = promisify(execFile);
const { values: args } = parseArgs({
  options: {
    output: { type: "string" },
    server: { type: "string" },
    cli: { type: "string" },
    samples: { type: "string", default: "21" },
    calls: { type: "string", default: "1000" },
    sizes: { type: "string", default: "65536,1048576,8388608" },
    sessions: { type: "string", default: "1,10,100" },
    "idle-seconds": { type: "string", default: "5" },
    "arrival-rate": { type: "string" },
    "max-outstanding": { type: "string", default: "128" },
    lane: { type: "string", default: "typescript" },
    providers: { type: "string", default: "1" },
    warmups: { type: "string", default: "0" },
    "browser-bundle": { type: "string" },
    "rust-bin": { type: "string" },
    "contract-entry": { type: "string" },
    "contract-worker": { type: "string" },
    "keep-workdir": { type: "boolean", default: false },
    diagnostics: { type: "boolean", default: false },
    "http-chunk": { type: "string", default: String(64 * 1024) },
    "http-tls": { type: "boolean", default: false },
  },
});
const httpChunkBytes = z.coerce
  .number()
  .int()
  .positive()
  .parse(args["http-chunk"]);
const positive = z.coerce.number().int().positive();
const samples = positive.parse(args.samples);
const calls = positive.parse(args.calls);
const warmups = z.coerce.number().int().nonnegative().parse(args.warmups);
const lane = z
  .enum(["typescript", "transfer", "lifecycle", "browser", "rust", "contract"])
  .parse(args.lane);
const providerCount = positive.parse(args.providers);
if (lane === "transfer" && providerCount !== 1) {
  throw new Error(
    "The focused transfer audit uses one provider; replica scaling belongs to the full suite",
  );
}
const arrivalRate = z.coerce
  .number()
  .nonnegative()
  .parse(args["arrival-rate"] ?? (lane === "browser" ? "0.05" : "0"));
const maxOutstanding = positive.parse(args["max-outstanding"]);
const sizes = z
  .array(positive.max(512 * 1024 * 1024))
  .nonempty()
  .parse(args.sizes.split(","));
const sessionCounts = z
  .array(positive)
  .nonempty()
  .parse(args.sessions.split(","))
  .sort((a, b) => a - b);
const idleSeconds = z.coerce.number().positive().parse(args["idle-seconds"]);
const server = resolve(
  args.server ??
    process.env.TRELLIS_TEST_SERVER_BIN ??
    `${root}/target/release/trellis-server`,
);
const cli = resolve(args.cli ?? `${root}/target/release/trellis`);
const output = resolve(
  args.output ??
    `${root}/benchmarks/runtime/results/${new Date()
      .toISOString()
      .replaceAll(":", "-")}`,
);
const cpuTicks = positive.parse(
  (await exec("getconf", ["CLK_TCK"])).stdout.trim(),
);
if (process.platform !== "linux") {
  throw new Error("Resource sampling currently requires Linux /proc");
}
await mkdir(output, { recursive: true });
// Exclusive creation prevents accidental reuse of stop markers or old worker results.
await (await open(`${output}/run.lock`, "wx")).close();
const children: TrackedProcess[] = [];
const logWrites: Promise<void>[] = [];
let runtime: TrellisTestRuntime | undefined;
let sampler: TrackedProcess | undefined;
let browserServer: Server | undefined;
let stopObserver: Awaited<ReturnType<typeof observeTransfer>> | undefined;
const browserSamples: Sample[] = [];
async function startWorker(
  file: string,
  options: Record<string, unknown>,
  provider = false,
) {
  const native = file.startsWith("rust");
  const child = spawnTracked(
    native ? resolve(z.string().parse(args["rust-bin"])) : process.execPath,
    native
      ? provider
        ? ["--provider"]
        : file === "rust-http"
          ? ["--http-provider"]
          : []
      : [
          "--experimental-transform-types",
          resolve(root, "benchmarks/runtime", file),
          ...(provider ? ["--provider"] : []),
        ],
    { cwd: root },
  );
  children.push(child);
  const label = provider
    ? `provider-${options.providerIndex ?? 0}`
    : file === "http.ts" || file === "rust-http"
      ? "http"
      : options.role === "storage"
        ? "storage"
        : options.role === "resume"
          ? "client-resume"
          : "client";
  for (const [kind, stream] of [
    ["stdout", child.stdout],
    ["stderr", child.stderr],
  ] as const) {
    const writing = pipeline(
      stream,
      createWriteStream(`${output}/${label}.${kind}.log`, { flags: "wx" }),
    );
    void writing.catch(() => {});
    logWrites.push(writing);
  }
  await pipeline(Readable.from([JSON.stringify(options)]), child.stdin);
  return child;
}
try {
  const git = await exec("git", ["rev-parse", "HEAD"], { cwd: root });
  const diff = await exec("git", ["diff", "HEAD"], {
    cwd: root,
    maxBuffer: 64 * 1024 * 1024,
  });
  const snapshotRevision = await readFile(
    `${root}/.source-revision`,
    "utf8",
  ).catch((error: unknown) => {
    if (error instanceof Error && "code" in error && error.code === "ENOENT")
      return undefined;
    throw error;
  });
  const revision =
    snapshotRevision === undefined
      ? git.stdout.trim()
      : z
          .string()
          .regex(/^[0-9a-f]{40}$/)
          .parse(snapshotRevision.trim());
  await writeFile(
    `${output}/source.diff`,
    snapshotRevision === undefined
      ? diff.stdout
      : await readFile(`${root}/.benchmark-overlay.diff`),
  );
  const hash = await exec("sha256sum", [server]);
  const cliHash = await exec("sha256sum", [cli]);
  const cpuInfo = await readFile("/proc/cpuinfo", "utf8");
  const cgroupPath = (await readFile("/proc/self/cgroup", "utf8"))
    .split("\n")
    .find((line) => line.startsWith("0::"))
    ?.slice(3);
  const cgroup = Object.fromEntries(
    await Promise.all(
      ["cpu.max", "cpuset.cpus.effective", "memory.max"].map(async (name) => [
        name,
        cgroupPath === undefined
          ? null
          : await readFile(`/sys/fs/cgroup${cgroupPath}/${name}`, "utf8")
              .then((value) => value.trim())
              .catch(() => null),
      ]),
    ),
  );
  await writeFile(
    `${output}/metadata.json`,
    JSON.stringify(
      {
        revision,
        sourceSnapshot: snapshotRevision !== undefined,
        trackedDiff: "source.diff",
        serverSha256: hash.stdout.split(" ")[0],
        cliSha256: cliHash.stdout.split(" ")[0],
        startedAt: new Date().toISOString(),
        node: process.versions,
        host: hostname(),
        os: { os: process.platform, arch: process.arch },
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
        diagnostics: args.diagnostics,
        httpChunkBytes,
        httpTls: args["http-tls"],
        topology: `loopback; native NATS TCP; ${args["http-tls"] ? "mutual-TLS" : "plaintext"} HTTP/1.1 keep-alive with ${httpChunkBytes}-byte body chunks; no compression or HTTP caching`,
        setup:
          "Contracts, resources and platform grants prepared before measurement; backend warm; fresh login includes ordinary login/consent and distinct OAuth custody. First-process connect includes lazy SDK initialization, not module parsing; OS caches are not cleared",
        security: args["http-tls"]
          ? "Trellis production auth; HTTP mutual TLS plus per-request Ed25519 signatures. Channel identity and request authorization are matched; Trellis revocation, authority renewal and brokered per-frame proofs have no HTTP equivalent"
          : "Trellis production auth; HTTP unauthenticated lower bound, NOT security equivalent",
        storage:
          "Trellis JetStream object storage. HTTP raw: local file, closed but not fsynced; HTTP store: the provider's NATS Object Store; HTTP stream: no storage",
        telemetry: {
          traces: process.env.OTEL_TRACES_SAMPLER ?? "SDK defaults",
          endpointConfigured: Boolean(process.env.OTEL_EXPORTER_OTLP_ENDPOINT),
        },
        cpuInfo,
        cpuModel: cpuInfo.match(/^model name\s*:\s*(.+)$/m)?.[1] ?? "unknown",
        logicalCpus: cpuInfo.match(/^processor\s*:/gm)?.length ?? null,
        cgroup,
        memInfo: await readFile("/proc/meminfo", "utf8"),
      },
      null,
      2,
    ),
  );
  sampler = spawnTracked(
    "python3",
    [`${root}/benchmarks/runtime/resources.py`, String(process.pid), output],
    { stdio: "inherit" },
  );
  let browserOrigin: string | undefined;
  if (lane === "browser") {
    const bundle = await readFile(z.string().parse(args["browser-bundle"]));
    browserServer = createServer((request, response) => {
      void (async () => {
        const url = new URL(request.url ?? "/", "http://127.0.0.1");
        response.setHeader("cache-control", "no-store");
        if (url.pathname === "/protocol_wasm/trellis_protocol_wasm_bg.wasm") {
          response.setHeader("content-type", "application/wasm");
          response.end(
            await readFile(
              `${root}/ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm_bg.wasm`,
            ),
          );
          return;
        }
        if (url.pathname === "/bundle.js") {
          response.setHeader("content-type", "text/javascript");
          response.end(bundle);
          return;
        }
        if (url.pathname === "/config") {
          const index = z.coerce
            .number()
            .int()
            .min(0)
            .max(samples - 1)
            .parse(url.searchParams.get("index"));
          response.setHeader("content-type", "application/json");
          response.end(
            JSON.stringify({
              trellisUrl: runtime!.oauthIssuer,
              credentialHandle: credentialHandles[index],
              clientId: apps.BrowserCaller.request.id,
            }),
          );
          return;
        }
        if (url.pathname === "/results" && request.method === "POST") {
          const sample = z
            .object({
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
            })
            .parse(JSON.parse(await text(request)));
          browserSamples.push(sample);
          response.setHeader("content-type", "application/json");
          response.end(JSON.stringify({ received: true }));
          return;
        }
        response.setHeader("content-type", "text/html");
        response.end(
          '<!doctype html><title>Trellis benchmark</title><body>Measuring real browser connection…<script type="module" src="/bundle.js"></script>',
        );
      })().catch((error: unknown) =>
        response.destroy(error instanceof Error ? error : undefined),
      );
    });
    browserServer.listen(0, "127.0.0.1");
    await once(browserServer, "listening");
    const address = browserServer.address();
    if (!address || typeof address === "string") {
      throw new Error("Browser fixture requires TCP");
    }
    browserOrigin = `http://127.0.0.1:${address.port}`;
  }
  let httpTlsDir: string | undefined;
  if (args["http-tls"]) {
    // Ephemeral CA plus server/client identities for mutual TLS, and one
    // Ed25519 request-signing key. Generated outside every measured window.
    httpTlsDir = `${output}/tls`;
    await generateHttpTls(httpTlsDir);
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
    authorization:
      lane === "lifecycle" ? { contextLifetimeSeconds: 120 } : undefined,
  });
  await runtime.ensureAdmin();
  if (args.diagnostics)
    stopObserver = await observeTransfer(await runtime.connectNats());
  const metadata = z
    .record(z.string(), z.unknown())
    .parse(JSON.parse(await readFile(`${output}/metadata.json`, "utf8")));
  await writeFile(
    `${output}/metadata.json`,
    JSON.stringify(
      {
        ...metadata,
        authorization:
          lane === "lifecycle"
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
  await writeFile(`${output}/runtime-workdir.txt`, runtime.workdir);

  const selected: {
    participants: { Provider: { participant: TrellisTestParticipantLike } };
    apps: { Caller: { request: TrellisTestClientParticipant } };
  } =
    lane === "contract"
      ? await import(
          pathToFileURL(resolve(z.string().parse(args["contract-entry"]))).href
        )
      : { participants, apps };
  const serviceKey = await runtime.registerService({
    name: "benchmark-provider",
    contract: selected.participants.Provider.participant,
  });
  const prepared = await runtime.registerClient({
    name: "benchmark-consent-setup",
    contract: selected.apps.Caller.request,
  });
  await prepared.session.close();
  const credentialHandles = Array.from(
    { length: samples + Math.max(...sessionCounts) + 1 },
    () => ulid(),
  );
  if (browserOrigin) {
    await runtime.callAdminRpc("authOAuthClientsPut", {
      clientId: apps.BrowserCaller.request.id,
      kind: "browser",
      title: "Browser benchmark",
      description: "Isolated browser performance caller",
      redirectUris: [`${browserOrigin}/callback`],
      development: true,
      impliedConsent: [],
      expectedRevision: 0n,
      idempotencyKey: ulid(),
    });
  }
  const options = {
    cpuTicks,
    arrivalRate,
    maxOutstanding,
    output,
    trellisUrl: runtime.oauthIssuer,
    username: runtime.adminUsername,
    password: runtime.adminPassword,
    serviceSeed: serviceKey.seed,
    credentialHandles,
    samples,
    calls,
    sizes,
    sessionCounts,
    idleSeconds,
    warmups,
    workload: lane === "transfer" || lane === "lifecycle" ? lane : "all",
    httpChunkBytes,
    httpTlsDir,
  };
  const providers: TrackedProcess[] = [];
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
      const provider = providers[providerIndex];
      if (provider.exitCode !== null || provider.signalCode !== null) {
        throw new Error(
          `Provider ${providerIndex} exited before readiness (${provider.exitCode ?? provider.signalCode}); see ${output}/provider-${providerIndex}.stderr.log`,
        );
      }
      try {
        return JSON.parse(
          await readFile(`${output}/provider-${providerIndex}.json`, "utf8"),
        );
      } catch (error) {
        if (
          error instanceof Error &&
          "code" in error &&
          error.code === "ENOENT"
        )
          return false;
        throw error;
      }
    });
  }
  if (lane === "transfer" || lane === "rust") {
    // Finite storage workloads are measurements, not provider startup. In
    // particular, repeated large-object rounds must not consume readiness time.
    const storage = await startWorker(lane === "rust" ? "rust" : "worker.ts", {
      ...options,
      role: "storage",
    });
    const result = await storage.exited;
    if (result.code !== 0) {
      throw new Error(
        `Storage measurements exited ${result.code ?? result.signal}; see ${output}/storage.stderr.log`,
      );
    }
  }
  const ready = await runtime.waitFor(async () => {
    try {
      return JSON.parse(await readFile(`${output}/provider-0.json`, "utf8"));
    } catch (error) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT")
        return false;
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
      return JSON.parse(await readFile(`${output}/http.json`, "utf8"));
    } catch (error) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT")
        return false;
      throw error;
    }
  });
  await writeFile(`${output}/phase.txt`, "baseline-idle");
  await new Promise((resolve) => setTimeout(resolve, idleSeconds * 1000));
  const resourceRows = (await readFile(`${output}/resources.jsonl`, "utf8"))
    .trim()
    .split("\n");
  const currentProcesses = z
    .object({
      processes: z.array(z.object({ pid: positive, role: z.string() })),
    })
    .parse(JSON.parse(resourceRows.at(-1)!));
  await writeFile(
    `${output}/processes.json`,
    JSON.stringify(
      currentProcesses.processes.filter(({ role }) =>
        ["provider", "http-provider", "nats", "trellis-server"].includes(role),
      ),
    ),
  );
  if (lane === "browser") {
    const browser = await chromium.launch();
    const page = await browser.newPage();
    page.setDefaultTimeout(30_000);
    const windows = [];
    try {
      await writeFile(`${output}/browser-version.txt`, browser.version());
      const scheduleStarted = performance.now();
      for (let index = 0; index < samples; index++) {
        // Browser latency samples are fresh-login/resume pairs, not an auth
        // overload test. Pace arrivals before starting either measured action.
        if (arrivalRate > 0) {
          const delay =
            scheduleStarted + (index * 1000) / arrivalRate - performance.now();
          if (delay > 0)
            await new Promise((resolve) => setTimeout(resolve, delay));
        }
        for (const resume of [false, true]) {
          await writeFile(
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
            if (!resume) await page.context().clearCookies();
            await page.goto(
              `${browserOrigin}/?index=${index}&resume=${resume}`,
            );
            if (!resume) {
              await page
                .getByLabel("Username", { exact: true })
                .fill(runtime.adminUsername);
              await page
                .getByLabel("Password", { exact: true })
                .fill(runtime.adminPassword);
              await page
                .getByRole("button", { name: "Sign in", exact: true })
                .click();
              const approve = page.getByRole("button", {
                name: "Authorize application",
                exact: true,
              });
              await approve.waitFor();
              for (const choice of await page.getByRole("checkbox").all()) {
                if (await choice.isEnabled()) await choice.check();
              }
              await approve.click();
            }
            await page.waitForFunction(
              () => document.body.dataset.complete === "true",
            );
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
      await browser.close();
    }
    await writeFile(
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
      contract: apps.Caller.request,
    });
    const authorities = new Set<string>();
    const unsubscribeAuthority = client.ownAuthority.subscribe((snapshot) => {
      if (snapshot) authorities.add(snapshot.authorityDigest);
    });
    try {
      const initial = authorities.size;
      await writeFile(`${output}/phase.txt`, "native/automatic-refresh-window");
      const before = await snapshotResources(output, cpuTicks);
      const started = performance.now();
      const end = started + 150_000;
      while (authorities.size < initial + 2 && performance.now() < end) {
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
          )
            throw new Error("Incorrect refresh response");
        } catch (error) {
          row.error = inspect(error, { colors: false, depth: 6 });
        }
        row.durationMs = performance.now() - at;
        rows.push(row);
        // Deliberate 10 Hz offered traffic, not a readiness sleep.
        await new Promise((resolve) =>
          setTimeout(resolve, Math.max(0, 100 - row.durationMs)),
        );
      }
      const issued = authorities.size;
      rows.push({
        scenario: "native/two-promoted-renewals",
        transport: "trellis",
        startedUnixMs: Date.now() - (performance.now() - started),
        durationMs: performance.now() - started,
        operations: issued - initial,
        ...(issued < initial + 2
          ? { error: "Two renewed authorities were not promoted" }
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
        await writeFile(
          `${output}/phase.txt`,
          "native/outage-reconnect-first-rpc",
        );
        const before = await snapshotResources(output, cpuTicks);
        const providerReconnects = await Promise.all(
          providers.map((_, providerIndex) =>
            readFile(
              `${output}/provider-${providerIndex}-connection.json`,
              "utf8",
            ).then(
              (text) =>
                z.object({ reconnects: z.number() }).parse(JSON.parse(text))
                  .reconnects,
            ),
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
            lost &&
            status.phase === "connected" &&
            status.transport?.event === "reconnect"
          )
            reconnected.resolve(performance.now());
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
            (
              await Promise.all(
                providers.map(async (_, providerIndex) => {
                  const status = z
                    .object({
                      connected: z.boolean(),
                      reconnects: z.number(),
                    })
                    .parse(
                      JSON.parse(
                        await readFile(
                          `${output}/provider-${providerIndex}-connection.json`,
                          "utf8",
                        ),
                      ),
                    );
                  return (
                    status.connected &&
                    status.reconnects > providerReconnects[providerIndex]
                  );
                }),
              )
            ).every(Boolean),
          );
          const value = `reconnected-${index}`;
          if (
            (await deadline(client.echo({ value }).orThrow())).value !== value
          )
            throw new Error("Incorrect reconnect response");
          row.durationMs = performance.now() - disconnectedAt;
          row.connectMs = connectedAt - disconnectedAt;
          row.firstRpcMs = performance.now() - connectedAt;
        } catch (error) {
          row.durationMs = performance.now() - started;
          row.error = inspect(error, { colors: false, depth: 6 });
        } finally {
          runtime.restoreNativeTransport();
          unsubscribe();
          await writeFile(
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
    } finally {
      unsubscribeAuthority();
      await writeFile(
        `${output}/samples.json`,
        JSON.stringify(
          { samples: rows, summary: summarize(rows), windows },
          null,
          2,
        ),
      );
      await client.connection.close();
    }
    if (rows.some((row) => row.error)) {
      throw new Error("Lifecycle samples failed; see samples.json");
    }
  } else if (lane === "rust") {
    const before = await snapshotResources(output, cpuTicks, "coordinator");
    const started = performance.now();
    // The raw Core NATS lower bound uses the same privileged runtime
    // credentials as the TypeScript lane's `runtime.connectNats()` observer.
    const creds = (
      await readdir(runtime.workdir, { recursive: true, withFileTypes: true })
    )
      .filter(
        (entry) =>
          entry.isFile() &&
          entry.name === "trellis-auth.creds" &&
          entry.parentPath.endsWith("/nats/creds"),
      )
      .map((entry) => `${entry.parentPath}/${entry.name}`);
    if (creds.length !== 1) {
      throw new Error(
        `Expected one runtime NATS credential, found ${creds.length}`,
      );
    }
    const child = await startWorker("rust", {
      ...options,
      role: "client",
      clientId: apps.Caller.request.id,
      httpUrl: z.url().parse(httpReady.httpUrl),
      natsUrl: runtime.natsUrl,
      natsCreds: creds[0],
    });
    const loginOutput = await runtime.waitFor(async () => {
      const stdout = await readFile(`${output}/client.stdout.log`, "utf8");
      if (stdout.split("\n").some((line) => line.startsWith("NATIVE_LOGIN ")))
        return stdout;
      if (child.exitCode !== null)
        throw new Error(await readFile(`${output}/client.stderr.log`, "utf8"));
      return undefined;
    });
    await authorizeRustNativeLogin(runtime, loginOutput, apps.Caller.request);
    const result = await child.exited;
    const nativeSamples = z.array(
      z.object({
        scenario: z.string(),
        transport: z.enum(["trellis", "http", "store", "nats"]),
        startedUnixMs: z.number(),
        durationMs: z.number(),
        bytes: z.number().optional(),
        error: z.string().optional(),
        warmup: z.boolean().optional(),
        committedMs: z.number().optional(),
        setupMs: z.number().optional(),
        firstByteMs: z.number().optional(),
        verifiedMs: z.number().optional(),
        durableMs: z.number().optional(),
        maxFrameBytes: z.number().optional(),
        windowFrames: z.number().optional(),
        windowBytes: z.number().optional(),
        dataFrames: z.number().optional(),
        schedulerDelayMs: z.number().optional(),
      }),
    );
    const rows = nativeSamples.parse(
      JSON.parse(await readFile(`${output}/native-samples.json`, "utf8")),
    );
    rows.push(
      ...nativeSamples.parse(
        JSON.parse(
          await readFile(`${output}/native-store-samples.json`, "utf8"),
        ),
      ),
    );
    const windows = [
      {
        scenario: "rust/all-workloads",
        transport: "mixed",
        calls: rows.length,
        durationMs: performance.now() - started,
        before,
        after: await snapshotResources(output, cpuTicks, "coordinator"),
      },
    ];
    windows.push(
      ...JSON.parse(await readFile(`${output}/native-windows.json`, "utf8")),
    );
    const resumeBefore = await snapshotResources(
      output,
      cpuTicks,
      "coordinator",
    );
    const resumeStarted = performance.now();
    const resume = await startWorker("rust", {
      ...options,
      role: "resume",
      clientId: apps.Caller.request.id,
    });
    const resumeResult = await resume.exited;
    if (resumeResult.code !== 0) {
      throw new Error(
        `Native cold resume exited ${resumeResult.code}; see client-resume.stderr.log`,
      );
    }
    const resumeRows = nativeSamples.parse(
      JSON.parse(
        await readFile(`${output}/native-resume-samples.json`, "utf8"),
      ),
    );
    rows.push(...resumeRows);
    windows.push({
      scenario: "rust/cold-resume",
      transport: "trellis",
      calls: 1,
      durationMs: performance.now() - resumeStarted,
      before: resumeBefore,
      after: await snapshotResources(output, cpuTicks, "coordinator"),
    });
    await writeFile(
      `${output}/samples.json`,
      JSON.stringify(
        { samples: rows, summary: summarize(rows), windows },
        null,
        2,
      ),
    );
    await writeFile(
      `${output}/native-binary-sha256.txt`,
      (await exec("sha256sum", [resolve(z.string().parse(args["rust-bin"]))]))
        .stdout,
    );
    if (result.code !== 0 || rows.some((row) => row.error)) {
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
    const result = await client.exited;
    if (result.code !== 0) {
      throw new Error(
        `Load generator exited ${result.code}; partial samples are retained in ${output}`,
      );
    }
  }
  if (lane === "transfer") {
    // Do not charge the later raw reference to the client's after-disconnect
    // observation window; it is a distinct unsigned diagnostic workload.
    await writeFile(`${output}/phase.txt`, "raw-core-nats-diagnostic");
    const store = z
      .array(
        z.object({
          scenario: z.string(),
          transport: z.literal("store"),
          bytes: z.number(),
          warmup: z.boolean(),
          startedUnixMs: z.number(),
          durationMs: z.number(),
          error: z.string().optional(),
        }),
      )
      .parse(
        JSON.parse(await readFile(`${output}/store-samples.json`, "utf8")),
      );
    const raw = await rawTransfer({
      connect: runtime.connectNats.bind(runtime),
      sizes,
      samples,
      warmups,
    });
    const results = JSON.parse(
      await readFile(`${output}/samples.json`, "utf8"),
    );
    results.samples.push(...store, ...raw);
    results.summary = summarize(results.samples);
    await writeFile(`${output}/samples.json`, JSON.stringify(results, null, 2));
    if (results.samples.some((sample: Sample) => sample.error)) {
      throw new Error("Transfer diagnostics failed; see samples.json");
    }
  }
  for (const provider of providers) provider.kill("SIGTERM");
  http.kill("SIGTERM");
  await Promise.all([
    ...providers.map((provider) => provider.exited),
    http.exited,
  ]);
  console.log(`Benchmark results: ${output}`);
} finally {
  if (browserServer) {
    const server = browserServer;
    await new Promise<void>((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
  }
  if (runtime) {
    await writeFile(`${output}/server.log`, runtime.controlPlaneOutput());
  }
  if (stopObserver)
    await writeFile(
      `${output}/transfer-diagnostics.json`,
      JSON.stringify(await stopObserver(), null, 2),
    );
  for (const child of children) child.kill("SIGTERM");
  await Promise.all(children.map((child) => child.exited));
  await Promise.all(logWrites);
  if (runtime) await runtime.stop();
  await writeFile(`${output}/stop-sampler`, "stopped");
  await sampler?.exited;
}
if (sampler && (await sampler.exited).code !== 0) {
  throw new Error("Resource sampler failed");
}
