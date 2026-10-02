/**
 * Real-broker closure through the native transport proxy.
 *
 * This isolates one question at the real broker boundary: when an ordinary NATS
 * client closes the connection it opened to the control plane's advertised
 * native NATS server (the testkit TCP proxy), does the broker's own validated
 * CONNZ inventory stop listing that exact `server:cid`?
 *
 * It never arms the readiness gate and never inspects the proxy's own closed
 * flag. A system-account observer reads the broker's authenticated `$SYS`
 * inventory directly, the subject connections are opened through the proxy URL
 * from the generated `[client] nats_servers`, and absence is only concluded
 * from a fully validated listing. Both the graceful `drain()` and the immediate
 * `close()` legs are exercised, because the two make the client's transport
 * close the socket at different points and only one of them is the shape a
 * retired generation uses.
 */

import type { NatsConnection } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";

import {
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Reads the native proxy URL the runtime advertises to the control plane. */
function nativeProxyUrl(configText: string, natsUrl: string): string {
  const match = configText.match(
    /^\s*nats_servers\s*=\s*\[\s*"([^"]+)"\]/m,
  );
  assert(match, "the runtime must advertise a native NATS server");
  const proxyUrl = match[1]!;
  assert(
    proxyUrl !== natsUrl,
    "the subject connection must not bypass the native proxy",
  );
  return proxyUrl;
}

/** Reject with `message` if `promise` has not settled within `ms`. */
function withTimeout<T>(
  promise: Promise<T>,
  ms: number,
  message: string,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => {
    if (timer !== undefined) clearTimeout(timer);
  });
}

type CloseMode = "drain" | "close";

/**
 * Open one subject connection through the proxy, prove its exact broker
 * identity is listed, close it with `mode`, then require the exact identity to
 * leave a fully validated broker listing.
 */
async function expectIdentityLeaves(
  system: NatsConnection,
  proxyUrl: string,
  creds: Uint8Array,
  mode: CloseMode,
): Promise<void> {
  const subject: NatsConnection = await connect({
    servers: proxyUrl,
    authenticator: credsAuthenticator(creds),
  });
  const serverId = subject.info?.server_id;
  const cid = subject.info?.client_id;
  assert(
    typeof serverId === "string" && serverId.length > 0,
    `[${mode}] the subject connection must report its broker server id`,
  );
  assert(
    typeof cid === "number" && cid > 0,
    `[${mode}] the subject connection must report its broker client id`,
  );
  const key = `${serverId}:${cid}`;
  try {
    const before = await completeBrokerInventory(system, {
      requiredServerIds: [serverId],
    });
    assert(
      before.map(brokerConnectionKey).includes(key),
      `[${mode}] the subject connection ${key} must appear in the validated inventory`,
    );

    if (mode === "drain") {
      await withTimeout(subject.drain(), 15_000, `[${mode}] drain hung`);
    } else {
      await withTimeout(subject.close(), 15_000, `[${mode}] close hung`);
    }
    await withTimeout(
      subject.closed().then(() => undefined),
      15_000,
      `[${mode}] connection never reported closed`,
    );
    console.error(`[native-close ${mode}] subject ${key} closed`);

    const deadline = Date.now() + 60_000;
    let present = true;
    while (Date.now() < deadline) {
      const inventory = await completeBrokerInventory(system, {
        requiredServerIds: [serverId],
      }).catch(() => undefined);
      if (inventory === undefined) continue;
      present = inventory.map(brokerConnectionKey).includes(key);
      if (!present) break;
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
    assertEquals(
      present,
      false,
      `[${mode}] the broker must stop listing the closed connection ${key}`,
    );
    console.error(`[native-close ${mode}] broker no longer lists ${key}`);
  } finally {
    await subject.close().catch(() => undefined);
  }
}

Deno.test(
  "a real NATS connection closed through the native proxy leaves the broker inventory",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const systemCreds = await Deno.readFile(
        join(runtime.workdir, "nats", "creds", "system.creds"),
      );
      const system: NatsConnection = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(systemCreds),
      });
      try {
        const config = await Deno.readTextFile(
          join(runtime.workdir, "trellis", "config.toml"),
        );
        const proxyUrl = nativeProxyUrl(config, runtime.natsUrl);
        await expectIdentityLeaves(system, proxyUrl, systemCreds, "drain");
        await expectIdentityLeaves(system, proxyUrl, systemCreds, "close");
      } finally {
        await system.close().catch(() => undefined);
      }
    }, { interruptibleNativeProxy: true });
  },
);
