import assert from "node:assert/strict";
import { test } from "node:test";
import { eventSubjectWasm } from "../auth/protocol_wasm.ts";
import { eventSubject } from "./api.ts";

test("WASM and TypeScript encode qualified event identities identically", async () => {
  for (const apiId of ["acme-orders.runtime@v1", "other.runtime@v2"]) {
    assert.deepEqual(
      await eventSubjectWasm(apiId, "Orders.Changed"),
      eventSubject(apiId, "Orders.Changed"),
    );
  }
});
