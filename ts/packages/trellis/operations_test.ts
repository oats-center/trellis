import { assertEquals } from "@std/assert";
import { AsyncResult, isErr } from "@oatscenter/result";

import { codecs } from "./generated.ts";
import { OperationInvoker, type OperationTransport } from "./operations.ts";
import { ConsumerCore, LiveSubscription } from "./live/subscription.ts";
import { LiveCancellation, LiveEnd } from "./live/types.ts";

const SNAPSHOT = {
  id: "01J00000000000000000000000",
  service: "orders",
  operation: "Run",
  revision: 1,
  state: "pending" as const,
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z",
};

const COMPLETED = {
  ...SNAPSHOT,
  revision: 2,
  state: "completed" as const,
  updatedAt: "2026-01-01T00:00:01.000Z",
};

function completedWatch<T>(
  value: T,
  onDispose?: () => void,
): LiveSubscription<T> {
  const core = new ConsumerCore<T>("watch", "operation");
  core.setPhase("draining");
  core.admit({ value, encodedLen: 1 });
  core.setPendingEnd(new LiveEnd("complete"));
  return new LiveSubscription(
    core,
    new LiveCancellation(),
    () =>
      Promise.resolve({
        end: new LiveEnd("cancelled"),
        remote: "not-required",
        cleanup: "unknown",
      }),
    onDispose ? { [Symbol.dispose]: onDispose } : undefined,
  );
}

Deno.test("operation starts carry caller-selected invocation identity", async () => {
  let requestBody: unknown;
  const transport: OperationTransport = {
    requestJson: (_subject, body) => {
      requestBody = body;
      return AsyncResult.ok({
        kind: "accepted",
        ref: {
          id: "01J00000000000000000000000",
          service: "orders",
          operation: "Run",
        },
        snapshot: SNAPSHOT,
      });
    },
    watchJson: (_subject, _body, decodeEvent) =>
      AsyncResult.ok(
        completedWatch(
          decodeEvent({
            kind: "snapshot",
            snapshot: COMPLETED,
          })!,
        ),
      ),
    putTransfer: () => {
      throw new Error("not used");
    },
  };
  const invocationId = "01J00000000000000000000000";
  const started = await new OperationInvoker(transport, {
    subject: "operation.v1.test",
    input: codecs.string,
  }).input("payload").start(undefined, { invocationId });
  const result = started.take();
  if (isErr(result)) throw result.error;

  assertEquals(requestBody, { invocationId, input: "payload" });
});

Deno.test("watch returns a live subscription of operation events", async () => {
  const transport: OperationTransport = {
    requestJson: () => {
      throw new Error("not used");
    },
    watchJson: (subject, body, decodeEvent) => {
      assertEquals(subject, "operation.v1.test.control");
      assertEquals(body, {
        action: "watch",
        operationId: "01J00000000000000000000000",
      });
      return AsyncResult.ok(
        completedWatch(
          decodeEvent({
            kind: "snapshot",
            snapshot: COMPLETED,
          })!,
        ),
      );
    },
    putTransfer: () => {
      throw new Error("not used");
    },
  };
  const operation = new OperationInvoker(transport, {
    subject: "operation.v1.test",
    input: codecs.string,
  }).resume({
    id: "01J00000000000000000000000",
    service: "orders",
    operation: "Run",
  });
  const watched = await operation.live();
  const subscription = watched.take();
  if (isErr(subscription)) throw subscription.error;
  const events = [];
  for await (const event of subscription) events.push(event);
  assertEquals(events, [{ type: "completed", snapshot: COMPLETED }]);
});

Deno.test("wait owns and disposes the live observer", async () => {
  let disposed = false;
  const transport: OperationTransport = {
    requestJson: (_subject, body) => {
      if (
        body && typeof body === "object" && !Array.isArray(body) &&
        (body as { action?: string }).action === "get"
      ) {
        return AsyncResult.ok({
          kind: "snapshot",
          snapshot: { ...SNAPSHOT, state: "running", revision: 1 },
        });
      }
      throw new Error("unexpected request");
    },
    watchJson: (_subject, _body, decodeEvent) =>
      AsyncResult.ok(
        completedWatch(
          decodeEvent({
            kind: "snapshot",
            snapshot: COMPLETED,
          })!,
          () => {
            disposed = true;
          },
        ),
      ),
    putTransfer: () => {
      throw new Error("not used");
    },
  };
  const operation = new OperationInvoker(transport, {
    subject: "operation.v1.test",
    input: codecs.string,
  }).resume({
    id: "01J00000000000000000000000",
    service: "orders",
    operation: "Run",
  });
  const waited = await operation.wait();
  const terminal = waited.take();
  if (isErr(terminal)) throw terminal.error;
  assertEquals(terminal, COMPLETED);
  assertEquals(disposed, true);
});
