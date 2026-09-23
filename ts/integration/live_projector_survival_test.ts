/**
 * Real-boundary built-in projector survival acceptance (BI06).
 *
 * Public Console observers are opened on the built-in Jobs and Events Feeds
 * and then closed. With no observer active, the underlying Jobs execution and
 * Events capture machinery must keep advancing and stay readable through
 * ordinary finite RPCs, and the runtime must keep reporting ready.
 */

import { Result } from "@oatscenter/trellis";
import type { CallerRuntime } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert } from "@std/assert";
import { fromFileUrl } from "@std/path";

import { participants as webParticipants } from "trellis-web-generated";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../rust/target/debug/trellis-server", import.meta.url),
    );
}

const runtimeOptions = {
  trellis: {
    command: {
      cmd: serverBinary(),
      args: ["--config", "{config}", "all"],
    },
  },
};

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];
type ConsoleClient = CallerRuntime<typeof webParticipants.Console.participant>;

/** Connects one fixture Provider service that executes jobs and publishes. */
async function connectProvider(runtime: Runtime, name: string) {
  const identity = await runtime.registerService({
    name,
    contract: participants.Provider.participant,
  });
  return await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: participants.Provider.participant,
    name,
    seed: identity.seed,
  }).orThrow();
}
type ProviderService = Awaited<ReturnType<typeof connectProvider>>;

function jsonFrame(frame: unknown): Record<string, unknown> {
  if (frame instanceof Uint8Array) {
    return JSON.parse(new TextDecoder().decode(frame));
  }
  if (frame && typeof frame === "object") {
    return frame as Record<string, unknown>;
  }
  throw new Error(`unexpected watch frame: ${typeof frame}`);
}

function pumpInto<T>(
  iterator: AsyncIterator<T>,
  frames: Record<string, unknown>[],
): Promise<void> {
  return (async () => {
    while (true) {
      const next = await iterator.next();
      if (next.done) break;
      frames.push(jsonFrame(next.value));
    }
  })();
}

/** Opens a real Jobs observer, admits it, and closes it. */
async function observeAndCloseJobs(
  runtime: Runtime,
  client: ConsoleClient,
): Promise<void> {
  const frames: Record<string, unknown>[] = [];
  const iterator = (await client.jobsWatch({ includeInitial: false }).orThrow())
    [Symbol.asyncIterator]();
  const pump = pumpInto(iterator, frames);
  await runtime.waitFor(() => frames.some((frame) => frame.kind === "ready"), {
    timeoutMs: 30_000,
  });
  await iterator.return?.();
  await pump.catch(() => undefined);
}

/** Opens a real Events observer, admits it with a published event, and closes it. */
async function observeAndCloseEvents(
  runtime: Runtime,
  client: ConsoleClient,
  service: ProviderService,
): Promise<void> {
  const frames: Record<string, unknown>[] = [];
  const iterator = (await client.eventsWatch({}).orThrow())[
    Symbol.asyncIterator
  ]();
  const pump = pumpInto(iterator, frames);
  // Events.Watch has no initial frame; republish until the observer is
  // admitted so the admission event cannot be missed.
  await runtime.waitFor(async () => {
    if (frames.length > 0) return true;
    await service.publishChanged({
      value: `bi06-prime-${crypto.randomUUID()}`,
    }).orThrow();
    return false;
  }, { timeoutMs: 30_000 });
  await iterator.return?.();
  await pump.catch(() => undefined);
}

Deno.test("BI06 closing a public observer leaves built-in Jobs and Events projectors running", async () => {
  await withTrellisRuntime(async (runtime) => {
    const client = await runtime.connectClient({
      name: `bi06-console-${crypto.randomUUID()}`,
      contract: webParticipants.Console.participant,
    });
    const service = await connectProvider(
      runtime,
      `bi06-provider-${crypto.randomUUID()}`,
    );
    await service.jobs.work.handle(({ job }) =>
      Promise.resolve(Result.ok(job.payload))
    );
    // Connect registers handlers; `wait()` starts the job workers that execute
    // them. Close the observer first so execution is proven without one.
    const serviceExit = service.wait().catch((error: unknown) => error);
    const published = `bi06-events-${crypto.randomUUID()}`;
    try {
      // Open and close real public observers on both built-in Feeds before any
      // survival assertion, so no observer is active from here on.
      await observeAndCloseJobs(runtime, client);
      await observeAndCloseEvents(runtime, client, service);

      // Jobs execution and the built-in Jobs projection keep advancing.
      await service.jobs.work.create({
        value: `bi06-job-${crypto.randomUUID()}`,
      }).orThrow();
      await runtime.waitFor(async () => {
        const summary = await client.jobsSummary({ state: ["completed"] })
          .orThrow();
        return summary.stats.total >= 1n;
      }, { timeoutMs: 30_000 });

      // The Events projector keeps capturing and exposing published events.
      await service.publishChanged({ value: published }).orThrow();
      await runtime.waitFor(async () => {
        const page = await client.eventsQuery({ ownerEventName: "Changed" })
          .orThrow();
        return page.items.length >= 1;
      }, { timeoutMs: 30_000 });

      // The runtime stayed healthy through ordinary use.
      const ready = await fetch(`${runtime.trellisUrl}/readyz`);
      assert(
        ready.ok,
        `/readyz must stay healthy after observer close: ${ready.status}`,
      );
    } finally {
      await service.stop();
      await client.connection.close();
      assert(
        (await serviceExit) === undefined,
        "provider service must stop cleanly",
      );
    }
  }, runtimeOptions);
});
