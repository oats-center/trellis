/**
 * Loopback functional coverage for the intercepting TCP proxy's teardown.
 *
 * The proxy replaces `pipeTo` with a frame-aware pump so it can observe and
 * withhold protocol frames, which means it also owns close propagation. When one
 * direction ends, the other must not stay blocked on a peer that will never
 * write again, and `#forward` must report the connection closed. This drives a
 * real local client and a real upstream socket that holds its read side open,
 * which is exactly the condition that previously left both pumps waiting.
 *
 * Deno cannot signal a peer FIN on a connection whose own read is outstanding,
 * so this asserts the realizable contract: the local close tears the path down
 * and the connection is reported closed within a bound.
 */

import { assert } from "@std/assert";

import { NativeTransportGate } from "../src/native_gate.ts";
import { TcpProxy } from "../src/runtime.ts";
import { waitFor } from "../src/wait.ts";

function withTimeout<T>(
  promise: Promise<T>,
  ms: number,
  message: string,
): Promise<T> {
  return Promise.race([
    promise,
    new Promise<never>((_, reject) =>
      setTimeout(() => reject(new Error(message)), ms)
    ),
  ]);
}

Deno.test(
  "a local client close tears the proxied connection down and reports it closed",
  async () => {
    const upstream = Deno.listen({ hostname: "127.0.0.1", port: 0 });
    let accepted: Deno.Conn | undefined;
    let upstreamReader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    const upstreamTask = (async () => {
      try {
        for await (const connection of upstream) {
          accepted = connection;
          upstreamReader = connection.readable.getReader();
          // Hold the read side open, mirroring a peer that never writes: this is
          // exactly the condition that left both pumps waiting before the fix.
          for (;;) {
            const { done } = await upstreamReader.read();
            if (done) break;
          }
        }
      } catch {
        // The listener or connection was closed during teardown.
      }
    })();

    const gate = new NativeTransportGate();
    const proxy = TcpProxy.start(
      `tcp://127.0.0.1:${upstream.addr.port}`,
      { gate },
    );
    try {
      const port = Number(new URL(proxy.url).port);
      const client = await Deno.connect({ hostname: "127.0.0.1", port });
      await client.write(new Uint8Array([1, 2, 3]));
      // Wait until the proxy holds an upstream connection with a pending read.
      await waitFor(() =>
        accepted !== undefined && upstreamReader !== undefined
      );
      client.close();

      await withTimeout(
        gate.waitForClose(1),
        5_000,
        "the gate never observed the proxied connection close",
      );
      assert(
        gate.isClosed(1),
        "the proxied connection must be reported closed",
      );
    } finally {
      proxy.stop();
      upstream.close();
      await upstreamReader?.cancel().catch(() => undefined);
      try {
        accepted?.close();
      } catch {
        // The proxy may already have closed the upstream connection.
      }
      await upstreamTask;
    }
  },
);
