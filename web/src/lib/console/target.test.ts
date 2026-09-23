import { equal } from "node:assert/strict";

import { resolveRequestedTarget } from "./target.ts";
import { err, ok } from "@oatscenter/result";
import { apis } from "trellis-web-generated";

function authFailure(code: string) {
  return new apis.auth.AuthError({
    id: "err_test",
    type: "trellis.auth@v1::AuthError",
    message: code,
    code,
    field: null,
    retryable: false,
  });
}

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};

type Record_ = { id: string; state: string };

Deno.test("U34 an exact Get resolves the requested record only", async () => {
  const resolution = await resolveRequestedTarget<Record_>({
    requestedId: "dep_b",
    get: (id) => Promise.resolve(ok({ id, state: "active" })),
    idOf: (item) => item.id,
  });
  equal(resolution.kind, "ready");
  if (resolution.kind === "ready") equal(resolution.item.id, "dep_b");
});

Deno.test("U34 a Get for a different record is an error, not a target", async () => {
  const resolution = await resolveRequestedTarget<Record_>({
    requestedId: "dep_b",
    get: () => Promise.resolve(ok({ id: "dep_a", state: "active" })),
    idOf: (item) => item.id,
  });
  equal(resolution.kind, "error");
});

Deno.test("U34 a missing target is not found, and is never substituted", async () => {
  const resolution = await resolveRequestedTarget<Record_>({
    requestedId: "dep_missing",
    get: () => Promise.resolve(err(authFailure("not_found"))),
    idOf: (item) => item.id,
  });
  equal(resolution.kind, "not-found");
});

Deno.test("U34 a forbidden target is distinct from a missing one", async () => {
  const resolution = await resolveRequestedTarget<Record_>({
    requestedId: "dep_forbidden",
    get: () => Promise.resolve(err(authFailure("not_authorized"))),
    idOf: (item) => item.id,
  });
  equal(resolution.kind, "forbidden");
});

Deno.test("N02 a target beyond the first page resolves by complete traversal", async () => {
  const pages: Array<{ items: Record_[]; cursor?: string }> = [
    {
      items: Array.from(
        { length: 100 },
        (_, index) => ({ id: `dep_${index}`, state: "active" }),
      ),
      cursor: "p2",
    },
    { items: [{ id: "dep_target", state: "active" }] },
  ];
  let call = 0;
  const resolution = await resolveRequestedTarget<Record_>({
    requestedId: "dep_target",
    listPage: () => Promise.resolve(ok(pages[call++])),
    idOf: (item) => item.id,
  });
  equal(resolution.kind, "ready");
  equal(call, 2);
});

Deno.test("N02 a failed later page is incomplete, not absence", async () => {
  let call = 0;
  const resolution = await resolveRequestedTarget<Record_>({
    requestedId: "dep_target",
    listPage: () => {
      call += 1;
      if (call === 1) {
        return Promise.resolve(
          ok({ items: [{ id: "dep_0", state: "active" }], cursor: "p2" }),
        );
      }
      return Promise.resolve(err(authFailure("internal_error")));
    },
    idOf: (item) => item.id,
  });
  equal(
    resolution.kind,
    "incomplete",
    "a failed traversal must not declare the target absent",
  );
});

Deno.test("U04 a repeated cursor is an incomplete lookup, not a loop", async () => {
  const resolution = await resolveRequestedTarget<Record_>({
    requestedId: "dep_target",
    listPage: () =>
      Promise.resolve(
        ok({ items: [{ id: "dep_0", state: "active" }], cursor: "same" }),
      ),
    idOf: (item) => item.id,
  });
  equal(resolution.kind, "incomplete");
});

Deno.test("U05 an empty requested target is not-requested, not a first row", async () => {
  let called = 0;
  const resolution = await resolveRequestedTarget<Record_>({
    requestedId: "",
    listPage: () => {
      called += 1;
      return Promise.resolve(ok({ items: [{ id: "dep_a", state: "active" }] }));
    },
    idOf: (item) => item.id,
  });
  equal(resolution.kind, "not-requested");
  equal(called, 0, "an empty target must not read or select anything");
});
