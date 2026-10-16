import assert from "node:assert/strict";
import { test } from "node:test";
import { AsyncResult, BaseError, err, ok } from "@oatscenter/result";
import { cursorItems, cursorPages, PaginationError } from "./pagination.ts";
import type { CallerRuntime } from "./caller.ts";
import { normalizeCursorQuery } from "./participant_runtime/schemas.ts";
import { request as consoleRequest } from "./internal_sdk/generated/apps/console/mod.js";

type Page = { items: number[]; page: { nextCursor?: string } };

class TestError extends BaseError {
  override readonly name = "TestError";
  override toSerializable() {
    return { ...this.baseSerializable(), type: this.name };
  }
}

test("cursor limits are positive integers without a library maximum", () => {
  assert.deepEqual(normalizeCursorQuery({ limit: 1 }), { limit: 1 });
  assert.deepEqual(normalizeCursorQuery({ limit: 10_000 }), { limit: 10_000 });
  for (const limit of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    let threw = false;
    try {
      normalizeCursorQuery({ limit });
    } catch (error) {
      threw = error instanceof RangeError;
    }
    assert.deepEqual(threw, true);
  }
  let capped = false;
  try {
    normalizeCursorQuery({ limit: 201 }, { maxLimit: 200 });
  } catch (error) {
    capped = error instanceof RangeError;
  }
  assert.deepEqual(capped, true);
});

test("cursor pagination is lazy, serial, and preserves input and options", async () => {
  const calls: Array<{ input: unknown; timeout?: number }> = [];
  const responses: Page[] = [
    { items: [1], page: { nextCursor: "next" } },
    { items: [2], page: {} },
  ];
  const pages = cursorPages(
    (input, options) => {
      calls.push({ input, timeout: options?.timeout });
      return AsyncResult.from(Promise.resolve(ok(responses.shift()!)));
    },
    { filter: "open", page: { limit: 17 } },
    { timeout: 123 },
  );

  assert.deepEqual(calls, []);
  const iterator = pages[Symbol.asyncIterator]();
  assert.deepEqual((await iterator.next()).value?.orThrow().items, [1]);
  assert.deepEqual(calls, [
    {
      input: { filter: "open", page: { limit: 17 } },
      timeout: 123,
    },
  ]);
  assert.deepEqual((await iterator.next()).value?.orThrow().items, [2]);
  assert.deepEqual(calls[1], {
    input: { filter: "open", page: { limit: 17, cursor: "next" } },
    timeout: 123,
  });
  assert.deepEqual(await iterator.next(), { value: undefined, done: true });
});

/** Compile-only consumer coverage; the tests below exercise pagination behavior. */
export function checkGeneratedCursorTypes(
  caller: CallerRuntime<typeof consoleRequest>,
): void {
  caller.usersList({ page: { limit: 10 } });
  caller.usersList.pages({});
  caller.usersList.items({}, { timeout: 123 });
  // @ts-expect-error Non-paginated RPCs keep only their one-page method.
  caller.sessionsMe.pages({});
}

test("concurrent next calls still request one page at a time", async () => {
  const responses: Array<(page: Page) => void> = [];
  let active = 0;
  let maxActive = 0;
  const iterator = cursorPages<Page>(() => {
    active++;
    maxActive = Math.max(maxActive, active);
    return AsyncResult.from(
      new Promise((resolve) => {
        responses.push((page) => {
          active--;
          resolve(ok(page));
        });
      }),
    );
  })[Symbol.asyncIterator]();

  const first = iterator.next();
  const second = iterator.next();
  await Promise.resolve();
  assert.deepEqual(responses.length, 1);
  responses.shift()!({ items: [1], page: { nextCursor: "next" } });
  await first;
  await Promise.resolve();
  assert.deepEqual(responses.length, 1);
  responses.shift()!({ items: [2], page: {} });
  await second;
  assert.deepEqual(maxActive, 1);
});

test("cursor items continue through empty pages and stop after one error", async () => {
  let calls = 0;
  const responses = [
    ok<Page>({ items: [], page: { nextCursor: "empty" } }),
    ok<Page>({ items: [1, 2], page: { nextCursor: "error" } }),
    err<TestError, Page>(new TestError("failed")),
  ];
  const seen = [];
  for await (const result of cursorItems<number, Page>(() => {
    calls++;
    return AsyncResult.from(Promise.resolve(responses.shift()!));
  })) {
    seen.push(result);
  }

  assert.deepEqual(
    seen.slice(0, 2).map((result) => result.orThrow()),
    [1, 2],
  );
  assert(seen[2].error instanceof TestError);
  assert.deepEqual(calls, 3);
});

test("cursor pagination reports a repeated cursor once", async () => {
  let calls = 0;
  const iterator = cursorPages<Page>(() => {
    calls++;
    return AsyncResult.from(
      Promise.resolve(
        ok({
          items: [],
          page: { nextCursor: "same" },
        }),
      ),
    );
  })[Symbol.asyncIterator]();

  assert.deepEqual((await iterator.next()).value?.isOk(), true);
  const repeated = await iterator.next();
  assert(repeated.value?.error instanceof PaginationError);
  assert.deepEqual(await iterator.next(), { value: undefined, done: true });
  assert.deepEqual(calls, 2);
});

test("cursor pages reject an echoed initial cursor before yielding a page", async () => {
  const calls: Array<{ input: unknown; timeout?: number }> = [];
  const seen = [];
  for await (const result of cursorPages<Page>(
    (input, options) => {
      calls.push({ input, timeout: options?.timeout });
      return AsyncResult.from(
        Promise.resolve(
          ok({
            items: [1],
            page: { nextCursor: "A" },
          }),
        ),
      );
    },
    { page: { cursor: "A", limit: 17 } },
    { timeout: 123 },
  )) {
    seen.push(result);
  }

  assert.deepEqual(seen.length, 1);
  assert(seen[0].error instanceof PaginationError);
  assert.deepEqual(calls, [
    {
      input: { page: { cursor: "A", limit: 17 } },
      timeout: 123,
    },
  ]);
});

test("cursor items reject an echoed initial cursor before yielding an item", async () => {
  const calls: Array<{ input: unknown; timeout?: number }> = [];
  const seen = [];
  for await (const result of cursorItems<number, Page>(
    (input, options) => {
      calls.push({ input, timeout: options?.timeout });
      return AsyncResult.from(
        Promise.resolve(
          ok({
            items: [1],
            page: { nextCursor: "A" },
          }),
        ),
      );
    },
    { page: { cursor: "A", limit: 17 } },
    { timeout: 123 },
  )) {
    seen.push(result);
  }

  assert.deepEqual(seen.length, 1);
  assert(seen[0].error instanceof PaginationError);
  assert.deepEqual(calls, [
    {
      input: { page: { cursor: "A", limit: 17 } },
      timeout: 123,
    },
  ]);
});

test("return aborts an outstanding pagination request", async () => {
  let aborted = false;
  const iterator = cursorPages<Page>((_input, options) =>
    AsyncResult.from(
      new Promise((resolve) => {
        options?.signal?.addEventListener("abort", () => {
          aborted = true;
          resolve(err(new TestError("aborted")));
        });
      }),
    ),
  )[Symbol.asyncIterator]();

  const pending = iterator.next();
  await Promise.resolve();
  await iterator.return?.();
  await pending;
  assert.deepEqual(aborted, true);
  assert.deepEqual(await iterator.next(), { value: undefined, done: true });
});
