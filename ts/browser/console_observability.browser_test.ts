// Console observability journeys. Real runtime, real generated client.

import assert from "node:assert/strict";
import { test } from "node:test";
import { Buffer } from "node:buffer";
import type { Page } from "playwright";
import { ulid } from "ulid";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { API as eventsApi } from "../packages/trellis/internal_sdk/generated/apis/events/mod.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  fixtureName,
  openConsoleAsRuntimeAdmin,
  waitForConsoleShell,
} from "./console_test_support.ts";
import { NatsResponseGate } from "./nats_response_gate.ts";

/**
 * Seeds a connected service provider so jobs, events, and health have real
 * records. The returned handle must stop before runtime teardown.
 */
async function seedProvider(runtime: TrellisTestRuntime, label: string) {
  const displayName = fixtureName(label);
  const key = await runtime.registerService({
    name: displayName,
    contract: participants.Provider.participant,
    deployment: displayName,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.oauthIssuer,
    participant: participants.Provider.participant,
    name: displayName,
    seed: key.seed,
  }).orThrow();
  await service.handleEcho(({ input }) => Result.ok(input));
  service.jobs.work.handle(({ job }) =>
    Promise.resolve(Result.ok(job.payload)),
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

const EVENTS_QUERY = eventsApi.actions["rpc:Query"].subject;
const EVENTS_METRICS = eventsApi.actions["rpc:Metrics"].subject;
const EVENTS_DIAGNOSTICS = eventsApi.actions["rpc:Diagnostics"].subject;
const EVENTS_CONSUMERS_QUERY = eventsApi.actions["rpc:Consumers.Query"].subject;
const EVENTS_CONSUMERS_INSPECT =
  eventsApi.actions["rpc:Consumers.Inspect"].subject;
const EVENTS_DEAD_LETTERS_QUERY =
  eventsApi.actions["rpc:DeadLetters.Query"].subject;
const EVENTS_DEAD_LETTERS_INSPECT =
  eventsApi.actions["rpc:DeadLetters.Inspect"].subject;
const EVENTS_DEAD_LETTERS_DISMISS =
  eventsApi.actions["rpc:DeadLetters.Dismiss"].subject;
const EVENTS_PAGE_QUERIES = [
  EVENTS_QUERY,
  EVENTS_METRICS,
  EVENTS_DIAGNOSTICS,
  EVENTS_CONSUMERS_QUERY,
  EVENTS_CONSUMERS_INSPECT,
  EVENTS_DEAD_LETTERS_QUERY,
  EVENTS_DEAD_LETTERS_INSPECT,
];

function toWsBytes(message: string | Buffer): Uint8Array {
  return typeof message === "string"
    ? new TextEncoder().encode(message)
    : new Uint8Array(message);
}

function sendWs(
  socket: { send: (message: string | Buffer) => void },
  chunks: Uint8Array[],
): void {
  for (const chunk of chunks) socket.send(Buffer.from(chunk));
}

async function wireNatsGate(page: Page): Promise<{
  gate: NatsResponseGate;
  releaseToPage: () => Uint8Array[];
}> {
  const gate = new NatsResponseGate();
  let pageSocket: { send: (message: string | Buffer) => void } | undefined;
  await page.routeWebSocket(
    (url) => url.protocol === "ws:" || url.protocol === "wss:",
    (ws) => {
      const server = ws.connectToServer();
      pageSocket = ws;
      ws.onMessage((message) => {
        sendWs(server, gate.ingestClient(toWsBytes(message)));
      });
      server.onMessage((message) => {
        sendWs(ws, gate.ingestServer(toWsBytes(message)));
      });
    },
  );
  return {
    gate,
    releaseToPage() {
      if (!pageSocket) return [];
      const chunks = gate.release();
      sendWs(pageSocket, chunks);
      return chunks;
    },
  };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

test("B37 Jobs renders a real primary result and type summary", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b37-provider");
    try {
      const created = await seeded.service.jobs.work
        .create({
          value: "b37-job",
        })
        .orThrow();
      await created.wait().orThrow();

      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);
        await assertNoBrowserErrors(errors);

        await page.goto(`${runtime.trellisUrl}/console/admin/jobs`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main
          .getByRole("heading", { name: "Jobs", exact: true, level: 1 })
          .waitFor({ state: "visible", timeout: 30_000 });
        // The default focus is running-risk, so select the Processed ledger
        // item to bring the finished seeded job into the primary result.
        await main
          .getByRole("button", { name: /Processed/ })
          .first()
          .click();
        // The primary job query renders regardless of optional panels: each
        // result row links to its exact job detail route.
        const jobDetailLinks = main.locator('a[href^="/console/admin/jobs/"]');
        await jobDetailLinks.first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assert.deepEqual(
          (await jobDetailLinks.count()) > 0,
          true,
          "the primary job query returned at least the seeded job",
        );
        await main.getByText("Job-type health").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

test("B38 job state summary excludes the focused state filter", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b38-provider");
    try {
      const created = await seeded.service.jobs.work
        .create({
          value: "b38-job",
        })
        .orThrow();
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
        assert.deepEqual(
          (await main.getByText("matching types").count()) > 0,
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

test("B41 Events renders its primary query table", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b41-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);
        await assertNoBrowserErrors(errors);
        await page.goto(`${runtime.trellisUrl}/console/admin/events`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main
          .getByRole("heading", {
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
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

test("N13 Events refreshes a verified provider event during continuous publication", async () => {
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
        await main
          .getByText(/^Updated /)
          .first()
          .waitFor({
            state: "visible",
            timeout: 30_000,
          });
        await main.getByText("Live", { exact: true }).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        const publishedEvent = main
          .locator("tbody tr")
          .filter({ hasText: "runtime-trellis.runtime@v1 / Changed" })
          .filter({ hasText: seeded.deploymentId });
        assert.deepEqual(await publishedEvent.count(), 0);

        let stopProducing = false;
        let producerDone = false;
        const producer = (async () => {
          try {
            for (let index = 0; !stopProducing; index++) {
              await seeded.service
                .publishChanged({
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
          await publishedEvent
            .first()
            .getByText("verified", { exact: true })
            .waitFor({
              state: "visible",
              timeout: 5_000,
            });
          assert.deepEqual(producerDone, false);
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

test("B44 Health renders its real participant query", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "b44-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);
        await assertNoBrowserErrors(errors);
        await page.goto(`${runtime.trellisUrl}/console/admin/health-events`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main
          .getByRole("heading", {
            name: "Participant Health",
            exact: true,
          })
          .waitFor({ state: "visible", timeout: 30_000 });
        // The participant table is the primary read; the summary is separate.
        await main.getByText("Participants", { exact: true }).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

test("B45 a completed job appears during continuous updates", async () => {
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

        await main
          .getByRole("button", { name: /Processed/ })
          .first()
          .click();
        await main.locator(".jobs-updated").waitFor({ state: "visible" });
        await main
          .locator(".jobs-updated span")
          .getByText("Live", {
            exact: true,
          })
          .waitFor({
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
              const created = await seeded.service.jobs.work
                .create({
                  value: `b45-${index}`,
                })
                .orThrow();
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
            const hrefs = await main
              .locator('a[href^="/console/admin/jobs/"]')
              .evaluateAll((links) =>
                links.map((link) => link.getAttribute("href")),
              );
            return hrefs.some(
              (href) =>
                href !== null &&
                createdJobIds.has(
                  decodeURIComponent(href.slice("/console/admin/jobs/".length)),
                ),
            );
          };
          try {
            await Promise.race([
              runtime.waitFor(visibleProduced, { timeoutMs: 20_000 }),
              producer,
            ]);
          } catch (cause) {
            const liveMatch = await visibleProduced();
            const status = await main
              .locator(".jobs-updated")
              .allTextContents();
            await main
              .getByRole("button", { name: "Refresh", exact: true })
              .first()
              .click();
            let manualMatch = false;
            try {
              await runtime.waitFor(visibleProduced, { timeoutMs: 8_000 });
              manualMatch = true;
            } catch {
              /* Preserve the original failure. */
            }
            console.error("B45 persisted-visibility check", {
              producedCount: createdJobIds.size,
              liveMatch,
              manualMatch,
              status,
            });
            throw cause;
          }
          assert.deepEqual(
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

test("B47 overview refresh loads an instance provisioned after the initial snapshot", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await assertNoBrowserErrors(errors);
      await page.goto(`${runtime.trellisUrl}/console/admin`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const main = page.locator("main");
      await main
        .getByRole("heading", { name: "Overview", exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });

      await main
        .getByText(/^Snapshot /)
        .first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      const fresh = await seedProvider(runtime, "b47-fresh-instance");
      try {
        await main
          .getByRole("button", { name: "Refresh", exact: true })
          .click();
        await main
          .getByText(fresh.deploymentId, { exact: true })
          .first()
          .waitFor({ state: "visible" });
        await assertNoBrowserErrors(errors);
      } finally {
        await fresh.stop();
      }
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("Z10 Events visibility recovery refreshes a retained notification", async () => {
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
        const publishedEvent = main
          .locator("tbody tr")
          .filter({ hasText: "runtime-trellis.runtime@v1 / Changed" })
          .filter({ hasText: seeded.deploymentId });
        assert.deepEqual(await publishedEvent.count(), 0);

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
        await seeded.service
          .publishChanged({ value: `z10-event-${ulid()}` })
          .orThrow();
        await new Promise((resolve) => setTimeout(resolve, 500));
        assert.deepEqual(
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
          timeout: 60_000,
        });
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

test("Z06 permitted Events data survives denied consumer-management authority", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const credentials = {
      username: `z06-${ulid().toLowerCase().slice(-8)}`,
      password: "z06-ordinary-password",
    };
    const principalId = await createOrdinaryUserWithPassword(
      runtime,
      credentials,
    );
    for (const capabilityId of [
      "trellis.events@v1::query",
      "trellis.events@v1::metrics",
    ]) {
      const current = await runtime.callAdminRpc("authCapabilityGrantsList", {
        principalId,
        capabilityId,
      });
      await runtime.callAdminRpc("authCapabilityGrantsGrant", {
        principalId,
        capabilityId,
        expectedRevision: current.items[0]?.revision ?? 0n,
        idempotencyKey: ulid(),
      });
    }
    const seeded = await seedProvider(runtime, "z06-provider");
    const marker = `z06-event-${ulid()}`;
    try {
      await seeded.service.publishChanged({ value: marker }).orThrow();
      const reader = await runtime.connectClient({
        name: fixtureName("z06-reader"),
        contract: apps.Observer.request,
      });
      await runtime.waitFor(
        async () => {
          const items = (
            await reader
              .eventsQuery({
                publisherDeploymentId: seeded.deploymentId,
                window: "1h",
              })
              .orThrow()
          ).items;
          return items.length > 0 ? true : undefined;
        },
        { timeoutMs: 30_000 },
      );

      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsole(page, runtime, credentials);
        await waitForConsoleShell(page);
        await page.locator('summary[aria-label="Open user menu"]').click();
        await page.getByText("Member", { exact: true }).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });

        await assertNoBrowserErrors(errors);
        await page.goto(
          `${runtime.trellisUrl}/console/admin/events?focus=all`,
          { waitUntil: "domcontentloaded" },
        );
        await waitForConsoleShell(page);
        const main = page.locator("main");
        await main
          .getByRole("heading", {
            name: "Events",
            exact: true,
            level: 1,
          })
          .waitFor({ state: "visible", timeout: 30_000 });
        const publishedEvent = main
          .locator("tbody tr")
          .filter({ hasText: "runtime-trellis.runtime@v1 / Changed" })
          .filter({ hasText: seeded.deploymentId });
        await publishedEvent.first().waitFor({
          state: "visible",
          timeout: 30_000,
        });

        const ledger = main.locator('[aria-label="Event health summary"]');
        const consumers = ledger.locator("button").filter({
          hasText: "Consumers",
        });
        await runtime.waitFor(
          async () =>
            (await consumers.locator("strong").innerText()).trim() ===
            "Unavailable",
          { timeoutMs: 15_000 },
        );
        assert.deepEqual(
          await consumers.locator("small").innerText(),
          "Consumer health unavailable",
        );
        const oldestLag = ledger.locator("button").filter({
          hasText: "Oldest lag",
        });
        assert.deepEqual(
          (await oldestLag.locator("strong").innerText()).trim(),
          "Unknown",
        );
        const eventFlow = ledger.locator("button").filter({
          hasText: "Event flow",
        });
        const eventTotal = Number(
          (await eventFlow.locator("strong").innerText()).trim(),
        );
        assert(
          Number.isFinite(eventTotal) && eventTotal > 0,
          "the permitted metrics summary stays numeric while consumers are denied",
        );
        await main
          .getByRole("button", { name: "Refresh", exact: true })
          .click();
        await publishedEvent.first().waitFor({ state: "visible" });
        await runtime.waitFor(
          async () =>
            (await consumers.locator("strong").innerText()).trim() ===
            "Unavailable",
          { timeoutMs: 15_000 },
        );
        assert.deepEqual(
          await consumers.locator("small").innerText(),
          "Consumer health unavailable",
        );
        assert.deepEqual(new URL(page.url()).pathname, "/console/admin/events");
        assert.deepEqual(await page.getByText("Sign in required").count(), 0);
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

test("Z07 deferred page refresh stays single-flight under live invalidation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvider(runtime, "z07-deferred-provider");
    try {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        const wired = await wireNatsGate(page);
        try {
          await openConsoleAsRuntimeAdmin(page, runtime);
          await assertNoBrowserErrors(errors);
          await page.goto(
            `${runtime.trellisUrl}/console/admin/events?focus=all`,
            { waitUntil: "domcontentloaded" },
          );
          await waitForConsoleShell(page);
          const main = page.locator("main");
          await main
            .getByRole("heading", {
              name: "Events",
              exact: true,
              level: 1,
            })
            .waitFor({ state: "visible", timeout: 30_000 });
          await main.getByText("Live", { exact: true }).first().waitFor({
            state: "visible",
            timeout: 30_000,
          });
          await main
            .getByText(/^Updated /)
            .first()
            .waitFor({
              state: "visible",
              timeout: 30_000,
            });

          const mark = wired.gate.requestMark();
          const held = wired.gate.armNextReply(EVENTS_QUERY);
          await main
            .getByRole("button", { name: "Refresh", exact: true })
            .click();
          await held;
          await wired.gate.waitHeld();
          for (let index = 0; index < 3; index++) {
            await seeded.service
              .publishChanged({
                value: `z07-held-${index}-${ulid()}`,
              })
              .orThrow();
          }
          await sleep(400);
          assert.deepEqual(
            wired.gate.requestsSince(mark, [EVENTS_QUERY]),
            1,
            "live invalidations must not start a second Query group",
          );

          const trailing = wired.gate.armNextReply(EVENTS_QUERY);
          wired.releaseToPage();
          await trailing;
          assert.deepEqual(
            wired.gate.requestsSince(mark, [EVENTS_QUERY]),
            2,
            "exactly one trailing Query group follows the held refresh",
          );
          wired.releaseToPage();
          await main
            .getByText(/^Updated /)
            .first()
            .waitFor({
              state: "visible",
              timeout: 30_000,
            });
          await assertNoBrowserErrors(errors);
        } finally {
          wired.gate.dispose();
        }
      } finally {
        await context.close();
      }
    } finally {
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

test("Z08 page discards superseded results and drains the latest query", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seededA = await seedProvider(runtime, "z08-a");
    const seededB = await seedProvider(runtime, "z08-b");
    try {
      // Publish before the page loads and confirm projection through the
      // ordinary generated query, so establishing the initial rows does not
      // depend on a first live invalidation racing the initial snapshot.
      await seededA.service
        .publishChanged({ value: `z08-a-${ulid()}` })
        .orThrow();
      await seededB.service
        .publishChanged({ value: `z08-b-${ulid()}` })
        .orThrow();
      const reader = await runtime.connectClient({
        name: fixtureName("z08-reader"),
        contract: apps.Observer.request,
      });
      await runtime.waitFor(
        async () => {
          const a = (
            await reader
              .eventsQuery({
                publisherDeploymentId: seededA.deploymentId,
                window: "1h",
              })
              .orThrow()
          ).items;
          const b = (
            await reader
              .eventsQuery({
                publisherDeploymentId: seededB.deploymentId,
                window: "1h",
              })
              .orThrow()
          ).items;
          return a.length > 0 && b.length > 0 ? true : undefined;
        },
        { timeoutMs: 60_000 },
      );

      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        const wired = await wireNatsGate(page);
        try {
          await openConsoleAsRuntimeAdmin(page, runtime);
          await assertNoBrowserErrors(errors);
          await page.goto(
            `${runtime.trellisUrl}/console/admin/events?focus=all`,
            { waitUntil: "domcontentloaded" },
          );
          await waitForConsoleShell(page);
          const main = page.locator("main");
          await main.getByText("Live", { exact: true }).first().waitFor({
            state: "visible",
            timeout: 30_000,
          });
          const search = main.getByPlaceholder("Search event metadata");
          const rowA = main
            .locator("tbody tr")
            .filter({ hasText: "runtime-trellis.runtime@v1 / Changed" })
            .filter({ hasText: seededA.deploymentId });
          const rowB = main
            .locator("tbody tr")
            .filter({ hasText: "runtime-trellis.runtime@v1 / Changed" })
            .filter({ hasText: seededB.deploymentId });
          await rowA.first().waitFor({ state: "visible", timeout: 60_000 });
          await rowB.first().waitFor({ state: "visible", timeout: 60_000 });

          // Commit real metadata scope A through the search control.
          await search.click();
          await search.pressSequentially(seededA.deploymentId);
          await search.press("Enter");
          await runtime.waitFor(async () => (await rowB.count()) === 0, {
            timeoutMs: 15_000,
          });
          await rowA.first().waitFor({ state: "visible", timeout: 15_000 });

          const mark = wired.gate.requestMark();
          const heldA = wired.gate.armNextReply(EVENTS_QUERY);
          await main
            .getByRole("button", { name: "Refresh", exact: true })
            .click();
          await heldA;

          // Commit B while A's read is pending: it must be queued, not parallel.
          await search.click();
          await search.press("Control+a");
          await search.pressSequentially(seededB.deploymentId);
          await search.press("Enter");
          assert.deepEqual(await search.inputValue(), seededB.deploymentId);
          await sleep(150);
          assert.deepEqual(
            wired.gate.requestsSince(mark, [EVENTS_QUERY]),
            1,
            "committing B must not dispatch a parallel Query group",
          );

          const heldB = wired.gate.armNextReply(EVENTS_QUERY);
          wired.releaseToPage();
          await heldB;
          assert.deepEqual(
            wired.gate.requestsSince(mark, [EVENTS_QUERY]),
            2,
            "exactly one B Query follows the discarded A reply",
          );
          assert.deepEqual(
            await rowA.count(),
            0,
            "late A must not populate B while B is still held",
          );
          await main.getByText("Loading event health").waitFor({
            state: "visible",
            timeout: 15_000,
          });
          wired.releaseToPage();
          await rowB.first().waitFor({ state: "visible", timeout: 15_000 });
          assert.deepEqual(await rowA.count(), 0);
          assert.deepEqual(await search.inputValue(), seededB.deploymentId);

          // A timed notification followed by a manual refresh inside the 250 ms
          // window replaces the pending timer instead of reading twice.
          const timerMark = wired.gate.requestMark();
          const frame = wired.gate.armNextPush();
          await seededB.service
            .publishChanged({ value: `z08-timer-${ulid()}` })
            .orThrow();
          await Promise.race([
            frame,
            sleep(10_000).then(() => {
              throw new Error("the live watch frame did not reach the browser");
            }),
          ]);
          await main
            .getByRole("button", { name: "Refresh", exact: true })
            .click();
          await sleep(400);
          assert.deepEqual(
            wired.gate.requestsSince(timerMark, [EVENTS_QUERY]),
            1,
            "manual refresh inside the coalescing window replaces the pending timer",
          );
          await assertNoBrowserErrors(errors);
        } finally {
          wired.gate.dispose();
        }
      } finally {
        await context.close();
      }
    } finally {
      await seededB.stop();
      await seededA.stop();
    }
  }, browserRuntimeOptions());
});

test("Z09 Events page disposes an in-flight dead-letter dismissal without follow-up", async () => {
  await withTrellisRuntime(async (runtime) => {
    const displayName = fixtureName("z09-events");
    const key = await runtime.registerService({
      name: displayName,
      contract: participants.EventService.participant,
      deployment: displayName,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.EventService.participant,
      name: displayName,
      seed: key.seed,
    }).orThrow();
    const attempts: bigint[] = [];
    const successes: string[] = [];
    await service
      .onAlpha(
        async ({ event }) => {
          attempts.push(BigInt(Date.now()));
          if (event.value === "fail") {
            throw new Error("fixture handler failure");
          }
          successes.push(event.value);
        },
        {},
        { mode: "durable", group: "events" },
      )
      .orThrow();
    // The declared consumer group covers Alpha and Beta; the pump only starts
    // once every declared event has a registration.
    await service
      .onBeta(() => {}, {}, { mode: "durable", group: "events" })
      .orThrow();
    await service.handleDeliveryStats(() =>
      Result.ok({
        active: 0n,
        attempts: [...attempts],
        maxActive: 0n,
        successes: [...successes],
      }),
    );
    void service.wait().catch(() => {});
    try {
      const alpha = await runtime.connectClient({
        name: fixtureName("z09-alpha"),
        contract: apps.Alpha.request,
      });
      // The consumer only receives events published after it is attached, so
      // prove attachment with one ordinary successful delivery first.
      await runtime.waitFor(
        async () => {
          const stats = await alpha.deliveryStats({}, { timeout: 1_000 });
          return stats.isOk();
        },
        { timeoutMs: 60_000 },
      );
      const consumer = await runtime.waitFor(
        async () => {
          const entry = (await runtime.events.consumersQuery({})).items.find(
            (item) => item.deploymentId === key.deploymentId,
          );
          return entry ?? false;
        },
        { timeoutMs: 60_000 },
      );
      await alpha.publishAlpha({ site: "z09", value: "ready" }).orThrow();
      await runtime.waitFor(
        async () => {
          const stats = await alpha.deliveryStats({}, { timeout: 1_000 });
          return stats.isOk() && stats.orThrow().successes.includes("ready");
        },
        { timeoutMs: 30_000 },
      );
      await alpha.publishAlpha({ site: "z09", value: "fail" }).orThrow();
      const deadLetter = await runtime.waitFor(
        async () => {
          const rows = (
            await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
              state: ["dead"],
            })
          ).items;
          return rows[0] ?? false;
        },
        { timeoutMs: 60_000 },
      );

      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        const wired = await wireNatsGate(page);
        try {
          await openConsoleAsRuntimeAdmin(page, runtime);
          await assertNoBrowserErrors(errors);
          await page.goto(
            `${runtime.trellisUrl}/console/admin/events?focus=all`,
            { waitUntil: "domcontentloaded" },
          );
          await waitForConsoleShell(page);
          const main = page.locator("main");
          await main
            .getByRole("heading", {
              name: "Events",
              exact: true,
              level: 1,
            })
            .waitFor({ state: "visible", timeout: 30_000 });
          await main
            .getByRole("button", {
              name: key.deploymentId,
              exact: true,
            })
            .first()
            .click();
          await main.getByText(deadLetter.deadLetterId).waitFor({
            state: "visible",
            timeout: 30_000,
          });
          const held = wired.gate.armNextReply(EVENTS_DEAD_LETTERS_DISMISS);
          await main
            .getByRole("button", { name: "Dismiss", exact: true })
            .click();
          await page
            .getByRole("button", { name: "Dismiss", exact: true })
            .last()
            .click();
          await held;
          const committed = await runtime.waitFor(
            async () => {
              const rows = (
                await runtime.events.deadLettersQuery({
                  resourceId: consumer.resourceId,
                })
              ).items.filter(
                (row) => row.deadLetterId === deadLetter.deadLetterId,
              );
              const row = rows[0];
              return row && row.state !== "dead" ? row : false;
            },
            { timeoutMs: 15_000 },
          );
          assert(committed.state !== "dead", "dismiss commits on the server");

          await page.locator('summary[aria-label="Open user menu"]').click();
          await page
            .getByRole("banner")
            .getByRole("link", {
              name: "Account",
              exact: true,
            })
            .click();
          await page
            .getByRole("heading", {
              name: "Account Access Ledger",
              exact: true,
            })
            .waitFor({ state: "visible", timeout: 30_000 });
          assert.deepEqual(
            await main
              .getByRole("heading", {
                name: "Events",
                exact: true,
                level: 1,
              })
              .count(),
            0,
          );

          const mark = wired.gate.requestMark();
          wired.releaseToPage();
          await sleep(1200);
          assert.deepEqual(
            wired.gate.requestsSince(mark, EVENTS_PAGE_QUERIES),
            0,
            "disposed Events page must not follow up",
          );
          assert.deepEqual(
            wired.gate.requestsSince(mark, [EVENTS_DEAD_LETTERS_DISMISS]),
            0,
          );
          const stillCommitted = (
            await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
            })
          ).items.find((row) => row.deadLetterId === deadLetter.deadLetterId);
          assert(stillCommitted, "dismiss remains committed");
          assert.deepEqual(stillCommitted.state, committed.state);
          // The same document and WebSocket stayed alive through SPA navigation.
          await main
            .getByRole("heading", {
              name: "Account Access Ledger",
              exact: true,
            })
            .waitFor({ state: "visible", timeout: 15_000 });

          // Scope-change variant: a held dismissal must not touch a new scope.
          await alpha.publishAlpha({ site: "z09", value: "fail" }).orThrow();
          const second = await runtime.waitFor(
            async () => {
              const rows = (
                await runtime.events.deadLettersQuery({
                  resourceId: consumer.resourceId,
                })
              ).items.filter(
                (row) =>
                  row.deadLetterId !== deadLetter.deadLetterId &&
                  row.state === "dead",
              );
              return rows[0] ?? false;
            },
            { timeoutMs: 60_000 },
          );
          await assertNoBrowserErrors(errors);
          await page.goto(
            `${runtime.trellisUrl}/console/admin/events?focus=all`,
            { waitUntil: "domcontentloaded" },
          );
          await waitForConsoleShell(page);
          await main
            .getByRole("heading", {
              name: "Events",
              exact: true,
              level: 1,
            })
            .waitFor({ state: "visible", timeout: 30_000 });
          await main
            .getByRole("button", {
              name: key.deploymentId,
              exact: true,
            })
            .first()
            .click();
          await main.getByText(second.deadLetterId).waitFor({
            state: "visible",
            timeout: 30_000,
          });
          const heldSecond = wired.gate.armNextReply(
            EVENTS_DEAD_LETTERS_DISMISS,
          );
          await main
            .getByRole("button", { name: "Dismiss", exact: true })
            .click();
          await page
            .getByRole("button", { name: "Dismiss", exact: true })
            .last()
            .click();
          await heldSecond;
          const secondCommitted = await runtime.waitFor(
            async () => {
              const rows = (
                await runtime.events.deadLettersQuery({
                  resourceId: consumer.resourceId,
                })
              ).items.filter((row) => row.deadLetterId === second.deadLetterId);
              const row = rows[0];
              return row && row.state !== "dead" ? row : false;
            },
            { timeoutMs: 15_000 },
          );

          await main.getByRole("button", { name: "15m", exact: true }).click();
          await main.getByText("Select a consumer").waitFor({
            state: "visible",
            timeout: 15_000,
          });
          await runtime.waitFor(
            async () =>
              await main
                .getByRole("button", { name: "Refresh", exact: true })
                .isEnabled(),
            { timeoutMs: 15_000 },
          );
          const variantMark = wired.gate.requestMark();
          wired.releaseToPage();
          await sleep(1200);
          assert.deepEqual(
            wired.gate.requestsSince(variantMark, EVENTS_PAGE_QUERIES),
            0,
            "an obsolete dismissal continuation must not refresh the new scope",
          );
          assert.deepEqual(
            await main.getByText("Select a consumer").count(),
            1,
            "the new scope presentation is unchanged",
          );
          assert.deepEqual(
            await main.getByText("Outcome unknown.", { exact: false }).count(),
            0,
          );
          const secondStill = (
            await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
            })
          ).items.find((row) => row.deadLetterId === second.deadLetterId);
          assert(secondStill, "the second dismissal remains committed");
          assert.deepEqual(secondStill.state, secondCommitted.state);
          assert.deepEqual(
            new URL(page.url()).pathname,
            "/console/admin/events",
          );
          await assertNoBrowserErrors(errors);
        } finally {
          wired.gate.dispose();
        }
      } finally {
        await context.close();
      }
    } finally {
      await service.stop();
    }
  }, browserRuntimeOptions());
});
