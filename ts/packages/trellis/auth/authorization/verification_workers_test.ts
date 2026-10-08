import { assert, assertEquals, assertRejects } from "@std/assert";
import { Buffer } from "node:buffer";
import vectors from "../../../../../integration/fixtures/protocol/authorization-context/vectors.json" with {
  type: "json",
};
import { AuthorizationVerificationWorkers } from "./verification_workers.ts";
import {
  assertAuthorizationRequestCurrentWasm,
  createAuthorizationContextHandleWasm,
  type PermissionAtom,
} from "../protocol_wasm.ts";

Deno.test({
  name:
    "unavailable worker assets reject startup without an uncaught parent error",
  permissions: { read: false },
  async fn() {
    await assertRejects(() => AuthorizationVerificationWorkers.open(10_000));
  },
});

Deno.test("worker verification authenticates native signed bytes and releases contexts", async () => {
  const pool = await AuthorizationVerificationWorkers.open(10_000, {
    requests: 2,
  });
  const entry = {};
  const policy = () => vectors.defaults.policy;
  const context = {
    issuer: {
      keyId: vectors.completeChain.issuerKeyId,
      publicKey: vectors.completeChain.issuerPublicKey,
      state: "active" as const,
    },
    signed: JSON.parse(vectors.completeChain.contextCanonicalJson),
    digest: vectors.completeChain.contextDigest,
  };
  const input = {
    ...vectors.defaults.request,
    payload: Buffer.concat([
      Buffer.from("prefix"),
      Buffer.from(vectors.defaults.request.payload),
      Buffer.from("suffix"),
    ]).subarray(6, -6),
    proof: vectors.completeChain.requestProof,
    requiredPermissions: [vectors.defaults.permission as PermissionAtom],
  };
  const { handle } = await createAuthorizationContextHandleWasm({
    issuer: context.issuer,
    context: context.signed,
    policy: { ...policy(), refreshLeadSeconds: 30, refreshJitterSeconds: 0 },
  });
  try {
    const verified = await pool.verify({
      entry,
      context,
      input,
      policy,
      lane: "ordinary",
    });
    assert(verified.ok);
    assertEquals(verified.contextDigest, context.digest);
    assertEquals(
      new TextDecoder().decode(input.payload),
      vectors.defaults.request.payload,
    );
    const tampered = await pool.verify({
      entry,
      context,
      input: { ...input, payload: new TextEncoder().encode("tampered") },
      policy,
      lane: "control",
    });
    assert(!tampered.ok);
    assertEquals(tampered.error.code, "InvalidRequestProof");
    const stale = assertAuthorizationRequestCurrentWasm(handle, input.iat, {
      ...policy(),
      nowUnixSeconds: input.iat + 31,
      refreshLeadSeconds: 30,
      refreshJitterSeconds: 0,
    });
    assert(!stale.ok);
    assertEquals(stale.error.code, "ProofIatOutOfRange");
    await pool.release(entry);
    // Released evidence must be installed afresh, not sent with a freed handle.
    assert(
      (await pool.verify({ entry, context, input, policy, lane: "ordinary" }))
        .ok,
    );
    const first = pool.verify({
      entry,
      context,
      input,
      policy,
      lane: "ordinary",
    });
    const second = pool.verify({
      entry,
      context,
      input,
      policy,
      lane: "ordinary",
    });
    await assertRejects(
      () => pool.verify({ entry, context, input, policy, lane: "ordinary" }),
      Error,
      "capacity exhausted",
    );
    assert((await first).ok);
    assert((await second).ok);
    await pool.release(entry);
    const reinstalled = await Promise.all([
      pool.verify({ entry, context, input, policy, lane: "ordinary" }),
      pool.verify({ entry, context, input, policy, lane: "ordinary" }),
    ]);
    assert(reinstalled.every((result) => result.ok));
    const order: string[] = [];
    const firstRefusal = pool.verify({
      entry,
      context,
      input,
      policy,
      lane: "refusal",
    }).then((result) => {
      assert(result.ok);
      order.push("first-refusal");
    });
    const nextRefusal = pool.verify({
      entry,
      context,
      input,
      policy,
      lane: "refusal",
    }).then((result) => {
      assert(result.ok);
      order.push("next-refusal");
    });
    const prioritizedControl = pool.verify({
      entry,
      context,
      input,
      policy,
      lane: "control",
    }).then((result) => {
      assert(result.ok);
      order.push("control");
    });
    await Promise.all([firstRefusal, nextRefusal, prioritizedControl]);
    assertEquals(order, ["first-refusal", "control", "next-refusal"]);
    const ordinary = pool.verify({
      entry,
      context,
      input,
      policy,
      lane: "ordinary",
    });
    const control = pool.verify({
      entry,
      context,
      input,
      policy,
      lane: "control",
    });
    pool.close();
    await assertRejects(() => ordinary, Error, "Verification workers closed");
    await assertRejects(() => control, Error, "Verification workers closed");
    await assertRejects(
      () => pool.verify({ entry, context, input, policy, lane: "ordinary" }),
      Error,
      "Verification workers closed",
    );
  } finally {
    handle.free();
    pool.close();
  }
});
