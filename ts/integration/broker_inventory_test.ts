/**
 * Real-boundary coverage for the complete broker inventory helper.
 *
 * A broker inventory is only trusted evidence when every reply was actually
 * validated: the authenticated `$SYS` system surface must answer with
 * self-consistent CONNZ envelopes for the exact server requested, and a
 * denied, silent, or withheld reply must make the inventory unavailable rather
 * than empty. This drives the helper against a real Trellis runtime, a real
 * broker, its system account, and the testkit native proxy.
 */

import type { NatsConnection } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { assert, assertEquals, assertRejects } from "@std/assert";
import { join } from "@std/path";

import { completeBrokerInventory } from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

const CONNZ_SUFFIX = ".CONNZ";

/** Reads the native proxy URL the runtime advertises to the control plane. */
function nativeProxyUrl(configText: string, natsUrl: string): string {
  const match = configText.match(
    /^\s*nats_servers\s*=\s*\[\s*"([^"]+)"\]/m,
  );
  assert(match, "the runtime must advertise a native NATS server");
  const proxyUrl = match[1]!;
  assert(
    proxyUrl !== natsUrl,
    "the system connection must not bypass the native proxy",
  );
  return proxyUrl;
}

/** Opens a real NATS connection with one of the runtime's credentials. */
async function connectWithCreds(
  url: string,
  credsPath: string,
): Promise<NatsConnection> {
  return await connect({
    servers: url,
    authenticator: credsAuthenticator(await Deno.readFile(credsPath)),
  });
}

Deno.test(
  "complete broker inventory is validated evidence, never a silent empty listing",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const systemCreds = join(
        runtime.workdir,
        "nats",
        "creds",
        "system.creds",
      );
      const trellisCreds = join(
        runtime.workdir,
        "nats",
        "creds",
        "trellis-auth.creds",
      );

      // 1. The real system inventory validates: a non-empty listing for the
      //    connection's own broker, every record a real connection.
      const system = await connectWithCreds(runtime.natsUrl, systemCreds);
      let serverId: string;
      try {
        const inventory = await completeBrokerInventory(system);
        assert(
          inventory.length > 0,
          "the authenticated inventory must list at least the system connection",
        );
        const infoServerId = system.info?.server_id;
        assert(
          typeof infoServerId === "string" && infoServerId.length > 0,
          "the system connection must report its server identity",
        );
        for (const connection of inventory) {
          assertEquals(
            connection.server,
            infoServerId,
            "every record must belong to the connection's own broker",
          );
          assert(
            connection.cid > 0,
            "every record must carry a positive cid",
          );
        }
        serverId = infoServerId;
      } finally {
        await system.close();
      }

      // 2. The same read without system permissions is unavailable evidence:
      //    a non-system account must reject rather than observe an empty
      //    inventory.
      const unauthorized = await connectWithCreds(
        runtime.natsUrl,
        trellisCreds,
      );
      try {
        await assertRejects(
          () => completeBrokerInventory(unauthorized),
          Error,
          undefined,
          "a non-system account must not observe an empty broker inventory",
        );
      } finally {
        await unauthorized.close();
      }

      // 3. A withheld reply must not become an empty inventory: hold the exact
      //    CONNZ reply for the captured server on the real native proxy while
      //    the required-server inventory is in flight.
      const config = await Deno.readTextFile(
        join(runtime.workdir, "trellis", "config.toml"),
      );
      const gated = await connectWithCreds(
        nativeProxyUrl(config, runtime.natsUrl),
        systemCreds,
      );
      try {
        const subject = `$SYS.REQ.SERVER.${serverId}${CONNZ_SUFFIX}`;
        const barrier = runtime.nativeTransportGate().armResponseHold(subject);
        const pending = completeBrokerInventory(gated, {
          requiredServerIds: [serverId],
        });
        let settled: unknown;
        pending.then(
          (value) => {
            settled = { value };
          },
          (error) => {
            settled = { error };
          },
        );
        const held = await barrier.held;
        assertEquals(held.requestSubject, subject);
        assertEquals(
          settled,
          undefined,
          "a withheld CONNZ reply must not settle the inventory",
        );
        await barrier.release();
        const inventory = await pending;
        assert(
          inventory.some((connection) => connection.server === serverId),
          "the released listing must cover the required server",
        );
        for (const connection of inventory) {
          assert(
            connection.cid > 0,
            "every released record must carry a positive cid",
          );
        }
      } finally {
        await gated.close();
      }
    }, { interruptibleNativeProxy: true });
  },
);
