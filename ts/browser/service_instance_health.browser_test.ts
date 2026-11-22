/**
 * An administrator sees which instance of a service is unhealthy and why:
 * the CLI's `svc <ID> instances`, as text and as JSON, and the Console's
 * instance table name the failing check and its message, and say nothing
 * more of a healthy instance. One failed check makes an instance unhealthy
 * even though the SDK's own connection check passes, and a degraded check
 * makes it degraded, as a Rust service reports the same checks.
 *
 * A process that stops sending heartbeats while its connection stays open
 * (here a stopped process, as a starved or hung one is) still shows what it
 * last reported, with offline health, so it cannot pass for a fresh instance
 * that has not reported yet.
 */
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { chromium } from "playwright";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import {
  browserRuntimeOptions,
  launchProfile,
  loginCliAsAdministrator,
} from "./browser_test_support.ts";
import { openConsoleAsRuntimeAdmin } from "./console_test_support.ts";

/**
 * A service in its own process, so the test can stop that process while its
 * connection stays open. It serves the Provider contract from `fixture` with
 * a heartbeat every second.
 */
const program = `
import { TrellisService } from ${JSON.stringify(
  pathToFileURL(join(import.meta.dirname, "../packages/trellis/service/mod.ts"))
    .href,
)};
const [trellisUrl, seed, fixture] = process.argv.slice(1);
const { participants } = await import(fixture);
const service = await TrellisService.connect({
  trellisUrl,
  participant: participants.Provider.participant,
  seed,
  runtime: { health: { publishIntervalMs: 1000 } },
}).orThrow();
await service.wait();
`;
const fixtureUrl = (fixture: string) =>
  pathToFileURL(
    join(
      import.meta.dirname,
      `../../integration/fixtures/${fixture}/packages/runtime-trellis/index.js`,
    ),
  ).href;

type Listed = {
  instanceId: string;
  status: {
    state: string;
    connectedAt?: string;
    lastHeartbeat?: string;
    health?: {
      status: string;
      failingChecks: { name: string; status: string; error?: string }[];
    };
  };
};

