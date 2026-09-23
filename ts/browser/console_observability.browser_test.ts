// Console observability journeys. Real runtime, real generated client.

import { assertEquals } from "@std/assert";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { Result } from "@oats-center/trellis";
import { TrellisService } from "@oats-center/trellis/service";
import type { TrellisTestRuntime } from "@oats-center/trellis-test";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  fixtureName,
  openConsoleAsRuntimeAdmin,
  waitForConsoleShell,
} from "./console_test_support.ts";

/**
 * Seeds a connected service provider so jobs, events, and health have real
 * records. The returned handle must stop before runtime teardown.
 */
async function seedProvider(runtime: TrellisTestRuntime, label: string) {
  const displayName = fixtureName(label);
  await runtime.deployments.create({ id: displayName, kind: "service" });
  const key = await runtime.registerService({
    name: displayName,
    contract: participants.Provider.participant,
    deployment: displayName,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: participants.Provider.participant,
    name: displayName,
    seed: key.seed,
  }).orThrow();
  await service.handleEcho(({ input }) => Result.ok(input));
  service.jobs.work.handle(({ job }) =>
    Promise.resolve(Result.ok(job.payload))
  );
  void service.wait().catch(() => {});
  return {
    deploymentId: key.deploymentId,
    displayName,
    service,
    async stop() {
      await service.stop();
    },
  };
}

