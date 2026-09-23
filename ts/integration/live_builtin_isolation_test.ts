/**
 * Real-boundary built-in Feed consumer isolation acceptance (BI02).
 *
 * Two public Console consumers open the same built-in Health, Jobs, or Events
 * Feed against one all-in-one runtime. Closing one consumer must release
 * exactly that consumer's live session while the second consumer keeps
 * receiving new domain frames and the surviving connection still serves
 * unrelated finite RPCs.
 */

import { Result } from "@oatscenter/trellis";
import type { CallerRuntime } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "npm:@opentelemetry/sdk-metrics@^2.7.0";

import { participants as webParticipants } from "trellis-web-generated";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../target/debug/trellis-server", import.meta.url),
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

/** Connects one fixture Provider service so Jobs and Events have a publisher. */
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

/**
 * In-process metric capture rebinding Trellis instruments to a test reader.
 * Mirrors the shared capture used by the lifecycle and observability cases.
 */
function startMetricCapture() {
  const exporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const reader = new PeriodicExportingMetricReader({
    exporter,
    exportIntervalMillis: 60_000,
  });
  const provider = new MeterProvider({ readers: [reader] });
  metrics.setGlobalMeterProvider(provider);
  const total = (name: string, subset: Record<string, string> = {}): number => {
    let sum = 0;
    const scopes = exporter.getMetrics().at(-1)?.scopeMetrics ?? [];
    for (const metric of scopes.flatMap((scope) => scope.metrics)) {
      if (metric.descriptor.name !== name) continue;
      for (const point of metric.dataPoints) {
        if (
          Object.entries(subset).every(([key, value]) =>
            point.attributes[key] === value
          )
        ) {
          sum += point.value as number;
        }
      }
    }
    return sum;
  };
  return { total, flush: () => provider.forceFlush() };
}

let capture: ReturnType<typeof startMetricCapture> | undefined;
function ensureCapture(): ReturnType<typeof startMetricCapture> {
  capture ??= startMetricCapture();
  return capture;
}

/**
 * Closes consumer A's Feed handle and proves exactly one consumer-side live
 * end is released for it.
 */
async function closeConsumerA(
  metricsCapture: ReturnType<typeof startMetricCapture>,
  runtime: Runtime,
  iteratorA: AsyncIterator<unknown>,
  pumpA: Promise<void>,
  abortA: AbortController,
): Promise<void> {
  await metricsCapture.flush();
  const beforeEnds = metricsCapture.total("trellis.live.ends", {
    "trellis.kind": "standalone",
    "trellis.side": "consumer",
  });
  abortA.abort();
  await iteratorA.return?.();
  await pumpA.catch(() => undefined);
  await runtime.waitFor(async () => {
    await metricsCapture.flush();
    return metricsCapture.total("trellis.live.ends", {
          "trellis.kind": "standalone",
          "trellis.side": "consumer",
        }) - beforeEnds >= 1;
  }, { timeoutMs: 15_000 });
  await metricsCapture.flush();
  assertEquals(
    metricsCapture.total("trellis.live.ends", {
      "trellis.kind": "standalone",
      "trellis.side": "consumer",
    }) - beforeEnds,
    1,
    "closing consumer A must release exactly one feed consumer",
  );
}

/** Proves a surviving consumer still answers an unrelated finite RPC. */
async function assertFiniteRpcSurvives(client: ConsoleClient): Promise<void> {
  const sessions = await client.sessionsList({}).orThrow();
  assert(
    Array.isArray(sessions.items),
    "unrelated finite RPC on the surviving connection must succeed",
  );
}

