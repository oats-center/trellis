import { assertEquals, assertInstanceOf } from "@std/assert";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";
import { trackConnection, trackCoverage } from "./telemetry/lifecycle.ts";

import {
  observeTrellisConnection,
  startConnectionTelemetry,
  transitionConnectionAvailability,
  TrellisConnection,
  type TrellisConnectionStatus,
  type TrellisConnectionStatusTransport,
} from "./connection.ts";
import { installAuthorizationRefresh } from "./auth/authorization/install_refresh.ts";

class FakeStatusStream implements TrellisConnectionStatusTransport {
  #events: unknown[] = [];
  #waiting: (() => void) | undefined;
  #closed = false;
  #closedPromise: Promise<void | Error>;
  #resolveClosed: (value: void | Error) => void = () => {};
  #rejectClosed: (error: unknown) => void = () => {};
  closeCalls = 0;

  constructor(private readonly server = "nats://127.0.0.1:4222") {
    this.#closedPromise = new Promise((resolve, reject) => {
      this.#resolveClosed = resolve;
      this.#rejectClosed = reject;
    });
  }

  async *status(): AsyncIterable<unknown> {
    while (!this.#closed) {
      if (this.#events.length === 0) {
        await new Promise<void>((resolve) => {
          this.#waiting = resolve;
        });
      }

      while (this.#events.length > 0) {
        yield this.#events.shift();
      }
    }
  }

  closed(): Promise<void | Error> {
    return this.#closedPromise;
  }

  close(): Promise<void> {
    this.closeCalls += 1;
    this.resolveClosed();
    return Promise.resolve();
  }

  isClosed(): boolean {
    return this.#closed;
  }

  getServer(): string {
    return this.server;
  }

  push(event: unknown): void {
    this.#events.push(event);
    this.#waiting?.();
    this.#waiting = undefined;
  }

  resolveClosed(error?: Error): void {
    if (this.#closed) {
      return;
    }

    this.#closed = true;
    this.#waiting?.();
    this.#waiting = undefined;
    this.#resolveClosed(error);
  }

  rejectClosed(error: unknown): void {
    this.#closed = true;
    this.#waiting?.();
    this.#rejectClosed(error);
  }
}

