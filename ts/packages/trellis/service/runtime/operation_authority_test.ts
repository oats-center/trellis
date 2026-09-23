/**
 * O09 component proof at the production operation-observe authorization
 * boundary.
 *
 * Observation and control authority is scoped to the exact creating principal
 * and participant. A foreign principal, or the same principal on a different
 * participant, is denied without the operation being disclosed.
 */

import { assertEquals } from "@std/assert";

import {
  operationObserveAuthorized,
  OperationObserverArbiter,
  operationOwnerFenceHolds,
} from "./core.ts";

const OPERATION = {
  creatorPrincipalId: "principal-a",
  creatorParticipantId: "runtime-trellis.OperationCaller",
};

Deno.test("O09 the creating principal and participant are authorized", () => {
  assertEquals(
    operationObserveAuthorized(OPERATION, {
      principalId: "principal-a",
      participantId: "runtime-trellis.OperationCaller",
    }),
    true,
  );
});

Deno.test("O09 a foreign principal is denied", () => {
  assertEquals(
    operationObserveAuthorized(OPERATION, {
      principalId: "principal-b",
      participantId: "runtime-trellis.OperationCaller",
    }),
    false,
  );
});

Deno.test("O09 the same principal on a different participant is denied", () => {
  assertEquals(
    operationObserveAuthorized(OPERATION, {
      principalId: "principal-a",
      participantId: "runtime-trellis.OperationProvider",
    }),
    false,
  );
});

Deno.test("O09 internal callers without a verified caller are authorized", () => {
  assertEquals(operationObserveAuthorized(OPERATION, undefined), true);
});

const OWNER = { ownerInstanceId: "instance-a", ownerEpoch: 7 };

Deno.test("O10 the current executor fence still owns the operation", () => {
  assertEquals(
    operationOwnerFenceHolds(OWNER, {
      ownerInstanceId: "instance-a",
      ownerEpoch: 7,
    }),
    true,
  );
});

Deno.test("O10 a stale epoch is rejected before outer delivery", () => {
  assertEquals(
    operationOwnerFenceHolds(OWNER, {
      ownerInstanceId: "instance-a",
      ownerEpoch: 6,
    }),
    false,
  );
});

Deno.test("O10 a different executor instance is rejected", () => {
  assertEquals(
    operationOwnerFenceHolds(OWNER, {
      ownerInstanceId: "instance-b",
      ownerEpoch: 7,
    }),
    false,
  );
});

Deno.test("O08 the observer reconciles authoritative durable state across executors", async () => {
  const delivered: unknown[] = [];
  const arbiter = new OperationObserverArbiter((value) => {
    delivered.push(value);
    return Promise.resolve();
  });
  // Executor A emits a transient update.
  assertEquals(arbiter.update("executor-a-update"), true);
  // The authoritative snapshot comes from durable state, not the executor.
  await arbiter.snapshot("durable-a");
  // Executor ownership changes; the same observer keeps reconciling.
  assertEquals(arbiter.update("executor-b-update"), true);
  await arbiter.snapshot("durable-terminal");
  assertEquals(delivered, [
    "executor-a-update",
    "durable-a",
    "executor-b-update",
    "durable-terminal",
  ]);
});
