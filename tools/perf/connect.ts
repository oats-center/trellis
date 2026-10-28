// Manual same-machine connect audit for the Trellis console.
//
// This tool is developer-facing only: it is never invoked by tests, CI, or
// release validation, and it makes no pass/fail decision from latency. It
// reports p50/p95 and raw samples so a baseline and a candidate measured on the
// same machine under the same conditions can be compared by a human.
//
// Usage:
//   TRELLIS_URL=http://127.0.0.1:3000 \
//   TRELLIS_PROFILE_DIR=/path/to/already-signed-in/profile \
//     node --experimental-transform-types tools/perf/connect.ts \
//     --warmup 3 --runs 20 --asset-cache enabled \
//     --output tools/perf/results/connect-current.json
import { mkdir, writeFile } from "node:fs/promises";
import { chromium, type Page, type Request, type Response } from "playwright";

type Options = {
  warmup: number;
  runs: number;
  assetCache: "enabled" | "disabled";
  output: string;
};

type TokenAttempt = {
  start: number;
  end: number | null;
  status: number | null;
  ok: boolean;
  message?: string;
};

type Sample = {
  run: number;
  warmup: boolean;
  navReadyMs: number | null;
  oauthTokenMs: number | null;
  tokenAttempts: number;
  errors: string[];
};

function parseArgs(args: string[]): Options {
  const options: Options = {
    warmup: 3,
    runs: 20,
    assetCache: "enabled",
    output: "",
  };
  for (let index = 0; index < args.length; index += 1) {
    const option = args[index];
    switch (option) {
      case "--warmup": {
        const parsed = Number(args[++index]);
        if (!Number.isInteger(parsed) || parsed < 0) {
          throw new Error("--warmup requires a non-negative integer");
        }
        options.warmup = parsed;
        break;
      }
      case "--runs": {
        const parsed = Number(args[++index]);
        if (!Number.isInteger(parsed) || parsed < 1) {
          throw new Error("--runs requires a positive integer");
        }
        options.runs = parsed;
        break;
      }
      case "--asset-cache": {
        const value = args[++index];
        if (value !== "enabled" && value !== "disabled") {
          throw new Error("--asset-cache must be enabled or disabled");
        }
        options.assetCache = value;
        break;
      }
      case "--output":
        options.output = args[++index] ?? "";
        break;
      default:
        throw new Error(`unknown option ${option}`);
    }
  }
  if (options.output.length === 0) throw new Error("--output is required");
  return options;
}

function percentile(values: number[], fraction: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((left, right) => left - right);
  const rank = Math.ceil(fraction * sorted.length);
  return sorted[Math.min(Math.max(rank - 1, 0), sorted.length - 1)];
}

async function collectTokenAttempts(page: Page): Promise<TokenAttempt[]> {
  return await page.evaluate(() => {
    const state = (
      globalThis as { __connectAudit?: { tokens: TokenAttempt[] } }
    ).__connectAudit;
    return state?.tokens.map((attempt) => ({ ...attempt })) ?? [];
  });
}

async function measureSample(
  page: Page,
  consoleUrl: string,
  run: number,
  warmup: boolean,
): Promise<Sample> {
  const errors: string[] = [];
  const onPageError = (error: Error) =>
    errors.push(`pageerror: ${error.message}`);
  const onRequestFailed = (request: Request) =>
    errors.push(
      `requestfailed: ${request.url()} (${request.failure()?.errorText ?? "failed"})`,
    );
  const onResponse = (response: Response) => {
    if (response.status() >= 400) {
      errors.push(`http ${response.status()}: ${response.url()}`);
    }
  };
  page.on("pageerror", onPageError);
  page.on("requestfailed", onRequestFailed);
  page.on("response", onResponse);
  const started = performance.now();
  try {
    await page.goto(consoleUrl, {
      waitUntil: "domcontentloaded",
      timeout: 30_000,
    });
    await page
      .locator('[data-testid="console-ready"]')
      .waitFor({ state: "visible", timeout: 60_000 });
  } catch (error) {
    errors.push(`readiness: ${String(error)}`);
  }
  const navReadyMs = performance.now() - started;
  page.off("pageerror", onPageError);
  page.off("requestfailed", onRequestFailed);
  page.off("response", onResponse);

  const attempts = await collectTokenAttempts(page);
  const successful = attempts.find(
    (attempt) => attempt.ok && attempt.end !== null,
  );
  return {
    run,
    warmup,
    navReadyMs,
    oauthTokenMs:
      successful !== undefined && successful.end !== null
        ? successful.end - successful.start
        : null,
    tokenAttempts: attempts.length,
    errors,
  };
}

