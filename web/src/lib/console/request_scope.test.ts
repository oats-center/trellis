import { deepEqual, equal } from "node:assert/strict";

import { RequestScope } from "./request_scope.ts";

Deno.test("U01 later request supersedes an earlier response", () => {
  const scope = new RequestScope("list");
  const first = scope.begin();
  const second = scope.begin();
  equal(scope.isCurrent(first), false);
  equal(scope.isCurrent(second), true);
  equal(scope.settle(first), false);
  equal(scope.settle(second), true);
});

Deno.test("U01 disposal prevents every later commit", () => {
  const scope = new RequestScope("list");
  const token = scope.begin();
  scope.dispose();
  equal(scope.isCurrent(token), false);
  equal(scope.disposed, true);
  equal(scope.settle(token), false);
  equal(scope.isCurrent(scope.begin()), false);
});

Deno.test("U02 a semantic key change clears old data and invalidates reads", () => {
  const scope = new RequestScope("service-a");
  const token = scope.begin();
  equal(scope.setKey("service-a"), false);
  equal(scope.isCurrent(token), true);
  equal(scope.setKey("service-b"), true);
  equal(scope.isCurrent(token), false);
  equal(scope.key, "service-b");
});

Deno.test("U02 invalidate keeps the scope alive for a same-target refresh", () => {
  const scope = new RequestScope("service-a");
  const first = scope.begin();
  scope.invalidate();
  equal(scope.isCurrent(first), false);
  const refresh = scope.begin();
  equal(scope.isCurrent(refresh), true);
  equal(scope.setKey("service-a"), false);
});

Deno.test("request scope tracks one in-flight token", () => {
  const scope = new RequestScope("jobs");
  equal(scope.pending, false);
  const token = scope.begin();
  equal(scope.pending, true);
  scope.settle(token);
  equal(scope.pending, false);
  deepEqual(token, { generation: token.generation });
});

Deno.test("U02 a semantic target change clears the previous identity", () => {
  const scope = new RequestScope("deployment-a");
  const token = scope.begin();
  equal(scope.setKey("deployment-b"), true);
  equal(scope.isCurrent(token), false);
  equal(scope.key, "deployment-b");
});

Deno.test("U32 independent domains do not invalidate each other", () => {
  const primary = new RequestScope("events");
  const optional = new RequestScope("consumers");
  const primaryToken = primary.begin();
  optional.setKey("consumers-filtered");
  equal(
    primary.isCurrent(primaryToken),
    true,
    "an optional panel change must not invalidate the primary read",
  );
});

Deno.test("U33 disposal prevents every later commit", () => {
  const scope = new RequestScope("target");
  const token = scope.begin();
  scope.dispose();
  equal(scope.isCurrent(token), false);
  equal(scope.settle(token), false);
});
