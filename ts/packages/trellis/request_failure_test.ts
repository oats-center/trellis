/**
 * Classification of actual NATS SDK request errors.
 */

import assert from "node:assert/strict";
import { test } from "node:test";
import {
  NoRespondersError,
  PermissionViolationError,
} from "@nats-io/nats-core";

import { classifyRequestTransportFailure } from "./session.ts";

test("T16 an RPC permission denial is a request error", () => {
  const error = classifyRequestTransportFailure({
    subject: "rpc.v1.example",
    cause: new PermissionViolationError(
      "Publish denied",
      "publish",
      "rpc.v1.example",
    ),
  });
  assert.deepEqual(error.code, "trellis.request.denied");
});

test("T16 a no-responders failure is unavailable, not denied", () => {
  const error = classifyRequestTransportFailure({
    subject: "rpc.v1.example",
    cause: new NoRespondersError("rpc.v1.example"),
  });
  assert.deepEqual(error.code, "trellis.request.unavailable");
});
