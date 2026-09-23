import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert } from "@std/assert";
import { participants as webParticipants } from "trellis-web-generated";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

function jsonFrame(frame: unknown): Record<string, unknown> {
  if (frame instanceof Uint8Array) {
    return JSON.parse(new TextDecoder().decode(frame));
  }
  if (frame && typeof frame === "object") {
    return frame as Record<string, unknown>;
  }
  throw new Error(`unexpected watch frame: ${typeof frame}`);
}

Deno.test("L5 BI01 console client receives Health.Watch domain frame", async () => {
  await withTrellisRuntime(async (runtime) => {
    const client = await runtime.connectClient({
      name: "bi01-health",
      contract: webParticipants.Console.participant,
    });
    const feed = await client.healthWatch({}).orThrow();
    const first = await feed[Symbol.asyncIterator]().next();
    assert(
      !first.done,
      `Health.Watch ended without a frame: ${JSON.stringify(first)}`,
    );
    const frame = jsonFrame(first.value);
    assert(
      frame.type === "ready",
      `unexpected Health.Watch frame: ${JSON.stringify(frame)}`,
    );
    await feed[Symbol.asyncIterator]().return?.();
    await client.connection.close();
  });
});

Deno.test("Jobs.Watch emits live queryInvalidated after job create", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.registerService({
      name: "jobs-watch-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: "jobs-watch-provider",
      seed: key.seed,
    }).orThrow();
    try {
      service.jobs.work.handle(({ job }) =>
        Promise.resolve(Result.ok(job.payload))
      );
      const consoleClient = await runtime.connectClient({
        name: "jobs-watch-console",
        contract: webParticipants.Console.participant,
      });
      const abort = new AbortController();
      try {
        const stream = await consoleClient.jobsWatch(
          { includeInitial: false },
          {
            signal: abort.signal,
          },
        ).orThrow();
        const frames: Record<string, unknown>[] = [];
        const pump = (async () => {
          for await (const frame of stream) frames.push(jsonFrame(frame));
        })();
        await runtime.waitFor(() =>
          frames.some((frame) => frame.kind === "ready")
        );
        await service.jobs.work.create({ value: "watch-live" }).orThrow();
        await runtime.waitFor(() =>
          frames.some((frame) => frame.kind === "queryInvalidated")
        );
        abort.abort();
        await pump.catch(() => undefined);
      } finally {
        abort.abort();
        await consoleClient.connection.close();
      }
    } finally {
      await service.stop();
    }
  });
});

Deno.test("Events.Watch emits live frames after publishChanged", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.registerService({
      name: "events-watch-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: "events-watch-provider",
      seed: key.seed,
    }).orThrow();
    try {
      const consoleClient = await runtime.connectClient({
        name: "events-watch-console",
        contract: webParticipants.Console.participant,
      });
      const abort = new AbortController();
      try {
        const stream = await consoleClient.eventsWatch({}, {
          signal: abort.signal,
        }).orThrow();
        const frames: Record<string, unknown>[] = [];
        const pump = (async () => {
          for await (const frame of stream) frames.push(jsonFrame(frame));
        })();
        await runtime.waitFor(async () => {
          if (
            frames.some((frame) => {
              const events = frame.events;
              return Array.isArray(events) && events.length > 0;
            })
          ) return true;
          await service.publishChanged({ value: "watch-live" }).orThrow();
          return false;
        }, { timeoutMs: 30_000 });
        abort.abort();
        await pump.catch(() => undefined);
      } finally {
        abort.abort();
        await consoleClient.connection.close();
      }
    } finally {
      await service.stop();
    }
  });
});
