import { assert, assertEquals } from "@std/assert";

import vectors from "../../../../integration/fixtures/protocol/transport-authorization/vectors.json" with {
  type: "json",
};
import {
  classifyTransportAuthorizationWasm,
  transportAuthorizationDigestWasm,
  type TransportAuthorizationV1,
  type TransportPolicyClass,
} from "./protocol_wasm.ts";

type Case = {
  name: string;
  admitted: TransportAuthorizationV1;
  allowed: TransportAuthorizationV1;
  now: number;
  class: TransportPolicyClass;
  reverseClass?: TransportPolicyClass;
};

type DigestCase = {
  name: string;
  policy: TransportAuthorizationV1;
  digest: string;
};

Deno.test("shared transport vectors classify identically in TypeScript over WASM", async () => {
  const cases = (vectors as { cases: Case[] }).cases;
  assert(cases.length > 0, "shared vectors must cover behavior");
  for (const testCase of cases) {
    assertEquals(
      await classifyTransportAuthorizationWasm(
        testCase.admitted,
        testCase.allowed,
        testCase.now,
      ),
      testCase.class,
      testCase.name,
    );
    if (testCase.reverseClass) {
      assertEquals(
        await classifyTransportAuthorizationWasm(
          testCase.allowed,
          testCase.admitted,
          testCase.now,
        ),
        testCase.reverseClass,
        `${testCase.name} (reverse)`,
      );
    }
  }
});

Deno.test("shared transport digests match the cross-language fixture", async () => {
  const digests = (vectors as { digests: DigestCase[] }).digests;
  assert(digests.length > 0);
  for (const entry of digests) {
    assertEquals(
      await transportAuthorizationDigestWasm(entry.policy),
      entry.digest,
      entry.name,
    );
  }
});
