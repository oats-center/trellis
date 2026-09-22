import { assertEquals } from "@std/assert";
import { AsyncResult, isErr, ok } from "@qlever-llc/result";

import { codecs } from "./generated.ts";
import { OperationInvoker, type OperationTransport } from "./operations.ts";

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
        snapshot: {
          id: "01J00000000000000000000000",
          service: "orders",
          operation: "Run",
          revision: 1,
          state: "pending",
          createdAt: "2026-01-01T00:00:00.000Z",
          updatedAt: "2026-01-01T00:00:00.000Z",
        },
      });
    },
    watchJson: () =>
      AsyncResult.ok((async function* () {
        yield ok({
          kind: "snapshot",
          snapshot: {
            id: "01J00000000000000000000000",
            service: "orders",
            operation: "Run",
            revision: 2,
            state: "completed",
            createdAt: "2026-01-01T00:00:00.000Z",
            updatedAt: "2026-01-01T00:00:01.000Z",
          },
        });
      })()),
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
