import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { test } from "node:test";
import { promisify } from "node:util";
import { reserveLocalPort, type ReservedPort } from "../src/ports.ts";

test("port reservations exclude other listeners until their release completes", async () => {
  const first = await reserveLocalPort();
  let second: ReservedPort | undefined;
  try {
    await assert.rejects(() => reserveLocalPort(first.port), {
      code: "EADDRINUSE",
    });
    await Promise.all([first.releaseForSpawn(), first.release()]);
    second = await reserveLocalPort(first.port);
  } finally {
    await first.release();
    if (second) await second.release();
  }
});

test("released listener ports remain bindable when automatic outbound ports are occupied", async () => {
  await promisify(execFile)(
    "unshare",
    [
      "--user",
      "--map-root-user",
      "--net",
      "sh",
      "-c",
      'ip link set lo up && sysctl -q -w net.ipv4.ip_local_port_range="32768 32771" && exec "$@"',
      "port-availability",
      process.execPath,
      "--experimental-transform-types",
      "--input-type=module",
      "--eval",
      `
      import { createServer, createConnection } from "node:net";
      import { once } from "node:events";
      import { reserveLocalPort } from ${JSON.stringify(new URL("../src/ports.ts", import.meta.url).href)};
      const peer = createServer();
      peer.listen(12000, "127.0.0.1");
      await once(peer, "listening");
      const first = await reserveLocalPort();
      await first.releaseForSpawn();
      const connections = [];
      let replacement;
      try {
        for (let index = 0; index < 4; index++) {
          const connection = createConnection({ host: "127.0.0.1", port: 12000 });
          connections.push(connection);
          await once(connection, "connect");
        }
        replacement = await reserveLocalPort(first.port);
      } finally {
        await replacement?.release();
        for (const connection of connections) connection.destroy();
        await new Promise(resolve => peer.close(resolve));
      }
    `,
    ],
    { timeout: 10_000 },
  );
});
