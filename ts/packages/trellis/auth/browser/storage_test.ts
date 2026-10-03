import { assertEquals, assertNotEquals } from "@std/assert";

import { browserInstallationScope, BrowserSessionStore } from "./storage.ts";

Deno.test("browser installation scope uses only origin and participant", () => {
  assertEquals(
    browserInstallationScope("https://trellis.test/path", "app@v1"),
    browserInstallationScope("https://trellis.test/other", "app@v1"),
  );
});

Deno.test("temporary browser installation fences intent, bind, expiry, and clear", async () => {
  const persistence = "temporary";
  const scope = `${persistence}:${crypto.randomUUID()}`;
  const first = new BrowserSessionStore(scope, persistence);
  const second = new BrowserSessionStore(scope, persistence);
  const initial = await first.getOrCreateCredential(100);
  const winner = await second.getOrCreateCredential(100);

  assertEquals(winner.seed, initial.seed);
  assertEquals(winner.sessionKey, initial.sessionKey);
  assertEquals(
    await first.rememberIntent({ ...initial, generation: 1 }, "flow-stale"),
    false,
  );
  assertEquals(await first.rememberIntent(initial, "flow-current"), true);
  assertEquals(
    await second.completeBind({ ...winner, pendingIntentId: "flow-stale" }, {
      loginSessionId: "login-stale",
      expiresAt: 500,
    }),
    false,
  );
  assertEquals(
    await second.completeBind(
      { ...winner, pendingIntentId: "flow-current" },
      {
        loginSessionId: "login-current",
        expiresAt: 500,
      },
    ),
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
  assertEquals(
    await first.rememberIntent(replacement, "flow-expiring"),
    true,
  );
  assertEquals(
    await first.completeBind({
      ...replacement,
      pendingIntentId: "flow-expiring",
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
});
