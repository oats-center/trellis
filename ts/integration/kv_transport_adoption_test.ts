import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { jetstreamManager } from "@nats-io/jetstream";
import { assert, assertEquals, assertRejects } from "@std/assert";
import { TypedKV } from "../packages/trellis/kv.ts";
import { TransportError } from "../packages/trellis/errors/index.ts";
import {
  resourceTransportCheck,
  TransportAuthorizationState,
  type TransportAuthorizationGate,
} from "../packages/trellis/auth/authorization/transport_state.ts";
import {
  TRANSPORT_AUTHORIZATION_FORMAT_V1,
  type TransportAuthorizationV1,
} from "../packages/trellis/auth/protocol_wasm.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

const representation = {
  version: 1,
  codec: {
    encode: (value: number) => value,
    decode: (value: unknown) => Number(value),
  },
  migrations: {},
};

const ACCOUNT = `A${"B".repeat(55)}`;

function canonical(subjects: string[]): string[] {
  return [...new Set(subjects)].sort();
}

function policy(publishAllow: string[]): TransportAuthorizationV1 {
  return {
    format: TRANSPORT_AUTHORIZATION_FORMAT_V1,
    account: ACCOUNT,
    publishAllow: canonical(publishAllow),
    subscribeAllow: [],
    response: null,
    hardExpiresAt: null,
  };
}

const readSubjects = (bucket: string) => [
  `$JS.API.DIRECT.GET.KV_${bucket}`,
  `$JS.API.STREAM.INFO.KV_${bucket}`,
];
const writeSubjects = (bucket: string) => [`$KV.${bucket}.>`];

function gateFor(state: TransportAuthorizationState): TransportAuthorizationGate {
  return {
    status: () => state.status(),
    admittedPolicy: () => state.admittedPolicy(),
    allowedPolicy: () => state.allowedPolicy(),
    nowSeconds: () => 1_000,
  };
}

async function admit(
  state: TransportAuthorizationState,
  admitted: string[],
  allowed: string[],
): Promise<void> {
  await state.recordAdmission({
    contextDigest: `d-${crypto.randomUUID()}`,
    policy: policy(admitted),
    allowed: policy(allowed),
    nowUnixSeconds: 1_000,
  });
}

/**
 * Proves client transport adoption against a real broker: a newly granted
 * resource is bound but not opened, its operations fail with
 * transport_upgrade_required while the admitted attachment lags, and the same
 * handle performs the real broker operation once admission advances. The broker
 * connection carries full permissions, so the only variable is the client gate.
 */
Deno.test("a bound KV resource waits for transport adoption before it is used", async () => {
  await withTrellisRuntime(async (runtime) => {
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          `${runtime.workdir}/nats/creds/trellis-auth.creds`,
        ),
      ),
    });
    try {
      // Granted by the newest policy, admitted by neither read nor write: every
      // operation fails before NATS, and the bucket must never be created.
      const pendingBucket = `kv-pending-${crypto.randomUUID()}`;
      const pendingState = new TransportAuthorizationState();
      await admit(pendingState, [], [
        ...readSubjects(pendingBucket),
        ...writeSubjects(pendingBucket),
      ]);
      const pending = TypedKV.bind(nats, pendingBucket, representation, {
        history: 1,
        isCurrent: () => true,
        transport: resourceTransportCheck(
          gateFor(pendingState),
          "kv",
          pendingBucket,
        ),
      });
      for (const result of [await pending.get("missing"), await pending.put("k", 1)]) {
        assert(result.isErr());
        assertEquals(
          result.error instanceof TransportError && result.error.code,
          "transport_upgrade_required",
        );
      }
      await assertRejects(
        async () => await (await jetstreamManager(nats)).streams.info(
          `KV_${pendingBucket}`,
        ),
        Error,
        undefined,
        "a pending resource must not be opened against NATS",
      );

      // Admitted for reads, not for writes: the handle works for reads and
      // withholds the write until adoption.
      const bucket = `kv-growing-${crypto.randomUUID()}`;
      const state = new TransportAuthorizationState();
      await admit(state, readSubjects(bucket), [
        ...readSubjects(bucket),
        ...writeSubjects(bucket),
      ]);
      const kv = TypedKV.bind(nats, bucket, representation, {
        history: 1,
        isCurrent: () => true,
        transport: resourceTransportCheck(gateFor(state), "kv", bucket),
      });
      assertEquals(await kv.get("missing").orThrow(), undefined);
      const withheld = await kv.put("k", 1);
      assert(withheld.isErr());
      assertEquals(
        withheld.error instanceof TransportError && withheld.error.code,
        "transport_upgrade_required",
      );

      // Adoption: the same state advances to cover the write, and the same
      // handle performs the real broker operation.
      await admit(state, [...readSubjects(bucket), ...writeSubjects(bucket)], [
        ...readSubjects(bucket),
        ...writeSubjects(bucket),
      ]);
      assertEquals((await kv.put("k", 1).orThrow()).value, 1);
      assertEquals(await kv.get("k").orThrow(), 1);
    } finally {
      await nats.close();
    }
  });
});
