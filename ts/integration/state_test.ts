import { assert, assertEquals, assertInstanceOf } from "@std/assert";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { StateConflictError } from "../packages/trellis/session.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("generated State survives a Rust control-plane restart", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.registerService({
      name: "state-provider",
      contract: participants.Provider.participant,
    });
    const client = await runtime.connectClient({
      name: "state-restart",
      contract: participants.Caller.participant,
    });

    await client.state.saved.set({ value: "durable" }).orThrow();
    await runtime.restartControlPlane();
    const stored = await client.state.saved.get().orThrow();

    assert(stored);
    assertEquals(stored.value.value, "durable");
  });
});

Deno.test("generated State preserves create and replace semantics", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.registerService({
      name: "state-provider",
      contract: participants.Provider.participant,
    });
    const client = await runtime.connectClient({
      name: "state-write-modes",
      contract: participants.Caller.participant,
    });

    const created = await client.state.saved.create({ value: "created" })
      .orThrow();
    const duplicate = await client.state.saved.create({ value: "duplicate" });
    assert(duplicate.isErr());
    assertInstanceOf(duplicate.error, StateConflictError);
    assertEquals(duplicate.error.current?.value, { value: "created" });

    const replaced = await client.state.saved.replace(
      created.revision,
      { value: "replaced" },
    ).orThrow();
    assertEquals(replaced.value, { value: "replaced" });

    const stale = await client.state.saved.replace(
      created.revision,
      { value: "stale" },
    );
    assert(stale.isErr());
    assertInstanceOf(stale.error, StateConflictError);
    assertEquals(stale.error.current?.value, { value: "replaced" });

    const staleDelete = await client.state.saved.delete(created.revision);
    assert(staleDelete.isErr());
    assertInstanceOf(staleDelete.error, StateConflictError);
    assertEquals(staleDelete.error.current?.value, { value: "replaced" });

    await client.state.saved.delete(replaced.revision).orThrow();
    const concurrent = await Promise.all([
      client.state.saved.create({ value: "first" }),
      client.state.saved.create({ value: "second" }),
    ]);
    const winner = concurrent.find((result) => result.isOk());
    const loser = concurrent.find((result) => result.isErr());
    assert(winner?.isOk());
    assert(loser?.isErr());
    assertInstanceOf(loser.error, StateConflictError);
    assertEquals(
      loser.error.current?.value,
      winner.unwrapOrElse(() => {
        throw new Error("concurrent create winner missing");
      }).value,
    );
  });
});