const options = parseArgs(process.argv.slice(2));
const trellisUrl = process.env.TRELLIS_URL;
const profileDir = process.env.TRELLIS_PROFILE_DIR;
if (!trellisUrl) throw new Error("TRELLIS_URL is required");
if (!profileDir) throw new Error("TRELLIS_PROFILE_DIR is required");
const baseUrl = trellisUrl.replace(/\/$/, "");
const consoleUrl = `${baseUrl}/console`;

const samples: Sample[] = [];
let context: Awaited<
  ReturnType<typeof chromium.launchPersistentContext>
> | null = null;
try {
  context = await chromium.launchPersistentContext(profileDir, {
    headless: false,
    args: [`--unsafely-treat-insecure-origin-as-secure=${baseUrl}`],
  });
  const page = context.pages()[0] ?? (await context.newPage());
  await page.addInitScript(() => {
    const state: { tokens: TokenAttempt[] } = ((
      globalThis as { __connectAudit?: { tokens: TokenAttempt[] } }
    ).__connectAudit = { tokens: [] });
    const original = globalThis.fetch;
    globalThis.fetch = async (input, init) => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof Request
            ? input.url
            : String(input);
      if (!url.includes("/oauth/token")) {
        return await original(input, init);
      }
      const attempt: TokenAttempt = {
        start: performance.now(),
        end: null,
        status: null,
        ok: false,
      };
      state.tokens.push(attempt);
      try {
        const response = await original(input, init);
        attempt.end = performance.now();
        attempt.status = response.status;
        attempt.ok = response.ok;
        return response;
      } catch (error) {
        attempt.end = performance.now();
        attempt.message = String(error);
        throw error;
      }
    };
  });
  if (options.assetCache === "disabled") {
    const session = await context.newCDPSession(page);
    await session.send("Network.setCacheDisabled", { cacheDisabled: true });
  }

  const total = options.warmup + options.runs;
  for (let run = 0; run < total; run += 1) {
    const warmup = run < options.warmup;
    const sample = await measureSample(page, consoleUrl, run + 1, warmup);
    samples.push(sample);
    console.log(
      `run ${run + 1}/${total}${warmup ? " (warmup)" : ""}: ` +
        `nav_ready=${sample.navReadyMs?.toFixed(0) ?? "n/a"}ms ` +
        `oauth_token=${sample.oauthTokenMs?.toFixed(0) ?? "n/a"}ms ` +
        `attempts=${sample.tokenAttempts} ` +
        (sample.errors.length === 0 ? "ok" : `errors=${sample.errors.length}`),
    );
  }
} finally {
  if (context) await context.close().catch(() => {});
}

const measured = samples.filter((sample) => !sample.warmup);
const successfulMeasured = measured.filter(
  (sample) => sample.errors.length === 0 && sample.navReadyMs !== null,
);
const navReadyValues = successfulMeasured.map(
  (sample) => sample.navReadyMs as number,
);
const tokenValues = successfulMeasured.flatMap((sample) =>
  sample.oauthTokenMs === null ? [] : [sample.oauthTokenMs],
);
const report = {
  generatedAt: new Date().toISOString(),
  trellisUrl: baseUrl,
  runtime: { os: process.platform, arch: process.arch, node: process.version },
  assetCache: options.assetCache,
  warmupRequested: options.warmup,
  runsRequested: options.runs,
  measuredSucceeded: successfulMeasured.length,
  measuredFailed: measured.length - successfulMeasured.length,
  navReady: {
    p50Ms: percentile(navReadyValues, 0.5),
    p95Ms: percentile(navReadyValues, 0.95),
  },
  oauthToken: {
    p50Ms: percentile(tokenValues, 0.5),
    p95Ms: percentile(tokenValues, 0.95),
  },
  samples,
};
const output = options.output;
const slash = output.lastIndexOf("/");
if (slash > 0) {
  await mkdir(output.slice(0, slash), { recursive: true });
}
await writeFile(output, JSON.stringify(report, null, 2));
console.log(`\nreport -> ${output}`);
console.log(
  `nav_ready p50=${report.navReady.p50Ms?.toFixed(1) ?? "n/a"}ms ` +
    `p95=${report.navReady.p95Ms?.toFixed(1) ?? "n/a"}ms | ` +
    `oauth_token p50=${report.oauthToken.p50Ms?.toFixed(1) ?? "n/a"}ms ` +
    `p95=${report.oauthToken.p95Ms?.toFixed(1) ?? "n/a"}ms | ` +
    `measured ${report.measuredSucceeded}/${measured.length} valid`,
);
if (report.measuredFailed > 0) process.exitCode = 1;
