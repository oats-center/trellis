import { assertEquals, assertInstanceOf } from "@std/assert";

import { NatsTestContainer } from "../trellis-testkit/src/nats_container.ts";
import { StoreError } from "./errors/StoreError.ts";
import { TypedStore } from "./store.ts";
import { fixedTransportProvider } from "./transport/generations.ts";

Deno.test("Store round trips objects, waits for keys, and enforces its wait budget", async () => {
  const workdir = await Deno.makeTempDir({ prefix: "trellis-store-" });
  let nats: NatsTestContainer | undefined;
  try {
    nats = await NatsTestContainer.start(workdir);
    const store = (await TypedStore.open(
      fixedTransportProvider(nats.nc),
      "store_test",
      {},
    )).orThrow();

    const payload = new TextEncoder().encode("stored generation bytes");
    await store.put("k", payload).orThrow();

    const entry = (await store.get("k")).orThrow();
    assertEquals(
      new TextDecoder().decode(await entry.bytes().orThrow()),
      "stored generation bytes",
    );

    const waited = (await store.waitFor("k", { timeoutMs: 2_000 })).orThrow();
    assertEquals(waited.key, "k");

    // The wait timeout bounds the whole attempt, not just key polling.
    const missing = await store.waitFor("missing", {
      timeoutMs: 250,
      pollIntervalMs: 25,
    });
    assertInstanceOf(missing.error, StoreError);
    assertEquals(missing.error.getContext().reason, "timeout");

    await store.delete("k").orThrow();
    const deleted = await store.get("k");
    assertInstanceOf(deleted.error, StoreError);
    assertEquals(deleted.error.getContext().reason, "not_found");
  } finally {
    await nats?.stop();
    await Deno.remove(workdir, { recursive: true });
  }
});