test("the CLI and the Console show an unhealthy instance's failing check and nothing for a healthy one", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.deployments.create({ id: "checked", contract });
    await runtime.contracts.apply({ deployment: "checked", contract });
    const healthyKey = await runtime.services.createInstance({
      name: "checked-healthy",
      deployment: "checked",
      contract,
    });
    const failingKey = await runtime.services.createInstance({
      name: "checked-failing",
      deployment: "checked",
      contract,
    });
    const degradedKey = await runtime.services.createInstance({
      name: "checked-degraded",
      deployment: "checked",
      contract,
    });
    const stalledKey = await runtime.services.createInstance({
      name: "checked-stalled",
      deployment: "checked",
      contract,
    });
    // Its build asks for more than the installed one, so it stays connected
    // waiting for approval and never sends a heartbeat.
    const unreportedKey = await runtime.services.createInstance({
      name: "checked-unreported",
      deployment: "checked",
      contract,
    });
    const deploymentId = healthyKey.deploymentId;
    const launch = (seed: string, fixture: string) =>
      spawn(
        process.execPath,
        [
          "--no-warnings",
          "--input-type=module",
          "-e",
          program,
          runtime.trellisUrl,
          seed,
          fixtureUrl(fixture),
        ],
        {
          // Its output (the waiting one prints the review link) only
          // explains a failure.
          stdio: ["ignore", "pipe", "pipe"],
          env: {
            ...process.env,
            NODE_EXTRA_CA_CERTS: runtime.caCertificatePath,
          },
        },
      );
    const stalled = launch(stalledKey.seed, "runtime");
    const unreported = launch(unreportedKey.seed, "runtime-pinned");
    let childOutput = "";
    for (const child of [stalled, unreported])
      for (const stream of [child.stdout, child.stderr])
        stream.on("data", (chunk: Buffer) => (childOutput += chunk.toString()));
    const connect = (seed: string) =>
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed,
        runtime: { health: { publishIntervalMs: 1000 } },
      }).orThrow();
    const healthy = await connect(healthyKey.seed);
    const failing = await connect(failingKey.seed);
    const degraded = await connect(degradedKey.seed);
    healthy.health.add("database", () => ({ status: "ok" }));
    failing.health.add("database", () => ({
      status: "failed",
      error: "connection refused",
    }));
    degraded.health.add("cache", () => ({
      status: "degraded",
      summary: "serving from one replica",
    }));
    const exits = [healthy, failing, degraded].map((service) =>
      service.wait().catch((error: unknown) => error),
    );
    const browser = await chromium.launch({
      headless: true,
      env: browserTrustEnvironment(),
    });
    const context = await launchProfile(runtime);
    try {
      const { directory, run } = await loginCliAsAdministrator(
        runtime,
        await browser.newPage(),
      );
      const listed = async () =>
        new Map(
          (
            (await run("svc", deploymentId, "instances")) as {
              instances: Listed[];
            }
          ).instances.map((item) => [item.instanceId, item.status]),
        );
      // Wait until both have reported their check on this connection.
      let byId = new Map<string, Listed["status"]>();
      await runtime.waitFor(
        async () => {
          byId = await listed();
          return (
            byId.get(healthyKey.instanceId)?.health?.status === "healthy" &&
            (byId.get(failingKey.instanceId)?.health?.failingChecks.length ??
              0) > 0 &&
            (byId.get(degradedKey.instanceId)?.health?.failingChecks.length ??
              0) > 0 &&
            byId.get(stalledKey.instanceId)?.health?.status === "healthy" &&
            byId.get(unreportedKey.instanceId)?.state === "waitingForApproval"
          );
        },
        { timeoutMs: 30_000 },
      );
      assert.deepEqual(byId.get(healthyKey.instanceId)?.health, {
        status: "healthy",
        failingChecks: [],
      });
      assert.deepEqual(byId.get(failingKey.instanceId)?.health, {
        status: "unhealthy",
        failingChecks: [
          { name: "database", status: "failed", error: "connection refused" },
        ],
      });
      assert.deepEqual(byId.get(degradedKey.instanceId)?.health, {
        status: "degraded",
        failingChecks: [
          {
            name: "cache",
            status: "degraded",
            summary: "serving from one replica",
          },
        ],
      });

      // One process stops; its connection stays open, its heartbeats end.
      assert(stalled.pid);
      process.kill(stalled.pid, "SIGSTOP");
      await runtime.waitFor(
        async () => {
          byId = await listed();
          return (
            byId.get(stalledKey.instanceId)?.health?.status === "offline" ||
            byId.get(stalledKey.instanceId)?.connectedAt === undefined
          );
        },
        { timeoutMs: 30_000 },
      );
      const stalledStatus = byId.get(stalledKey.instanceId);
      assert(
        stalledStatus?.connectedAt,
        `the stopped process disconnected\n${childOutput}`,
      );
      assert.equal(stalledStatus.state, "serving");
      assert.deepEqual(stalledStatus.health, {
        status: "offline",
        failingChecks: [],
      });
      assert(stalledStatus.lastHeartbeat);
      const unreportedStatus = byId.get(unreportedKey.instanceId);
      assert.equal(unreportedStatus?.state, "waitingForApproval");
      assert(unreportedStatus.connectedAt);
      assert.equal(unreportedStatus.health, undefined);
      assert.equal(unreportedStatus.lastHeartbeat, undefined);

      // The text listing adds a line for each failing check only.
      const binary = process.env.TRELLIS_TEST_CLI_BIN;
      assert(binary);
      const { stdout } = await promisify(execFile)(
        binary,
        ["--format", "text", "svc", deploymentId, "instances"],
        {
          env: {
            ...process.env,
            XDG_CONFIG_HOME: directory,
            NO_COLOR: "1",
          },
          timeout: 15_000,
        },
      );
      const lines = stdout.split("\n");
      assert(
        lines.includes(
          `${failingKey.instanceId}: check database failed: connection refused`,
        ),
        stdout,
      );
      assert(
        lines.includes(
          `${degradedKey.instanceId}: check cache degraded: serving from one replica`,
        ),
        stdout,
      );
      assert(
        !lines.some((line) => line.startsWith(`${healthyKey.instanceId}:`)),
        stdout,
      );
      assert(
        lines.includes(
          `${stalledKey.instanceId}: heartbeats stopped; last heartbeat ${stalledStatus.lastHeartbeat}`,
        ),
        stdout,
      );
      assert(
        !lines.some((line) => line.startsWith(`${unreportedKey.instanceId}:`)),
        stdout,
      );
      // Piped, each row is tab-separated and carries its instance's health.
      const row = (instanceId: string) =>
        lines
          .map((line) => line.split("\t"))
          .find((cells) => cells.includes(instanceId));
      assert(row(healthyKey.instanceId)?.includes("healthy"), stdout);
      assert(row(failingKey.instanceId)?.includes("unhealthy"), stdout);
      assert(row(degradedKey.instanceId)?.includes("degraded"), stdout);
      assert(row(stalledKey.instanceId)?.includes("serving"), stdout);
      assert(row(stalledKey.instanceId)?.includes("offline"), stdout);
      assert(row(unreportedKey.instanceId), stdout);
      assert(!row(unreportedKey.instanceId)?.includes("offline"), stdout);

      // The Console's instance table names the same check.
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.consoleUrl}/console/admin/services/${deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      const running = (instanceId: string) =>
        page.locator(
          `tr[data-instance-id="${instanceId}"] [data-runtime-state]`,
        );
      await runtime.waitFor(
        async () =>
          (await running(failingKey.instanceId).getAttribute(
            "data-health-status",
          )) === "unhealthy",
        { timeoutMs: 30_000 },
      );
      const check = running(failingKey.instanceId).locator(
        '[data-failing-check="database"]',
      );
      assert.match(await check.innerText(), /connection refused/);
      assert.equal(
        await running(healthyKey.instanceId).getAttribute("data-health-status"),
        "healthy",
      );
      assert.equal(
        await running(healthyKey.instanceId)
          .locator("[data-failing-check]")
          .count(),
        0,
      );
      assert.equal(
        await running(healthyKey.instanceId).getByText("Healthy").count(),
        0,
      );
      // The stopped instance says its heartbeats stopped; the one that never
      // reported has no health at all.
      await running(stalledKey.instanceId)
        .locator("[data-heartbeats-stopped]")
        .waitFor();
      assert.equal(
        await running(stalledKey.instanceId).getAttribute("data-runtime-state"),
        "serving",
      );
      assert.equal(
        await running(stalledKey.instanceId).getAttribute("data-health-status"),
        "offline",
      );
      assert.equal(
        await running(unreportedKey.instanceId).getAttribute(
          "data-runtime-state",
        ),
        "waitingForApproval",
      );
      assert.equal(
        await running(unreportedKey.instanceId).getAttribute(
          "data-health-status",
        ),
        null,
      );
      assert.equal(
        await running(unreportedKey.instanceId)
          .locator("[data-heartbeats-stopped]")
          .count(),
        0,
      );
    } finally {
      for (const child of [stalled, unreported]) {
        if (child.pid && child.exitCode === null) {
          process.kill(child.pid, "SIGCONT");
          child.kill("SIGKILL");
        }
      }
      await context.close();
      await browser.close();
      await healthy.stop();
      await failing.stop();
      await degraded.stop();
      for (const exit of exits) assert.equal(await exit, undefined);
    }
  }, browserRuntimeOptions());
});