Deno.test("BI02 closing one of two Health.Watch consumers leaves the other serving", async () => {
  const metricsCapture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    const clientA = await runtime.connectClient({
      name: `bi02-health-a-${crypto.randomUUID()}`,
      contract: webParticipants.Console.participant,
    });
    const clientB = await runtime.connectClient({
      name: `bi02-health-b-${crypto.randomUUID()}`,
      contract: webParticipants.Console.participant,
    });
    const abortA = new AbortController();
    const abortB = new AbortController();
    const framesA: Record<string, unknown>[] = [];
    const framesB: Record<string, unknown>[] = [];
    let trigger: ProviderService | undefined;
    try {
      const iteratorA = (await clientA.healthWatch({}).orThrow())[
        Symbol.asyncIterator
      ]();
      const iteratorB = (await clientB.healthWatch({}).orThrow())[
        Symbol.asyncIterator
      ]();
      const pumpA = pumpInto(iteratorA, framesA);
      const pumpB = pumpInto(iteratorB, framesB);
      await runtime.waitFor(
        () =>
          framesA.some((frame) => frame.type === "ready") &&
          framesB.some((frame) => frame.type === "ready"),
        { timeoutMs: 30_000 },
      );

      await closeConsumerA(metricsCapture, runtime, iteratorA, pumpA, abortA);
      const baselineB = framesB.length;

      // A fresh service heartbeat is a real Health change the surviving
      // consumer must still observe.
      trigger = await connectProvider(
        runtime,
        `bi02-health-trigger-${crypto.randomUUID()}`,
      );
      await runtime.waitFor(
        () =>
          framesB.slice(baselineB).some((frame) =>
            frame.type === "healthInvalidated"
          ),
        { timeoutMs: 30_000 },
      );

      await assertFiniteRpcSurvives(clientB);

      abortB.abort();
      await iteratorB.return?.();
      await pumpB.catch(() => undefined);
    } finally {
      await trigger?.stop();
      abortA.abort();
      abortB.abort();
      await clientA.connection.close();
      await clientB.connection.close();
    }
  }, runtimeOptions);
});

Deno.test("BI02 closing one of two Jobs.Watch consumers leaves the other serving", async () => {
  const metricsCapture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    const service = await connectProvider(
      runtime,
      `bi02-jobs-provider-${crypto.randomUUID()}`,
    );
    await service.jobs.work.handle(({ job }) =>
      Promise.resolve(Result.ok(job.payload))
    );
    const clientA = await runtime.connectClient({
      name: `bi02-jobs-a-${crypto.randomUUID()}`,
      contract: webParticipants.Console.participant,
    });
    const clientB = await runtime.connectClient({
      name: `bi02-jobs-b-${crypto.randomUUID()}`,
      contract: webParticipants.Console.participant,
    });
    const abortA = new AbortController();
    const abortB = new AbortController();
    const framesA: Record<string, unknown>[] = [];
    const framesB: Record<string, unknown>[] = [];
    try {
      const iteratorA = (await clientA.jobsWatch(
        { includeInitial: false },
        { signal: abortA.signal },
      ).orThrow())[Symbol.asyncIterator]();
      const iteratorB = (await clientB.jobsWatch(
        { includeInitial: false },
        { signal: abortB.signal },
      ).orThrow())[Symbol.asyncIterator]();
      const pumpA = pumpInto(iteratorA, framesA);
      const pumpB = pumpInto(iteratorB, framesB);
      await runtime.waitFor(
        () =>
          framesA.some((frame) => frame.kind === "ready") &&
          framesB.some((frame) => frame.kind === "ready"),
        { timeoutMs: 30_000 },
      );

      await closeConsumerA(metricsCapture, runtime, iteratorA, pumpA, abortA);
      const baselineB = framesB.length;

      await service.jobs.work.create({
        value: `bi02-jobs-${crypto.randomUUID()}`,
      }).orThrow();
      await runtime.waitFor(
        () =>
          framesB.slice(baselineB).some((frame) =>
            frame.kind === "queryInvalidated"
          ),
        { timeoutMs: 30_000 },
      );

      await assertFiniteRpcSurvives(clientB);

      abortB.abort();
      await iteratorB.return?.();
      await pumpB.catch(() => undefined);
    } finally {
      await service.stop();
      abortA.abort();
      abortB.abort();
      await clientA.connection.close();
      await clientB.connection.close();
    }
  }, runtimeOptions);
});

