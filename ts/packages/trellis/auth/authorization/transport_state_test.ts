import { assertEquals } from "@std/assert";

import {
  admissionContextDigest,
  admittedPolicyCovers,
  readOwnAdmission,
  requiresTransportUpgrade,
  resourceTransportCheck,
  TransportAuthorizationState,
  type TransportAuthorizationStatus,
  transportUpgradeRequiredError,
} from "./transport_state.ts";
import {
  TRANSPORT_AUTHORIZATION_FORMAT_V1,
  type TransportAuthorizationV1,
} from "../protocol_wasm.ts";

const policy = (
  over: Partial<TransportAuthorizationV1> = {},
): TransportAuthorizationV1 => ({
  format: TRANSPORT_AUTHORIZATION_FORMAT_V1,
  account: "AACCOUNT",
  publishAllow: [],
  subscribeAllow: [],
  response: null,
  hardExpiresAt: null,
  ...over,
});

Deno.test("admission names parse only Trellis callout markers", () => {
  assertEquals(
    admissionContextDigest("trellis.auth.v1:digest-abc:NATSSERVER:42"),
    "digest-abc",
  );
  assertEquals(admissionContextDigest("someone-else"), undefined);
  assertEquals(
    admissionContextDigest("trellis.auth.v1:no-separator"),
    undefined,
  );
  assertEquals(admissionContextDigest("trellis.auth.v1:has:server:42"), "has");
});

Deno.test("transport status compares admitted A against newest allowed D", async () => {
  const state = new TransportAuthorizationState();
  const seen: TransportAuthorizationStatus[] = [];
  state.onStatusChanged((status) => seen.push(status));

  await state.recordAdmission({
    contextDigest: "d1",
    policy: policy({ publishAllow: ["rpc.v1.Orders.*"] }),
    allowed: policy({ publishAllow: ["rpc.v1.>"] }),
    nowUnixSeconds: 1_000,
  });
  assertEquals(state.status(), "upgrade_available");

  // A duplicate renewal repeats the admitted capability, so it is current-only.
  await state.recompute(policy({ publishAllow: ["rpc.v1.Orders.*"] }), 1_000);
  assertEquals(state.status(), "current");

  // A genuinely narrowed current policy means the admitted socket is dead.
  await state.recompute(policy({ publishAllow: ["other"] }), 1_000);
  assertEquals(state.status(), "unavailable");

  // Only semantic changes are published: unavailable, upgrade, current,
  // unavailable.
  assertEquals(seen, [
    "unavailable",
    "upgrade_available",
    "current",
    "unavailable",
  ]);
});

Deno.test("a real disconnect clears admitted transport state", async () => {
  const state = new TransportAuthorizationState();
  await state.recordAdmission({
    contextDigest: "d1",
    policy: policy({ publishAllow: ["a"] }),
    allowed: policy({ publishAllow: ["a"] }),
    nowUnixSeconds: 1_000,
  });
  assertEquals(state.status(), "current");

  state.markDisconnected();
  assertEquals(state.status(), "unavailable");
  assertEquals(state.admittedDigest(), undefined);
});

Deno.test("own admission reads tolerate missing and malformed replies", async () => {
  const valid = await readOwnAdmission({
    request: () =>
      Promise.resolve({
        json: <T>() =>
          ({ data: { user: "trellis.auth.v1:digest-xyz:SERVER:7" } }) as T,
      }),
  }, 100);
  assertEquals(valid?.contextDigest, "digest-xyz");

  const missing = await readOwnAdmission({
    request: () => Promise.resolve({ json: <T>() => ({}) as T }),
  }, 100);
  assertEquals(missing, undefined);

  const failing = await readOwnAdmission({
    request: () => Promise.reject(new Error("no responder")),
  }, 100);
  assertEquals(failing, undefined);
});

Deno.test("admitted policy coverage follows exact NATS wildcard containment", async () => {
  const wide = policy({ publishAllow: ["rpc.v1.>"] });
  assertEquals(
    await admittedPolicyCovers(wide, { publish: ["rpc.v1.Orders.Get"] }, 1_000),
    true,
  );
  const single = policy({ publishAllow: ["a.*"] });
  assertEquals(
    await admittedPolicyCovers(single, { publish: ["a.b"] }, 1_000),
    true,
  );
  // `a.*` matches exactly one token, so it does not cover a nested path.
  assertEquals(
    await admittedPolicyCovers(single, { publish: ["a.b.c"] }, 1_000),
    false,
  );
  // Publish authority does not satisfy a subscribe need.
  assertEquals(
    await admittedPolicyCovers(wide, { subscribe: ["x"] }, 1_000),
    false,
  );
});

Deno.test("upgrade-required failure carries the distinct runtime code", () => {
  const error = transportUpgradeRequiredError({ method: "Orders.Get" });
  assertEquals(error.code, "transport_upgrade_required");
});

Deno.test("boundary gate separates granted-but-unadopted from denied", async () => {
  const state = new TransportAuthorizationState();
  const gate = {
    status: () => state.status(),
    admittedPolicy: () => state.admittedPolicy(),
    allowedPolicy: () => state.allowedPolicy(),
    nowSeconds: () => 1_000,
  };
  await state.recordAdmission({
    contextDigest: "d1",
    policy: policy({ publishAllow: ["rpc.v1.Orders.Get"] }),
    allowed: policy({
      publishAllow: ["rpc.v1.Orders.Get", "rpc.v1.Users.Get"],
    }),
    nowUnixSeconds: 1_000,
  });
  assertEquals(state.status(), "upgrade_available");
  // Granted by the newest policy but absent from the admitted attachment.
  assertEquals(
    await requiresTransportUpgrade(gate, { publish: ["rpc.v1.Users.Get"] }),
    true,
  );
  // Already admitted: no upgrade needed.
  assertEquals(
    await requiresTransportUpgrade(gate, { publish: ["rpc.v1.Orders.Get"] }),
    false,
  );
  // Granted by nobody: an ordinary denial, not a transport upgrade.
  assertEquals(
    await requiresTransportUpgrade(gate, { publish: ["rpc.v1.Secret.Get"] }),
    false,
  );
});

Deno.test("resource transport markers match the server-compiled grants", async () => {
  type Fixture = {
    bucket: string;
    readPublish: string[];
    writePublish: string[];
  };
  const fixture = JSON.parse(
    await Deno.readTextFile(
      new URL(
        "../../../../../integration/fixtures/protocol/resource-grants/vectors.json",
        import.meta.url,
      ),
    ),
  ) as Record<"kv" | "store", Fixture>;

  for (const kind of ["kv", "store"] as const) {
    const { bucket, readPublish, writePublish } = fixture[kind];
    const canonical = (subjects: string[]) => [...new Set(subjects)].sort();
    const state = new TransportAuthorizationState();
    await state.recordAdmission({
      contextDigest: "d1",
      policy: policy({ publishAllow: canonical(readPublish) }),
      allowed: policy({
        publishAllow: canonical([...readPublish, ...writePublish]),
      }),
      nowUnixSeconds: 1_000,
    });
    assertEquals(state.status(), "upgrade_available");
    const check = resourceTransportCheck(
      {
        status: () => state.status(),
        admittedPolicy: () => state.admittedPolicy(),
        allowedPolicy: () => state.allowedPolicy(),
        nowSeconds: () => 1_000,
      },
      kind,
      bucket,
    );

    assertEquals(await check("read"), undefined, `${kind} read is admitted`);
    const write = await check("write");
    assertEquals(
      write?.code,
      "transport_upgrade_required",
      `${kind} write must await adoption`,
    );
  }
});
