import { assertEquals, assertInstanceOf } from "@std/assert";

import {
  observeTrellisConnection,
  TrellisConnection,
  type TrellisConnectionStatus,
  type TrellisConnectionStatusTransport,
} from "./connection.ts";

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

Deno.test("observeTrellisConnection publishes error transition from status stream", async () => {
  const stream = new FakeStatusStream();
  const connection = observeTrellisConnection({
    kind: "client",
    transport: stream,
  });
  const error = new Error("status failed");

  stream.push({ type: "error", error });
  await delay();

  assertEquals(connection.status.phase, "error");
  assertEquals(connection.status.transport?.error, error);
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
