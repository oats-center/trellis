/**
 * Loopback functional coverage for the intercepting TCP proxy.
 *
 * The proxy replaces `pipeTo` with a pump that owns close propagation: when one
 * direction ends, the other must not stay blocked on a peer that will never
 * write again, and `#forward` must report the connection closed. The first test
 * drives a real local client and a real upstream socket that holds its read side
 * open to prove that. The second proves a gate-free proxy is byte-transparent:
 * binary payloads with no CRLF must pass through unchanged in both directions,
 * where a frame-aware pump would buffer them waiting for a boundary.
 */

import { assert, assertEquals } from "@std/assert";

import { NativeTransportGate } from "../src/native_gate.ts";
import { TcpProxy } from "../src/runtime.ts";
import { waitFor } from "../src/wait.ts";

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

/** Whether an error is an ordinary socket close race during teardown. */
function isCloseError(error: unknown): boolean {
  return error instanceof Deno.errors.BadResource ||
    error instanceof Deno.errors.BrokenPipe ||
    error instanceof Deno.errors.ConnectionReset;
}

/** Read exactly `length` bytes, or fail after `ms`; the timer is always cleared. */
async function readExactly(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  length: number,
  ms: number,
): Promise<Uint8Array> {
  const buffer = new Uint8Array(length);
  let offset = 0;
  await withTimeout(
    (async () => {
      while (offset < length) {
        const { done, value } = await reader.read();
        if (done) break;
        const take = Math.min(value.length, length - offset);
        buffer.set(value.subarray(0, take), offset);
        offset += take;
      }
    })(),
    ms,
    `stream ended after ${offset}/${length} bytes`,
  );
  assertEquals(offset, length, `expected ${length} bytes, received ${offset}`);
  return buffer;
}

