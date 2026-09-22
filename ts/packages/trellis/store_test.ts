import { assertEquals, assertRejects } from "@std/assert";

import {
  ensureExistingStoreOptions,
  type StoreInfo,
  type StoreListOptions,
  type StorePutOptions,
  type StoreStatus,
  type StoreWaitOptions,
} from "./store.ts";
import type { StoreListPage } from "./store.ts";

Deno.test("Store public types compile", () => {
  const _putOptions: StorePutOptions = {
    contentType: "application/pdf",
    metadata: { source: "portal" },
  };

  const _info: StoreInfo = {
    key: "incoming/test.pdf",
    size: 123,
    updatedAt: new Date().toISOString(),
    digest: "sha256:test",
    contentType: "application/pdf",
    metadata: { source: "portal" },
  };

  const _status: StoreStatus = {
    size: 123,
    sealed: false,
    ttlMs: 60_000,
    maxObjectBytes: 1024,
    maxTotalBytes: 4096,
  };

  const _waitOptions: StoreWaitOptions = {
    timeoutMs: 5_000,
    pollIntervalMs: 100,
    signal: new AbortController().signal,
  };

  const _listOptions: StoreListOptions = {
    prefix: "incoming/",
    cursor: "opaque",
    limit: 10,
  };
  const _listPage: StoreListPage = {
    entries: [_info],
    nextCursor: "opaque",
  };

  assertEquals(true, true);
});

Deno.test("existing Store options require exact TTL and ignore advisory sizes", async () => {
  const check = (
    ttlMs: number,
    options: {
      ttlMs?: number;
      maxObjectBytes?: number;
      maxTotalBytes?: number;
    },
  ) =>
    ensureExistingStoreOptions(
      {
        status: () =>
          Promise.resolve({
            ttl: ttlMs * 1_000_000,
          }),
      },
      "files",
      options,
    );

  await check(1_000, {
    ttlMs: 1_000,
    maxObjectBytes: 1,
    maxTotalBytes: 4_096,
  });
  await check(0, { maxObjectBytes: 1 });
  await assertRejects(() => check(999, { ttlMs: 1_000, maxTotalBytes: 4_096 }));
  await assertRejects(() =>
    check(1_001, { ttlMs: 1_000, maxTotalBytes: 4_096 })
  );
  await check(1_000, { ttlMs: 1_000, maxTotalBytes: 4_095 });
  await check(1_000, { ttlMs: 1_000, maxTotalBytes: 4_096 });
});
