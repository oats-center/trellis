import assert from "node:assert/strict";
import { once } from "node:events";
import { createConnection, createServer, type Socket } from "node:net";
import { addAbortSignal } from "node:stream";
import { test } from "node:test";
import { NativeTransportGate } from "../src/native_gate.ts";
import { TcpProxy } from "../src/runtime.ts";

async function readExactly(socket: Socket, length: number): Promise<Buffer> {
  const chunks: Uint8Array[] = [];
  let received = 0;
  for await (const chunk of socket.iterator({ destroyOnReturn: false })) {
    chunks.push(chunk);
    received += chunk.length;
    if (received >= length) break;
  }
  return Buffer.concat(chunks);
}

test(
  "closing a client notifies the gate and sends upstream EOF without a peer reply",
  { timeout: 5_000 },
  async (context) => {
    await using upstream = createServer({ allowHalfOpen: true });
    upstream.listen({ host: "127.0.0.1", port: 0, signal: context.signal });
    await once(upstream, "listening", { signal: context.signal });
    const address = upstream.address();
    assert.ok(address && typeof address === "object");
    const gate = new NativeTransportGate();
    const proxy = await TcpProxy.start(`tcp://127.0.0.1:${address.port}`, {
      gate,
    });
    const accepted = once(upstream, "connection", { signal: context.signal });
    const client = createConnection({
      host: "127.0.0.1",
      port: Number(new URL(proxy.url).port),
      signal: context.signal,
    });
    let peer: Socket | undefined;
    try {
      [peer] = await accepted;
      assert.ok(peer);
      addAbortSignal(context.signal, peer);
      const probe = Buffer.from("PING\r\n");
      client.write(probe);
      assert.deepEqual(await readExactly(peer, probe.length), probe);
      const eof = once(peer, "end", { signal: context.signal });
      peer.resume();
      client.destroy();
      await Promise.all([gate.waitForClose(1), eof]);
    } finally {
      client.destroy();
      peer?.destroy();
      await proxy.stop();
    }
  },
);

test(
  "a gate-free proxy forwards binary data without CRLF in both directions before EOF",
  { timeout: 5_000 },
  async (context) => {
    await using upstream = createServer({ allowHalfOpen: true });
    upstream.listen({ host: "127.0.0.1", port: 0, signal: context.signal });
    await once(upstream, "listening", { signal: context.signal });
    const address = upstream.address();
    assert.ok(address && typeof address === "object");
    const proxy = await TcpProxy.start(`tcp://127.0.0.1:${address.port}`);
    const accepted = once(upstream, "connection", { signal: context.signal });
    const client = createConnection({
      host: "127.0.0.1",
      port: Number(new URL(proxy.url).port),
      signal: context.signal,
    });
    let peer: Socket | undefined;
    try {
      [peer] = await accepted;
      assert.ok(peer);
      addAbortSignal(context.signal, peer);
      const sent = Buffer.from([0x16, 0x03, 0x01, 0x00, 0xff, 0x81]);
      const reply = Buffer.from([0x16, 0x03, 0x03, 0x00, 0xde, 0xad]);
      client.write(sent);
      assert.deepEqual(await readExactly(peer, sent.length), sent);
      peer.write(reply);
      assert.deepEqual(await readExactly(client, reply.length), reply);
    } finally {
      client.destroy();
      peer?.destroy();
      await proxy.stop();
    }
  },
);