Deno.test(
  "a local client close tears the proxied connection down and reports it closed",
  async () => {
    const upstream = Deno.listen({ hostname: "127.0.0.1", port: 0 });
    let accepted: Deno.Conn | undefined;
    let upstreamReader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    // Independent proof that the upstream's own read side reached EOF. The
    // proxied connection's closed flag is set by the proxy from its own
    // teardown, so it cannot by itself distinguish a real upstream EOF from a
    // proxy that dropped its bookkeeping while leaving the upstream attached.
    // This resolves only when the upstream reader observes `done`; a read error
    // is recorded instead of being counted as EOF.
    let acknowledgeUpstreamEof: (() => void) | undefined;
    const upstreamEof = new Promise<void>((resolve) => {
      acknowledgeUpstreamEof = resolve;
    });
    let upstreamReadError: unknown;
    const upstreamTask = (async () => {
      try {
        for await (const connection of upstream) {
          accepted = connection;
          upstreamReader = connection.readable.getReader();
          // Hold the read side open, mirroring a peer that never writes: this is
          // exactly the condition that left both pumps waiting before the fix.
          try {
            for (;;) {
              const { done } = await upstreamReader.read();
              if (done) {
                acknowledgeUpstreamEof?.();
                break;
              }
            }
          } catch (error) {
            // A read failure (reset, bad resource) is not a clean EOF: keep it
            // so the test fails rather than accepting a broken teardown.
            upstreamReadError = error;
          }
        }
      } catch {
        // The listener was closed during teardown.
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
      // Assert the upstream EOF before teardown closes or cancels the upstream
      // reader, so a gate flag alone can never satisfy this test.
      await withTimeout(
        upstreamEof,
        5_000,
        "the upstream never observed the client close as EOF",
      );
      assert(
        upstreamReadError === undefined,
        `the upstream read failed instead of reaching EOF: ${upstreamReadError}`,
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

/**
 * Binary bytes with no CRLF, chosen so they cannot be read as NATS control lines
 * (which are ASCII and CRLF-terminated). A frame-aware pump buffers these
 * forever waiting for a boundary that never arrives.
 */
const CLIENT_BYTES = new Uint8Array([
  0x16,
  0x03,
  0x01,
  0x00,
  0x2f,
  0x01,
  0x00,
  0x00,
  0x2b,
  0x03,
  0x03,
  0x81,
  0x85,
  0x37,
  0xfa,
  0x21,
  0x3d,
  0x7f,
  0x9e,
  0x00,
]);
const SERVER_BYTES = new Uint8Array([
  0x16,
  0x03,
  0x03,
  0x00,
  0x35,
  0x02,
  0x00,
  0x00,
  0x31,
  0x03,
  0x03,
  0x88,
  0x82,
  0x03,
  0xea,
  0xde,
  0xad,
  0xbe,
  0xef,
  0xff,
]);

Deno.test(
  "a gate-free proxy forwards binary bytes unchanged in both directions",
  async () => {
    const upstream = Deno.listen({ hostname: "127.0.0.1", port: 0 });
    let accepted: Deno.Conn | undefined;
    // The upstream runs its own assertions; the test observes this task so a
    // failure is reported rather than hidden by teardown handling.
    const upstreamTask = (async () => {
      for await (const connection of upstream) {
        accepted = connection;
        const reader = connection.readable.getReader();
        // The upstream must receive the client bytes verbatim, before any EOF.
        const received = await readExactly(reader, CLIENT_BYTES.length, 5_000);
        assertEquals(
          received,
          CLIENT_BYTES,
          "the upstream must receive the client bytes verbatim",
        );
        // Reply with bytes that are also not a valid NATS control line.
        await connection.writable.getWriter().write(SERVER_BYTES);
        return;
      }
    })();

    const proxy = TcpProxy.start(`tcp://127.0.0.1:${upstream.addr.port}`);
    let client: Deno.Conn | undefined;
    let teardownError: unknown;
    try {
      const port = Number(new URL(proxy.url).port);
      client = await Deno.connect({ hostname: "127.0.0.1", port });
      const clientReader = client.readable.getReader();
      await client.writable.getWriter().write(CLIENT_BYTES);

      // The client must receive the reply verbatim, before any EOF.
      const reply = await readExactly(clientReader, SERVER_BYTES.length, 5_000);
      assertEquals(
        reply,
        SERVER_BYTES,
        "the client must receive the upstream bytes verbatim",
      );
      // Awaited while the exchange is still live so the upstream assertions
      // surface here instead of being swallowed by cleanup.
      await withTimeout(
        upstreamTask,
        5_000,
        "the upstream never observed the client bytes",
      );
    } finally {
      // Teardown suppresses only the expected resource-close races; anything
      // unexpected is recorded and rethrown after the block so the upstream
      // task's own result is not lost.
      const closeQuietly = (close: () => void): void => {
        try {
          close();
        } catch (error) {
          if (!isCloseError(error)) teardownError = error;
        }
      };
      closeQuietly(() => client?.close());
      proxy.stop();
      closeQuietly(() => upstream.close());
      closeQuietly(() => accepted?.close());
      await upstreamTask.catch((error) => {
        if (!isCloseError(error)) teardownError = error;
      });
    }
    if (teardownError !== undefined) throw teardownError;
  },
);

/**
 * A refused upstream must be a connection-local failure.
 *
 * Reserving and then closing an ephemeral listener leaves nothing accepting on
 * that loopback port, so the kernel refuses the proxy's own upstream connect
 * instead of resetting it after acceptance. That refusal must still tear the
 * client connection down — the client observes EOF — and must never escape as
 * a process-wide unhandled rejection, which is why this test also fails the
 * whole run when the launch-boundary rejection consumption is removed.
 */
Deno.test(
  "a refused upstream tears the client down without an unhandled rejection",
  async () => {
    // Reserve an ephemeral loopback port, then close the listener so a connect
    // to it is refused rather than accepted.
    const reserved = Deno.listen({ hostname: "127.0.0.1", port: 0 });
    const refusedPort = reserved.addr.port;
    reserved.close();

    const proxy = TcpProxy.start(`tcp://127.0.0.1:${refusedPort}`);
    let client: Deno.Conn | undefined;
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    try {
      const port = Number(new URL(proxy.url).port);
      client = await Deno.connect({ hostname: "127.0.0.1", port });
      reader = client.readable.getReader();
      // No upstream byte can ever arrive, so the read settles only when the
      // proxy ends the client's direction after the refused connect.
      const { done } = await withTimeout(
        reader.read(),
        5_000,
        "the client never observed the refused upstream tear the connection down",
      );
      assert(done, "the client must observe EOF when the upstream is refused");
    } finally {
      await reader?.cancel().catch(() => undefined);
      try {
        client?.close();
      } catch {
        // The proxy may already have closed the client connection.
      }
      proxy.stop();
    }
  },
);