Deno.test("T09 disposing one of two scopes on one client leaves the other serving", async () => {
  const metricsCapture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    const service = await connectProvider(
      runtime,
      `t09-jobs-provider-${crypto.randomUUID()}`,
    );
    await service.jobs.work.handle(({ job }) =>
      Promise.resolve(Result.ok(job.payload))
    );
    // One client connection owns both scopes.
    const client = await runtime.connectClient({
      name: `t09-jobs-${crypto.randomUUID()}`,
      contract: webParticipants.Console.participant,
    });
    const abortA = new AbortController();
    const abortB = new AbortController();
    const framesA: Record<string, unknown>[] = [];
    const framesB: Record<string, unknown>[] = [];
    try {
      const iteratorA = (await client.jobsWatch(
        { includeInitial: false },
        { signal: abortA.signal },
      ).orThrow())[Symbol.asyncIterator]();
      const iteratorB = (await client.jobsWatch(
        { includeInitial: false },
        { signal: abortB.signal },
      ).orThrow())[Symbol.asyncIterator]();
      const pumpA = pumpInto(iteratorA, framesA);
      const pumpB = pumpInto(iteratorB, framesB);
      await runtime.waitFor(
        () =>
          framesA.some((frame) => frame.kind === "ready") &&
          framesB.some((frame) => frame.kind === "ready"),
        { timeoutMs: 30_000 },
      );

      await closeConsumerA(metricsCapture, runtime, iteratorA, pumpA, abortA);
      const baselineB = framesB.length;

      await service.jobs.work.create({
        value: `t09-jobs-${crypto.randomUUID()}`,
      }).orThrow();
      await runtime.waitFor(
        () =>
          framesB.slice(baselineB).some((frame) =>
            frame.kind === "queryInvalidated"
          ),
        { timeoutMs: 30_000 },
      );

      await assertFiniteRpcSurvives(client);

      abortB.abort();
      await iteratorB.return?.();
      await pumpB.catch(() => undefined);
    } finally {
      await service.stop();
      abortA.abort();
      abortB.abort();
      await client.connection.close();
    }
  }, runtimeOptions);
});

Deno.test("BI02 closing one of two Events.Watch consumers leaves the other serving", async () => {
  const metricsCapture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    const service = await connectProvider(
      runtime,
      `bi02-events-provider-${crypto.randomUUID()}`,
    );
    const published = `bi02-events-${crypto.randomUUID()}`;
    const matchesPublished = (frame: Record<string, unknown>): boolean => {
      const events = frame.events;
      if (!Array.isArray(events)) return false;
      return events.some((event) => {
        // The decoded Feed frame carries the event payload as raw bytes.
        const payload = (event as { payload?: unknown }).payload;
        if (!(payload instanceof Uint8Array)) return false;
        try {
          const decoded = JSON.parse(
            new TextDecoder().decode(payload),
          ) as { value?: unknown };
          return decoded.value === published;
        } catch {
          return false;
        }
      });
    };
    const clientA = await runtime.connectClient({
      name: `bi02-events-a-${crypto.randomUUID()}`,
      contract: webParticipants.Console.participant,
    });
    const clientB = await runtime.connectClient({
      name: `bi02-events-b-${crypto.randomUUID()}`,
      contract: webParticipants.Console.participant,
    });
    const abortA = new AbortController();
    const abortB = new AbortController();
    const framesA: Record<string, unknown>[] = [];
    const framesB: Record<string, unknown>[] = [];
    try {
      const iteratorA = (await clientA.eventsWatch({}, {
        signal: abortA.signal,
      }).orThrow())[Symbol.asyncIterator]();
      const iteratorB = (await clientB.eventsWatch({}, {
        signal: abortB.signal,
      }).orThrow())[Symbol.asyncIterator]();
      const pumpA = pumpInto(iteratorA, framesA);
      const pumpB = pumpInto(iteratorB, framesB);
      // Events.Watch has no ready frame; one real publish proves both
      // consumers are admitted before A is closed.
      await runtime.waitFor(async () => {
        if (framesA.length > 0 && framesB.length > 0) return true;
        await service.publishChanged({
          value: `bi02-events-prime-${crypto.randomUUID()}`,
        }).orThrow();
        return false;
      }, { timeoutMs: 30_000 });

      await closeConsumerA(metricsCapture, runtime, iteratorA, pumpA, abortA);
      const baselineB = framesB.length;

      await service.publishChanged({ value: published }).orThrow();
      await runtime.waitFor(
        () => framesB.slice(baselineB).some(matchesPublished),
        { timeoutMs: 30_000 },
      );

      await assertFiniteRpcSurvives(clientB);

      abortB.abort();
      await iteratorB.return?.();
      await pumpB.catch(() => undefined);
    } finally {
      await service.stop();
      abortA.abort();
      abortB.abort();
      await clientA.connection.close();
      await clientB.connection.close();
    }
  }, runtimeOptions);
});
