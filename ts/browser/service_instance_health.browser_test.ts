/**
 * An administrator sees which instance of a service is unhealthy and why:
 * the CLI's `svc <ID> instances`, as text and as JSON, and the Console's
 * instance table name the failing check and its message, and say nothing
 * more of a healthy instance. One failed check makes an instance unhealthy
 * even though the SDK's own connection check passes, and a degraded check
 * makes it degraded, as a Rust service reports the same checks.
 */
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { test } from "node:test";
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

type Listed = {
  instanceId: string;
  status: {
    state: string;
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
    const deploymentId = healthyKey.deploymentId;
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
              0) > 0
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
      // Piped, each row is tab-separated and carries its instance's health.
      const row = (instanceId: string) =>
        lines
          .map((line) => line.split("\t"))
          .find((cells) => cells.includes(instanceId));
      assert(row(healthyKey.instanceId)?.includes("healthy"), stdout);
      assert(row(failingKey.instanceId)?.includes("unhealthy"), stdout);
      assert(row(degradedKey.instanceId)?.includes("degraded"), stdout);

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
    } finally {
      await context.close();
      await browser.close();
      await healthy.stop();
      await failing.stop();
      await degraded.stop();
      for (const exit of exits) assert.equal(await exit, undefined);
    }
  }, browserRuntimeOptions());
});
