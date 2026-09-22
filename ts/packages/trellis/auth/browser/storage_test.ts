import "fake-indexeddb/auto";
import { assert, assertEquals, assertNotEquals } from "@std/assert";

import { browserInstallationScope, BrowserSessionStore } from "./storage.ts";

Deno.test("browser installation scope uses only origin and participant", () => {
  assertEquals(
    browserInstallationScope("https://trellis.test/path", "app@v1"),
    browserInstallationScope("https://trellis.test/other", "app@v1"),
  );
});

Deno.test("browser installation fences flow, bind, expiry, and clear", async () => {
  for (const persistence of ["remembered", "temporary"] as const) {
    const scope = `${persistence}:${crypto.randomUUID()}`;
    const first = new BrowserSessionStore(scope, persistence);
    const second = new BrowserSessionStore(scope, persistence);
    const initial = await first.getOrCreateCredential(100);
    const winner = await second.getOrCreateCredential(100);

    assertEquals(initial.generation, 0);
    assertEquals(initial.seed.length, 32);
    assertEquals(winner.seed, initial.seed);
    assertEquals(winner.sessionKey, initial.sessionKey);
    assertEquals(
      await first.rememberFlow({ ...initial, generation: 1 }, "flow-stale"),
      false,
    );
    assertEquals(await first.rememberFlow(initial, "flow-current"), true);
    assertEquals(
      await second.completeBind({ ...winner, pendingFlowId: "flow-stale" }, {
        loginSessionId: "login-stale",
        expiresAt: 500,
      }),
      false,
    );
    assertEquals(
      await second.completeBind({ ...winner, pendingFlowId: "flow-current" }, {
        loginSessionId: "login-current",
        expiresAt: 500,
      }),
      true,
    );
    assertEquals((await first.readLogin(499))?.loginSessionId, "login-current");
    assertEquals(
      await first.clearLogin({ ...initial, loginSessionId: "login-stale" }),
      false,
    );
    assertEquals(
      await first.clearLogin({ ...initial, loginSessionId: "login-current" }),
      true,
    );
    assertEquals(await first.readLogin(499), undefined);

    const replacement = await first.getOrCreateCredential(499);
    assertEquals(replacement.generation, 1);
    assertNotEquals(replacement.sessionKey, initial.sessionKey);
    assertEquals(await first.rememberFlow(replacement, "flow-expiring"), true);
    assertEquals(
      await first.completeBind({
        ...replacement,
        pendingFlowId: "flow-expiring",
      }, {
        loginSessionId: "login-expiring",
        expiresAt: 600,
      }),
      true,
    );
    assertEquals(await second.readLogin(600), undefined);
    const afterExpiry = await second.getOrCreateCredential(600);
    assertEquals(afterExpiry.generation, 2);
    assertNotEquals(afterExpiry.sessionKey, replacement.sessionKey);
  }
});

Deno.test("remembered browser records contain no authorization context", async () => {
  const scope = `remembered:${crypto.randomUUID()}`;
  const store = new BrowserSessionStore(scope);
  const current = await store.getOrCreateCredential();
  await store.rememberFlow(current, "flow-current");
  await store.completeBind({ ...current, pendingFlowId: "flow-current" }, {
    loginSessionId: "login-current",
    expiresAt: null,
  });

  const db = await new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open("trellis-auth", 3);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);
  });
  const records = await new Promise<Record<string, unknown>[]>(
    (resolve, reject) => {
      const request = db.transaction("installations").objectStore(
        "installations",
      ).getAll();
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve(request.result);
    },
  );
  db.close();
  const record = records.find((value) => value.id === scope);
  assert(record);
  assertEquals(Object.keys(record).sort(), [
    "expiresAt",
    "generation",
    "id",
    "loginSessionId",
    "seed",
    "sessionKey",
  ]);
});