function delay(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

Deno.test("numeric connection and coverage sources survive first collection and clear to zero", async () => {
  const exporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const reader = new PeriodicExportingMetricReader({
    exporter,
    exportIntervalMillis: 60_000,
  });
  const provider = new MeterProvider({ readers: [reader] });
  metrics.setGlobalMeterProvider(provider);
  const first = trackConnection("user");
  const stopFirst = trackCoverage(() => ({
    own: false,
    peerCovered: 0,
    peerUnavailable: 0,
  }));
  await provider.forceFlush();
  first.dispose();
  stopFirst();
  const second = trackConnection("service");
  second.transition("usable", "connected");
  const stopSecond = trackCoverage(() => ({
    own: true,
    peerCovered: 2,
    peerUnavailable: 0,
  }));
  await provider.forceFlush();
  const samples =
    exporter.getMetrics().at(-1)?.scopeMetrics.flatMap((scope) =>
      scope.metrics
    ) ?? [];
  const points = (name: string) =>
    samples.find((metric) => metric.descriptor.name === name)
      ?.dataPoints ?? [];
  assertEquals(
    points("trellis.connection.count").some((point) =>
      point.attributes["trellis.participant.kind"] === "service" &&
      point.attributes["trellis.state"] === "usable" && point.value === 1
    ),
    true,
  );
  assertEquals(
    points("trellis.connection.count").some((point) =>
      point.attributes["trellis.participant.kind"] === "user" &&
      point.value === 0
    ),
    true,
  );
  assertEquals(
    points("trellis.auth.coverage.count").some((point) =>
      point.attributes["trellis.kind"] === "peer" &&
      point.attributes["trellis.state"] === "covered" && point.value === 2
    ),
    true,
  );
  second.dispose();
  stopSecond();
  await provider.forceFlush();
  assertEquals(
    (exporter.getMetrics().at(-1)?.scopeMetrics.flatMap((scope) =>
      scope.metrics
    )
      .find((metric) => metric.descriptor.name === "trellis.connection.count")
      ?.dataPoints ?? []).some((point) =>
        point.attributes["trellis.participant.kind"] === "service" &&
        point.attributes["trellis.state"] === "usable" && point.value === 0
      ),
    true,
  );
  await provider.shutdown();
  metrics.disable();
});

Deno.test("production connection owner publishes usable, suspended, resumed, and clears on terminal", async () => {
  const exporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const reader = new PeriodicExportingMetricReader({
    exporter,
    exportIntervalMillis: 60_000,
  });
  const provider = new MeterProvider({ readers: [reader] });
  metrics.setGlobalMeterProvider(provider);
  const states = async () => {
    await provider.forceFlush();
    return (exporter.getMetrics().at(-1)?.scopeMetrics.flatMap((scope) =>
      scope.metrics
    ).find((metric) => metric.descriptor.name === "trellis.connection.count")
      ?.dataPoints ?? []).map((point) =>
        [
          String(point.attributes["trellis.participant.kind"]),
          String(point.attributes["trellis.state"]),
          point.value,
        ] as const
      );
  };
  for (const kind of ["service", "device"] as const) {
    const stream = new FakeStatusStream();
    const connection = observeTrellisConnection({
      kind,
      transport: stream,
      telemetry: startConnectionTelemetry(kind),
    });
    try {
      // The attempt is counted from creation, before any promotion.
      assertEquals(
        (await states()).some(([participantKind, state, value]) =>
          participantKind === kind && state === "connecting" && value === 1
        ),
        true,
      );
      // Bootstrap publication promotes the attempt after verification.
      transitionConnectionAvailability(connection, true, "connected");
      const usable = await states();
      assertEquals(
        usable.some(([participantKind, state, value]) =>
          participantKind === kind && state === "usable" && value === 1
        ),
        true,
      );
      assertEquals(
        usable.some(([participantKind, state, value]) =>
          participantKind === kind && state === "connecting" && value === 0
        ),
        true,
      );
      // Withdrawn own coverage suspends without counting a second connection.
      transitionConnectionAvailability(connection, false, "coverage_lost");
      const suspended = await states();
      assertEquals(
        suspended.some(([participantKind, state, value]) =>
          participantKind === kind && state === "usable" && value === 0
        ),
        true,
      );
      assertEquals(
        suspended.some(([participantKind, state, value]) =>
          participantKind === kind && state === "suspended" && value === 1
        ),
        true,
      );
      transitionConnectionAvailability(connection, true, "resumed");
      const resumed = await states();
      assertEquals(
        resumed.filter(([participantKind, state]) =>
          participantKind === kind && state === "usable"
        ).every(([, , value]) => value === 1),
        true,
      );
      assertEquals(
        resumed.some(([participantKind, state, value]) =>
          participantKind === kind && state === "suspended" && value === 0
        ),
        true,
      );
      // A diagnostic transport error is not a terminal authority decision:
      // the semantic count stays usable until a real close disposes it.
      stream.push({ type: "error", error: new Error("diagnostic") });
      await new Promise((resolve) => setTimeout(resolve, 0));
      const diagnostic = await states();
      assertEquals(
        diagnostic.some(([participantKind, state, value]) =>
          participantKind === kind && state === "usable" && value === 1
        ),
        true,
      );
      assertEquals(
        diagnostic.some(([participantKind, state, value]) =>
          participantKind === kind && state === "terminal" && value === 1
        ),
        false,
      );
      // A real close still records terminal exactly once.
      stream.resolveClosed();
      await new Promise((resolve) => setTimeout(resolve, 0));
      const terminal = await states();
      assertEquals(
        terminal.some(([participantKind, state, value]) =>
          participantKind === kind && state === "terminal" && value === 1
        ),
        true,
      );
      assertEquals(
        terminal.some(([participantKind, state, value]) =>
          participantKind === kind && state === "usable" && value === 0
        ),
        true,
      );
      await connection.close();
      // Final cleanup unregisters the attempt and clears its aggregates to zero.
      const cleared = await states();
      assertEquals(
        cleared.filter(([participantKind]) => participantKind === kind).every(
          ([, , value]) => value === 0,
        ),
        true,
      );
    } finally {
      await connection.close();
    }
  }
  await provider.shutdown();
  metrics.disable();
});

Deno.test("TrellisConnection starts connected and delivers current status on subscribe", () => {
  const connection = new TrellisConnection({ kind: "client" });
  const received: TrellisConnectionStatus[] = [];

  connection.subscribe((status) => received.push(status));

  assertEquals(connection.status.kind, "client");
  assertEquals(connection.status.phase, "connected");
  assertEquals(received.length, 1);
  assertEquals(received[0]?.phase, "connected");
  assertInstanceOf(received[0]?.observedAt, Date);
});

Deno.test("observeTrellisConnection maps transport lifecycle transitions", async () => {
  const stream = new FakeStatusStream();
  const connection = observeTrellisConnection({
    kind: "service",
    transport: stream,
    transportName: "fake",
  });
  const phases: string[] = [];
  connection.subscribe((status) => phases.push(status.phase));

  stream.push({ type: "disconnect" });
  await delay();
  stream.push({ type: "reconnecting" });
  await delay();
  stream.push({ type: "forceReconnect" });
  await delay();
  stream.push({ type: "staleConnection" });
  await delay();
  stream.push({ type: "reconnect" });
  await delay();

  assertEquals(phases, [
    "connected",
    "disconnected",
    "reconnecting",
    "reconnecting",
    "reconnecting",
    "connected",
  ]);
  assertEquals(connection.status.transport, {
    name: "fake",
    server: "nats://127.0.0.1:4222",
    event: "reconnect",
  });
});

Deno.test("observeTrellisConnection publishes close transition from close", async () => {
  const stream = new FakeStatusStream();
  const connection = observeTrellisConnection({
    kind: "device",
    transport: stream,
  });
  const phases: string[] = [];
  connection.subscribe((status) => phases.push(status.phase));

  await connection.close();

  assertEquals(stream.closeCalls, 1);
  assertEquals(connection.status.phase, "closed");
  assertEquals(phases, ["connected", "closed"]);
});

Deno.test("a diagnostic transport error keeps the logical phase and reaches raw observers", async () => {
  const stream = new FakeStatusStream();
  const events: Array<{ event: unknown; planned: boolean }> = [];
  const connection = observeTrellisConnection({
    kind: "client",
    transport: stream,
    onTransportEvent: (event, planned) => events.push({ event, planned }),
  });
  const phases: string[] = [];
  connection.subscribe((status) => phases.push(status.phase));
  const error = new Error("diagnostic");

  stream.push({ type: "error", error });
  await delay();

  // A raw transport error is not a logical lifecycle decision: the current
  // phase is retained and the diagnostic still reaches raw bookkeeping.
  assertEquals(connection.status.phase, "connected");
  assertEquals(phases, ["connected"]);
  assertEquals(events.length, 1);
  assertEquals(events[0]?.planned, false);
  assertEquals((events[0]?.event as { error?: unknown })?.error, error);
});

Deno.test("installAuthorizationRefresh keeps a healthy rotation out of the logical lifecycle", async () => {
  const stream = new FakeStatusStream();
  const connection = observeTrellisConnection({
    kind: "client",
    transport: stream,
  });
  const phases: string[] = [];
  connection.subscribe((status) => phases.push(status.phase));
  const calls: string[] = [];
  const provider = {
    waitReady: () => {
      calls.push("waitReady");
      return Promise.resolve();
    },
    connectionGeneration: () => {
      calls.push("generation");
      return 7;
    },
    retainOwnCandidate: (digest: string, generation: number) => {
      calls.push(`retain:${digest}:${generation}`);
      return Promise.resolve();
    },
    promoteOwnCandidate: (digest: string, generation: number) => {
      calls.push(`promote:${digest}:${generation}`);
    },
    abandonRotation: () => {
      calls.push("abandon");
    },
  };
  try {
    // A healthy attachment rotates as planned maintenance: the physical
    // disconnect/reconnect must never reach the logical lifecycle.
    await installAuthorizationRefresh({
      connection,
      provider,
      contextDigest: "candidate-digest",
      reconnect: () => {
        calls.push("reconnect");
        stream.push({ type: "disconnect" });
        stream.push({ type: "reconnecting" });
        stream.push({ type: "reconnect" });
        return Promise.resolve();
      },
    });

    assertEquals(connection.status.phase, "connected");
    assertEquals(phases, ["connected"]);
    assertEquals(connection.live.isAvailable(), true);
    assertEquals(calls, [
      "reconnect",
      "waitReady",
      "generation",
      "retain:candidate-digest:7",
      "promote:candidate-digest:7",
    ]);
  } finally {
    await connection.close();
  }
});

Deno.test("installAuthorizationRefresh recovers an already-disconnected connection instead of suppressing its reconnect", async () => {
  const stream = new FakeStatusStream();
  const connection = observeTrellisConnection({
    kind: "client",
    transport: stream,
  });
  const phases: string[] = [];
  connection.subscribe((status) => phases.push(status.phase));
  const calls: string[] = [];
  let abandoned = false;
  const provider = {
    waitReady: async () => {
      const deadline = Date.now() + 2_000;
      while (
        connection.status.phase !== "connected" && Date.now() < deadline
      ) {
        await delay();
      }
      calls.push("waitReady");
    },
    connectionGeneration: () => {
      calls.push("generation");
      return 9;
    },
    retainOwnCandidate: (digest: string, generation: number) => {
      calls.push(`retain:${digest}:${generation}`);
      return Promise.resolve();
    },
    promoteOwnCandidate: (digest: string, generation: number) => {
      calls.push(`promote:${digest}:${generation}`);
    },
    abandonRotation: () => {
      abandoned = true;
    },
  };
  try {
    // A genuine transport outage: the logical connection is disconnected and
    // Live is suspended before the scheduled refresh fires.
    stream.push({ type: "disconnect" });
    await delay();
    assertEquals(connection.status.phase, "disconnected");
    assertEquals(connection.live.isAvailable(), false);
    assertEquals(connection.live.unavailableReason(), "epoch_changed");

    await installAuthorizationRefresh({
      connection,
      provider,
      contextDigest: "candidate-digest",
      reconnect: () => {
        calls.push("reconnect");
        stream.push({ type: "reconnecting" });
        stream.push({ type: "reconnect" });
        return Promise.resolve();
      },
    });

    // Ordinary recovery: the reconnect stays a real logical transition, so the
    // connection is connected again and Live accepts new sessions.
    assertEquals(connection.status.phase, "connected");
    assertEquals(connection.live.isAvailable(), true);
    assertEquals(connection.live.unavailableReason(), undefined);
    assertEquals(phases, [
      "connected",
      "disconnected",
      "reconnecting",
      "connected",
    ]);
    connection.live.admitConsumer()[Symbol.dispose]();
    assertEquals(abandoned, false);
    assertEquals(calls, [
      "reconnect",
      "waitReady",
      "generation",
      "retain:candidate-digest:9",
      "promote:candidate-digest:9",
    ]);
  } finally {
    await connection.close();
  }
});

Deno.test("observeTrellisConnection publishes error transition from closed result", async () => {
  const stream = new FakeStatusStream();
  const connection = observeTrellisConnection({
    kind: "client",
    transport: stream,
  });
  const error = new Error("closed failed");

  stream.resolveClosed(error);
  await delay();

  assertEquals(connection.status.phase, "error");
  assertEquals(connection.status.transport?.error, error);
});

Deno.test("observeTrellisConnection consumes a rejected transport close", async () => {
  const stream = new FakeStatusStream();
  const connection = observeTrellisConnection({
    kind: "client",
    transport: stream,
  });
  const error = new Error("read ECONNRESET");

  stream.rejectClosed(error);
  await delay();

  assertEquals(connection.status.phase, "error");
  assertEquals(connection.status.transport?.error, error);
  await connection.close();
});

Deno.test("TrellisConnection unsubscribe and stopObserving prevent later updates", async () => {
  const stream = new FakeStatusStream();
  const connection = observeTrellisConnection({
    kind: "service",
    transport: stream,
  });
  const phases: string[] = [];
  const unsubscribe = connection.subscribe((status) =>
    phases.push(status.phase)
  );

  unsubscribe();
  stream.push({ type: "disconnect" });
  await delay();
  connection.stopObserving();
  stream.push({ type: "reconnect" });
  stream.resolveClosed();
  await delay();

  assertEquals(phases, ["connected"]);
  assertEquals(connection.status.phase, "disconnected");
});
