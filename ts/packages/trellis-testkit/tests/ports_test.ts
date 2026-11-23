import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createConnection, type Socket } from "node:net";
import { test } from "node:test";
import { promisify } from "node:util";
import { reserveLocalPort, type ReservedPort } from "../src/ports.ts";

test("port reservations exclude other listeners until their release completes", async () => {
  const first = await reserveLocalPort();
  let second: ReservedPort | undefined;
  const server = createServer((_request, response) =>
    response.end("HTTP handoff"),
  );
  try {
    await assert.rejects(() => reserveLocalPort(first.port), {
      code: "EADDRINUSE",
    });
    await first.releaseForSpawn();
    await promisify(execFile)(process.execPath, [
      "--input-type=module",
      "--eval",
      `
      import assert from "node:assert/strict";
      import { reserveLocalPort } from ${JSON.stringify(new URL("../src/ports.ts", import.meta.url).href)};
      await assert.rejects(() => reserveLocalPort(${first.port}), { code: "EADDRINUSE" });
      `,
    ]);
    await Promise.all([first.releaseForSpawn(), first.release()]);
    second = await reserveLocalPort(first.port);
    await second.releaseForSpawn();
    server.listen(first.port, "127.0.0.1");
    await once(server, "listening");
    const response = await fetch(`http://127.0.0.1:${first.port}`);
    assert.equal(await response.text(), "HTTP handoff");
  } finally {
    if (server.listening) {
      await new Promise<void>((resolve, reject) => {
        server.close((error) => (error ? reject(error) : resolve()));
        server.closeAllConnections();
      });
    }
    await first.release();
    if (second) await second.release();
  }
});

// A runtime restarts its server on the same ports. While the old server is
// gone and the new one not yet started, another test process choosing ports
// must not get one of them, or the restart finds it in use for good.
test("a reserved port stays its owner's between the children it is handed to", async () => {
  const reservation = await reserveLocalPort();
  const ports = new URL("../src/ports.ts", import.meta.url).href;
  const serve = (text: string) =>
    spawn(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        `
        import { createServer } from "node:http";
        createServer((_request, response) => response.end(${JSON.stringify(text)}))
          .listen(${reservation.port}, "127.0.0.1", () => console.log("listening"));
        `,
      ],
      { stdio: ["ignore", "pipe", "inherit"] },
    );
  const handOff = async (text: string) => {
    await reservation.releaseForSpawn();
    const child = serve(text);
    try {
      const started = await Promise.race([
        once(child.stdout, "data").then(([line]) => String(line).trim()),
        once(child, "exit").then(([code]) => `exited with ${code}`),
      ]);
      assert.equal(started, "listening");
      const response = await fetch(`http://127.0.0.1:${reservation.port}`);
      assert.equal(await response.text(), text);
    } finally {
      if (child.exitCode === null && child.signalCode === null) {
        const exited = once(child, "exit");
        child.kill("SIGKILL");
        await exited;
      }
    }
  };
  try {
    await handOff("first server");
    await promisify(execFile)(process.execPath, [
      "--input-type=module",
      "--eval",
      `
      import assert from "node:assert/strict";
      import { reserveLocalPort } from ${JSON.stringify(ports)};
      await assert.rejects(() => reserveLocalPort(${reservation.port}), { code: "EADDRINUSE" });
      `,
    ]);
    await reservation.reacquire();
    await handOff("restarted server");
  } finally {
    await reservation.release();
  }
});

test("released listener ports remain bindable when automatic outbound ports are occupied", async () => {
  const [minimum, maximum] = (
    await readFile("/proc/sys/net/ipv4/ip_local_port_range", "utf8")
  )
    .trim()
    .split(/\s+/)
    .map(Number);
  const first = await reserveLocalPort();
  const peer = createServer();
  const replacement = createServer((_request, response) =>
    response.end("handoff"),
  );
  const connections: Socket[] = [];
  try {
    assert(first.port < minimum || first.port > maximum);
    peer.listen(0, "127.0.0.1");
    await once(peer, "listening");
    const address = peer.address();
    assert(address && typeof address !== "string");
    await first.releaseForSpawn();
    for (let index = 0; index < 4; index++) {
      const connection = createConnection({
        host: "127.0.0.1",
        port: address.port,
      });
      connections.push(connection);
      await once(connection, "connect");
      const localPort = connection.localPort;
      assert(
        localPort !== undefined && localPort >= minimum && localPort <= maximum,
      );
    }
    replacement.listen(first.port, "127.0.0.1");
    await once(replacement, "listening");
    assert.equal(
      await (await fetch(`http://127.0.0.1:${first.port}`)).text(),
      "handoff",
    );
  } finally {
    for (const connection of connections) connection.destroy();
    for (const server of [replacement, peer]) {
      if (server.listening) {
        await new Promise<void>((resolve, reject) => {
          server.close((error) => (error ? reject(error) : resolve()));
          server.closeAllConnections();
        });
      }
    }
    await first.release();
  }
});