Deno.test("B37 Jobs renders a real primary result and type summary", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b37-provider");
    try {
      const created = await seeded.service.jobs.work.create({
        value: "b37-job",
      }).orThrow();
      await created.wait().orThrow();

      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(`${runtime.trellisUrl}/console/admin/jobs`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main.getByRole("heading", { name: "Jobs", exact: true, level: 1 })
          .waitFor({ state: "visible", timeout: 30_000 });
        // The default focus is running-risk, so select the Processed ledger
        // item to bring the finished seeded job into the primary result.
        await main.getByRole("button", { name: /Processed/ }).first().click();
        // The primary job query renders regardless of optional panels: each
        // result row links to its exact job detail route.
        const jobDetailLinks = main.locator('a[href^="/console/admin/jobs/"]');
        await jobDetailLinks.first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assertEquals(
          await jobDetailLinks.count() > 0,
          true,
          "the primary job query returned at least the seeded job",
        );
        await main.getByText("Job-type health").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("B38 job state summary excludes the focused state filter", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b38-provider");
    try {
      const created = await seeded.service.jobs.work.create({
        value: "b38-job",
      }).orThrow();
      await created.wait().orThrow();

      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.trellisUrl}/console/admin/jobs`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main.getByText("Job-type health").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        // The ledger is a state ledger, not a summary of the focused tab.
        await main.getByText("Completed", { exact: false }).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assertEquals(
          await main.getByText("matching types").count() > 0,
          true,
        );
        await main.getByText(/\d+ in scope \(all states\)/).waitFor({
          state: "visible",
          timeout: 30_000,
        });
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("B41 Events renders its primary query table", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b41-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.trellisUrl}/console/admin/events`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main.getByRole("heading", {
          name: "Events",
          exact: true,
          level: 1,
        })
          .waitFor({ state: "visible", timeout: 30_000 });
        // The primary event table is present even if optional panels fail.
        await main.getByRole("table").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("N13 Events refreshes a verified provider event during continuous publication", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "n13-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(
          `${runtime.trellisUrl}/console/admin/events?focus=all`,
          {
            waitUntil: "domcontentloaded",
          },
        );
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main.getByText("Recent event flow").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        await main.getByText(/^Updated /).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        await main.getByText("Live", { exact: true }).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        const publishedEvent = main.locator("tbody tr")
          .filter({ hasText: "runtime-trellis.runtime@v1 / Changed" })
          .filter({ hasText: seeded.deploymentId });
        assertEquals(await publishedEvent.count(), 0);

        let stopProducing = false;
        let producerDone = false;
        const producer = (async () => {
          try {
            for (let index = 0; !stopProducing; index++) {
              await seeded.service.publishChanged({
                value: `n13-event-${index}`,
              })
                .orThrow();
              await new Promise((resolve) => setTimeout(resolve, 75));
            }
          } finally {
            producerDone = true;
          }
        })();
        try {
          await Promise.race([
            publishedEvent.first().waitFor({
              state: "visible",
              timeout: 20_000,
            }),
            producer,
          ]);
          await publishedEvent.first().getByText("verified", { exact: true })
            .waitFor({
              state: "visible",
              timeout: 5_000,
            });
          assertEquals(producerDone, false);
        } finally {
          stopProducing = true;
          await producer;
        }
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("B44 Health renders its real participant query", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b44-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.trellisUrl}/console/admin/health-events`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main.getByRole("heading", {
          name: "Participant Health",
          exact: true,
        }).waitFor({ state: "visible", timeout: 30_000 });
        // The participant table is the primary read; the summary is separate.
        await main.getByText("Participants", { exact: true }).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("B45 a completed job appears during continuous updates", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b45-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.trellisUrl}/console/admin/jobs`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main.getByText("Job-type health").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });

        await main.getByRole("button", { name: /Processed/ }).first().click();
        await main.locator(".jobs-updated").waitFor({ state: "visible" });
        await main.locator(".jobs-updated span").getByText("Live", {
          exact: true,
        }).waitFor({
          state: "visible",
          timeout: 10_000,
        });
        let producerDone = false;
        let stopProducer = false;
        const createdJobIds = new Set<string>();
        let firstCreated = () => {};
        let firstCompletion: Promise<unknown> | undefined;
        const firstJob = new Promise<void>((resolve) => {
          firstCreated = resolve;
        });
        const producer = (async () => {
          try {
            for (let index = 0; !stopProducer; index += 1) {
              const created = await seeded.service.jobs.work.create({
                value: `b45-${index}`,
              }).orThrow();
              createdJobIds.add(created.id);
              if (index === 0) {
                firstCompletion = created.wait().orThrow();
                firstCreated();
              }
              await new Promise((resolve) => setTimeout(resolve, 75));
            }
          } finally {
            producerDone = true;
          }
        })();
        try {
          await Promise.race([firstJob, producer]);
          await Promise.race([firstCompletion!, producer]);
          const visibleProduced = async () => {
            const hrefs = await main.locator(
              'a[href^="/console/admin/jobs/"]',
            ).evaluateAll(
              (links) => links.map((link) => link.getAttribute("href")),
            );
            return hrefs.some((href) =>
              href !== null &&
              createdJobIds.has(
                decodeURIComponent(href.slice("/console/admin/jobs/".length)),
              )
            );
          };
          try {
            await Promise.race([
              runtime.waitFor(visibleProduced, { timeoutMs: 20_000 }),
              producer,
            ]);
          } catch (cause) {
            const liveMatch = await visibleProduced();
            const status = await main.locator(".jobs-updated")
              .allTextContents();
            await main.getByRole("button", { name: "Refresh", exact: true })
              .first().click();
            let manualMatch = false;
            try {
              await runtime.waitFor(visibleProduced, { timeoutMs: 8_000 });
              manualMatch = true;
            } catch { /* Preserve the original failure. */ }
            console.error("B45 persisted-visibility check", {
              producedCount: createdJobIds.size,
              liveMatch,
              manualMatch,
              status,
            });
            throw cause;
          }
          assertEquals(
            producerDone,
            false,
            "a job must appear while the producer is still active",
          );
        } finally {
          stopProducer = true;
          await producer;
        }
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("B47 overview has no inert controls and labels snapshots", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const main = page.locator("main");
      await main.getByRole("heading", { name: "Overview", exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });

      // The inert time-range control is gone and a working refresh remains.
      assertEquals(
        await main.getByText("Last 5 minutes").count(),
        0,
        "the nonfunctional time-range control must be removed",
      );
      await main.getByRole("button", { name: "Refresh" }).waitFor({
        state: "visible",
      });
      // A snapshot label replaces invented live/last-seen values.
      await main.getByText(/Snapshot|Not loaded yet/).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("Z07 Events filter and refresh go through one snapshot scheduler", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "z07-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(
          `${runtime.trellisUrl}/console/admin/events?focus=all`,
          { waitUntil: "domcontentloaded" },
        );
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main.getByRole("heading", {
          name: "Events",
          exact: true,
          level: 1,
        })
          .waitFor({ state: "visible", timeout: 30_000 });
        await main.getByRole("table").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        await main.getByText(/^Updated /).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        const before = await main.getByText(/^Updated /).first().textContent();
        await main.getByRole("button", { name: "15m", exact: true }).click();
        await main.getByText(/^Updated /).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        await main.getByRole("button", { name: "Refresh", exact: true })
          .click();
        await main.getByText(/^Updated /).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        const after = await main.getByText(/^Updated /).first().textContent();
        assertEquals(typeof before, "string");
        assertEquals(typeof after, "string");
        await main.getByRole("table").first().waitFor({ state: "visible" });
        assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("Z10 Events visibility recovery refreshes a retained notification", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "z10-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(
          `${runtime.trellisUrl}/console/admin/events?focus=all`,
          { waitUntil: "domcontentloaded" },
        );
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main.getByText("Recent event flow").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        await main.getByText("Live", { exact: true }).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        const publishedEvent = main.locator("tbody tr")
          .filter({ hasText: "runtime-trellis.runtime@v1 / Changed" })
          .filter({ hasText: seeded.deploymentId });
        assertEquals(await publishedEvent.count(), 0);

        await page.evaluate(() => {
          Object.defineProperty(document, "visibilityState", {
            configurable: true,
            get: () => "hidden",
          });
          Object.defineProperty(document, "hidden", {
            configurable: true,
            get: () => true,
          });
          document.dispatchEvent(new Event("visibilitychange"));
        });
        await seeded.service.publishChanged({ value: `z10-event-${ulid()}` })
          .orThrow();
        await new Promise((resolve) => setTimeout(resolve, 500));
        assertEquals(
          await publishedEvent.count(),
          0,
          "a hidden page must not apply the snapshot until it is visible again",
        );

        await page.evaluate(() => {
          Object.defineProperty(document, "visibilityState", {
            configurable: true,
            get: () => "visible",
          });
          Object.defineProperty(document, "hidden", {
            configurable: true,
            get: () => false,
          });
          document.dispatchEvent(new Event("visibilitychange"));
        });
        await publishedEvent.first().waitFor({
          state: "visible",
          timeout: 20_000,
        });
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});
