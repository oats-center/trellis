/**
 * Ownership predicates and observer reconciliation used by the Operation runtime.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  operationObserveAuthorized,
  OperationObserverArbiter,
  operationOwnerFenceHolds,
} from "./core.ts";

const OPERATION = {
  creatorPrincipalId: "principal-a",
  creatorBinding: {
    kind: "browser" as const,
    clientId: "console",
    origin: "https://console.example",
  },
};

test("O09 the creating principal and browser binding are authorized", () => {
  assert.deepEqual(
    operationObserveAuthorized(OPERATION, {
      principalId: "principal-a",
      binding: {
        kind: "browser",
        clientId: "console",
        origin: "https://console.example",
      },
    }),
    true,
  );
});

test("O09 a foreign principal is denied", () => {
  assert.deepEqual(
    operationObserveAuthorized(OPERATION, {
      principalId: "principal-b",
      binding: {
        kind: "browser",
        clientId: "console",
        origin: "https://console.example",
      },
    }),
    false,
  );
});

test("O09 the same principal on another client or origin is denied", () => {
  assert.deepEqual(
    operationObserveAuthorized(OPERATION, {
      principalId: "principal-a",
      binding: {
        kind: "browser",
        clientId: "portal",
        origin: "https://console.example",
      },
    }),
    false,
  );
  assert.deepEqual(
    operationObserveAuthorized(OPERATION, {
      principalId: "principal-a",
      binding: {
        kind: "browser",
        clientId: "console",
        origin: "https://other.example",
      },
    }),
    false,
  );
  assert.deepEqual(
    operationObserveAuthorized(OPERATION, {
      principalId: "principal-a",
      binding: { kind: "native", clientId: "console", durableDpopJkt: "key" },
    }),
    false,
  );
});

test("O09 internal callers without a verified caller are authorized", () => {
  assert.deepEqual(operationObserveAuthorized(OPERATION, undefined), true);
});

const OWNER = { ownerInstanceId: "instance-a", ownerEpoch: 7 };

test("O10 the current executor fence still owns the operation", () => {
  assert.deepEqual(
    operationOwnerFenceHolds(OWNER, {
      ownerInstanceId: "instance-a",
      ownerEpoch: 7,
    }),
    true,
  );
});

test("O10 a stale epoch is rejected before outer delivery", () => {
  assert.deepEqual(
    operationOwnerFenceHolds(OWNER, {
      ownerInstanceId: "instance-a",
      ownerEpoch: 6,
    }),
    false,
  );
});

test("O10 a different executor instance is rejected", () => {
  assert.deepEqual(
    operationOwnerFenceHolds(OWNER, {
      ownerInstanceId: "instance-b",
      ownerEpoch: 7,
    }),
    false,
  );
});

test("O08 the observer reconciles authoritative durable state across executors", async () => {
  const delivered: unknown[] = [];
  const arbiter = new OperationObserverArbiter((value) => {
    delivered.push(value);
    return Promise.resolve();
  });
  // Executor A emits a transient update.
  assert.deepEqual(arbiter.update("executor-a-update"), true);
  // The authoritative snapshot comes from durable state, not the executor.
  await arbiter.snapshot("durable-a");
  // Executor ownership changes; the same observer keeps reconciling.
  assert.deepEqual(arbiter.update("executor-b-update"), true);
  await arbiter.snapshot("durable-terminal");
  assert.deepEqual(delivered, [
    "executor-a-update",
    "durable-a",
    "executor-b-update",
    "durable-terminal",
  ]);
});
