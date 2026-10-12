import { assertEquals } from "@std/assert";
import { eventSubjectWasm } from "../auth/protocol_wasm.ts";
import { eventSubject } from "./api.ts";

Deno.test(
  "WASM and TypeScript encode qualified event identities identically",
  async () => {
    for (const apiId of ["acme-orders.runtime@v1", "other.runtime@v2"]) {
      assertEquals(
        await eventSubjectWasm(apiId, "Orders.Changed"),
        eventSubject(apiId, "Orders.Changed"),
      );
    }
  },
);
