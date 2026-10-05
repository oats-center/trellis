/**
 * Real-boundary functional coverage for the one-shot response barrier.
 *
 * The barrier is testkit transport instrumentation: it must withhold exactly one
 * real request's server reply on the wire while every other frame on the same
 * physical connection keeps flowing. This drives it over the real Trellis native
 * proxy against a real NATS server. A system-account connection issues a real
 * `$SYS.REQ.SERVER.PING.CONNZ` request whose reply is withheld; an unrelated
 * `$SYS.REQ.SERVER.PING.VARZ` request on the same connection must still complete
 * while the first reply is retained; releasing must then deliver the retained
 * bytes so the held request settles with real data.
 */

import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { headers as natsHeaders } from "@nats-io/nats-core";
import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import { join } from "@std/path";

import { withTrellisRuntime } from "./_support/runtime.ts";

const CONNZ = "$SYS.REQ.SERVER.PING.CONNZ";
const VARZ = "$SYS.REQ.SERVER.PING.VARZ";
/** Subject prefix the two real requests above share, used to arm by body. */
const SYS_REQ_SERVER = "$SYS.REQ.SERVER.";
/** The original request bound the barrier must not disturb. */
const REQUEST_TIMEOUT_MS = 2_000;

/**
 * Decode one real `$SYS` server reply and prove it is the reporting server's
 * genuine envelope, not an error string that merely mentions a server id: the
 * reply must parse as a JSON object whose `server` envelope and `data` payload
 * identify the same server and that carries no `error`. Returns that identity.
 */
function assertServerReply(data: Uint8Array, subject: string): string {
  const text = new TextDecoder().decode(data);
  const parsed: unknown = JSON.parse(text);
  assert(
    parsed !== null && typeof parsed === "object" && !Array.isArray(parsed),
    `${subject} must reply with a JSON server envelope: ${text}`,
  );
  const envelope = parsed as {
    error?: unknown;
    server?: { id?: unknown };
    data?: { server_id?: unknown };
  };
  assert(
    envelope.error === undefined,
    `${subject} must not reply with an error: ${text}`,
  );
  const serverId = envelope.server?.id;
  assert(
    typeof serverId === "string" && serverId.length > 0,
    `${subject} must carry the reporting server envelope: ${text}`,
  );
  assert(
    envelope.data?.server_id === serverId,
    `${subject} must carry real data for the reporting server: ${text}`,
  );
  return serverId;
}

/**
 * True only for a real CONNZ reply. A CONNZ reply carries `data.connections` as
 * the array of per-connection records, while a VARZ reply on the same prefix
 * carries it as a plain connection count, so the body is what tells the two
 * concurrent replies apart.
 */
function isConnzReply(body: Uint8Array): boolean {
  const parsed: unknown = JSON.parse(new TextDecoder().decode(body));
  if (parsed === null || typeof parsed !== "object") return false;
  return Array.isArray(
    (parsed as { data?: { connections?: unknown } }).data?.connections,
  );
}

Deno.test(
  "the readiness barrier holds sequential physical connections independently",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const proxyUrl = runtime.nativeProxyUrl();
      assert(
        proxyUrl !== runtime.natsUrl,
        "connections must traverse the proxy",
      );
      const options = {
        servers: proxyUrl,
        timeout: REQUEST_TIMEOUT_MS,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            join(
              runtime.workdir,
              "config",
              "trellis",
              "nats",
              "creds",
              "system.creds",
            ),
          ),
        ),
      };
      const gate = runtime.nativeTransportGate();
      const route = `hold.readiness.${crypto.randomUUID()}`;
      gate.arm(route);
      const firstBarrier = gate.barrierHeld();
      const first = await connect(options);
      let second: Awaited<ReturnType<typeof connect>> | undefined;
      let responseHold: ReturnType<typeof gate.armResponseHold> | undefined;
      try {
        first.subscribe(route);
        const firstFlush = first.flush();
        const firstId = await Promise.race([
          firstBarrier,
          firstFlush.then(() => {
            throw new Error("the first subscription flush escaped the hold");
          }),
        ]);
        // Rearm while the previous release's tracked writes are still pending.
        // Those writes must retain their original connection and frame snapshot.
        const firstRelease = gate.release();
        gate.arm(route);
        const secondBarrier = gate.barrierHeld();
        let secondHeld = false;
        secondBarrier.then(() => {
          secondHeld = true;
        });
        await firstRelease;
        await firstFlush;

        // A new matching SUB on the old connection must neither capture the new
        // hold nor be blocked by it. Its round trip also exposes stale resolved
        // barrier promises without timing sleeps.
        first.subscribe(route);
        await first.flush();
        assertEquals(
          secondHeld,
          false,
          "rearming must not reuse the released connection's barrier result",
        );
        assertThrows(() => gate.arm(route), Error, "already armed");

        second = await connect(options);
        second.subscribe(route);
        const secondFlush = second.flush();
        let secondFlushSettled = false;
        secondFlush.then(
          () => {
            secondFlushSettled = true;
          },
          () => {
            secondFlushSettled = true;
          },
        );
        const secondId = await Promise.race([
          secondBarrier,
          secondFlush.then(() => {
            throw new Error("the second subscription flush escaped the hold");
          }),
        ]);
        assert(
          secondId !== firstId,
          "the second hold must select a fresh connection",
        );
        assertEquals(
          await firstBarrier,
          firstId,
          "the old result must stay stable",
        );
        assertThrows(() => gate.arm(route), Error, "already armed");

        // Readiness and response holds are independent, including release.
        responseHold = gate.armResponseHold(CONNZ, firstId);
        const connz = first.request(CONNZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        let connzSettled = false;
        connz.then(
          () => {
            connzSettled = true;
          },
          () => {
            connzSettled = true;
          },
        );
        await Promise.race([
          responseHold.held,
          connz.then(() => {
            throw new Error(
              "the response hold escaped during the readiness hold",
            );
          }),
        ]);
        const varz = await first.request(VARZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        const serverId = assertServerReply(varz.data, VARZ);
        assertEquals(
          secondFlushSettled,
          false,
          "the second flush must still be held",
        );
        assertEquals(connzSettled, false);

        await gate.release();
        await secondFlush;
        await first.flush();
        assertEquals(
          connzSettled,
          false,
          "readiness release must not release a reply",
        );
        await responseHold.release();
        assertEquals(assertServerReply((await connz).data, CONNZ), serverId);
        assertEquals(await firstBarrier, firstId);
        assertEquals(await secondBarrier, secondId);
      } finally {
        await gate.release().catch(() => undefined);
        await responseHold?.release().catch(() => undefined);
        await second?.close();
        await first.close();
      }
    }, { interruptibleNativeProxy: true });
  },
);

Deno.test(
  "closed physical connections discard retained PONGs and replies without disturbing fresh holds",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const proxyUrl = runtime.nativeProxyUrl();
      const options = {
        servers: proxyUrl,
        timeout: REQUEST_TIMEOUT_MS,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            join(
              runtime.workdir,
              "config",
              "trellis",
              "nats",
              "creds",
              "system.creds",
            ),
          ),
        ),
      };
      const gate = runtime.nativeTransportGate();
      const route = `hold.closed.${crypto.randomUUID()}`;
      gate.arm(route);
      const first = await connect(options);
      let second: Awaited<ReturnType<typeof connect>> | undefined;
      let responseHold = gate.armResponseHold(CONNZ);
      try {
        first.subscribe(route);
        const firstFlush = first.flush();
        firstFlush.catch(() => undefined);
        const firstId = await Promise.race([
          gate.barrierHeld(),
          firstFlush.then(() => {
            throw new Error("the closing connection's flush escaped its hold");
          }),
        ]);
        const firstRequest = first.request(CONNZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        const requestRejected = firstRequest.then(() => false, () => true);
        const held = await Promise.race([
          responseHold.held,
          firstRequest.then(() => {
            throw new Error("the closing connection's reply escaped its hold");
          }),
        ]);
        assertEquals(held.connectionId, firstId);

        // Both real frames are retained before explicitly ending their owner.
        // Observe the proxy's physical close, not just the client close promise.
        await first.close();
        await runtime.waitFor(() => gate.isClosed(firstId));
        assert(await requestRejected, "the closed owner's request must reject");
        await Promise.all([gate.release(), responseHold.release()]);
        const oldResponseHold = responseHold;
        await oldResponseHold.release();

        gate.arm(route);
        second = await connect(options);
        second.subscribe(route);
        const secondFlush = second.flush();
        let flushSettled = false;
        secondFlush.then(
          () => {
            flushSettled = true;
          },
          () => {
            flushSettled = true;
          },
        );
        const secondId = await Promise.race([
          gate.barrierHeld(),
          secondFlush.then(() => {
            throw new Error("the fresh readiness flush escaped its hold");
          }),
        ]);
        assert(
          secondId !== firstId,
          "the fresh hold needs a new physical owner",
        );
        responseHold = gate.armResponseHold(CONNZ, secondId);
        const secondRequest = second.request(CONNZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        let replySettled = false;
        secondRequest.then(
          () => {
            replySettled = true;
          },
          () => {
            replySettled = true;
          },
        );
        await Promise.race([
          responseHold.held,
          secondRequest.then(() => {
            throw new Error("the fresh reply escaped its hold");
          }),
        ]);
        await oldResponseHold.release();
        const varz = await second.request(VARZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        const serverId = assertServerReply(varz.data, VARZ);
        assertEquals(flushSettled, false);
        assertEquals(replySettled, false);
        await gate.release();
        await secondFlush;
        await responseHold.release();
        assertEquals(
          assertServerReply((await secondRequest).data, CONNZ),
          serverId,
        );
      } finally {
        await gate.release().catch(() => undefined);
        await responseHold.release().catch(() => undefined);
        await second?.close();
        await first.close();
      }
    }, { interruptibleNativeProxy: true });
  },
);

Deno.test(
  "the response barrier withholds only the targeted request's reply",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      // The system connection must traverse the intercepting native proxy, so
      // read its advertised URL from the generated control-plane config.
      const proxyUrl = runtime.nativeProxyUrl();
      assert(
        proxyUrl !== runtime.natsUrl,
        "the system connection must not bypass the native proxy",
      );

      const nc = await connect({
        servers: proxyUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            join(
              runtime.workdir,
              "config",
              "trellis",
              "nats",
              "creds",
              "system.creds",
            ),
          ),
        ),
      });
      const gate = runtime.nativeTransportGate();
      const barrier = gate.armResponseHold(CONNZ);
      try {
        // A second hold while one is active is misuse; the original stays armed.
        assertThrows(
          () => gate.armResponseHold(CONNZ),
          Error,
          "already armed",
        );

        const connz = nc.request(CONNZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        // Observe settlement from the instant the request is issued. An
        // already-settled promise runs a later-attached `.then` in a future
        // microtask, so reading a flag registered only after the awaits below
        // would always see `false`; registering here makes the post-VARZ check
        // report a real premature settlement.
        let connzSettled = false;
        connz.then(
          () => {
            connzSettled = true;
          },
          () => {
            connzSettled = true;
          },
        );
        // `held` resolves only once the real CONNZ reply frame is retained, so a
        // barrier failure surfaces here deterministically instead of as a hang:
        // if the reply had been forwarded rather than held, the request would
        // settle first.
        const held = await Promise.race([
          barrier.held,
          connz.then(() => {
            throw new Error(
              "the CONNZ reply was forwarded before the barrier held it",
            );
          }),
        ]);
        assertEquals(held.requestSubject, CONNZ);
        assert(
          held.connectionId > 0,
          "the barrier must attribute the request's physical connection",
        );

        // Unrelated real request on the same connection must still complete while
        // the CONNZ reply stays withheld.
        const varz = await nc.request(VARZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        const varzServerId = assertServerReply(varz.data, VARZ);

        assertEquals(
          connzSettled,
          false,
          "the withheld CONNZ reply must not have settled",
        );

        await barrier.release();
        const connzReply = await connz;
        assertEquals(
          assertServerReply(connzReply.data, CONNZ),
          varzServerId,
          "the released CONNZ reply must carry the same real server's data",
        );
      } finally {
        await barrier.release().catch(() => undefined);
        await nc.close();
      }
    }, { interruptibleNativeProxy: true });
  },
);

Deno.test(
  "the response barrier selects the withheld request by its reply body",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const proxyUrl = runtime.nativeProxyUrl();
      assert(
        proxyUrl !== runtime.natsUrl,
        "the system connection must not bypass the native proxy",
      );

      const nc = await connect({
        servers: proxyUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            join(
              runtime.workdir,
              "config",
              "trellis",
              "nats",
              "creds",
              "system.creds",
            ),
          ),
        ),
      });
      const gate = runtime.nativeTransportGate();
      // The two real requests share the prefix, so only the reply body decides
      // which one is retained. Both are published before either reply arrives,
      // exercising concurrent in-flight candidates.
      const barrier = gate.armResponseHold(
        SYS_REQ_SERVER,
        undefined,
        isConnzReply,
      );
      try {
        const connz = nc.request(CONNZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        const varz = nc.request(VARZ, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        // Register settlement before awaiting so a forwarded CONNZ reply is
        // observed rather than missed by a later-attached `.then`.
        let connzSettled = false;
        connz.then(
          () => {
            connzSettled = true;
          },
          () => {
            connzSettled = true;
          },
        );
        // `held` resolves only once the matching CONNZ reply frame is retained;
        // a selector that matched the VARZ reply instead would settle VARZ and
        // leave CONNZ to time out, so this race fails deterministically.
        const held = await Promise.race([
          barrier.held,
          connz.then(() => {
            throw new Error(
              "the CONNZ reply was forwarded before the barrier held it",
            );
          }),
        ]);
        assertEquals(held.requestSubject, CONNZ);
        assert(
          held.connectionId > 0,
          "the barrier must attribute the request's physical connection",
        );

        // The non-matching VARZ reply must have flowed through untouched even
        // though it shares the armed prefix and arrived concurrently.
        const varzReply = await varz;
        const varzServerId = assertServerReply(varzReply.data, VARZ);

        assertEquals(
          connzSettled,
          false,
          "the withheld CONNZ reply must not have settled",
        );

        await barrier.release();
        const connzReply = await connz;
        assertEquals(
          assertServerReply(connzReply.data, CONNZ),
          varzServerId,
          "the released CONNZ reply must carry the same real server's data",
        );
      } finally {
        await barrier.release().catch(() => undefined);
        await nc.close();
      }
    }, { interruptibleNativeProxy: true });
  },
);

Deno.test(
  "the response barrier matches the exact reply body behind real NATS headers",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const proxyUrl = runtime.nativeProxyUrl();
      assert(
        proxyUrl !== runtime.natsUrl,
        "the system connection must not bypass the native proxy",
      );

      const nc = await connect({
        servers: proxyUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            join(
              runtime.workdir,
              "config",
              "trellis",
              "nats",
              "creds",
              "system.creds",
            ),
          ),
        ),
      });
      // A unique subject space keeps this hold disjoint from the broker's own
      // `$SYS` traffic on the shared system account.
      const prefix = `hold.body.${crypto.randomUUID()}`;
      const targetSubject = `${prefix}.target`;
      const siblingSubject = `${prefix}.sibling`;
      // Binary, non-CRLF-terminated, multibyte bytes the parser must recover
      // byte-exactly rather than as a text line. The interior CR/LF proves the
      // header block is excluded by declared length, not by scanning for a
      // terminator.
      const binaryBody = new Uint8Array([
        0x00,
        0x0d,
        0x0a,
        0x7f,
        0xff,
        0xc3,
        0xa9,
        0xf0,
        0x9f,
        0x8e,
        0x89,
        0x0d,
        0x0a,
        0x41,
      ]);
      const siblingBody = new TextEncoder().encode("sibling reply body");
      const targetHeaders = natsHeaders();
      targetHeaders.set("x-hold-proof", "present");
      targetHeaders.set("x-hold-note", "héllo");
      // Real NATS core responders on the system account: the target answers an
      // HMSG carrying header bytes, the sibling answers a plain MSG.
      nc.subscribe(targetSubject, {
        callback: (_error, message) => {
          message.respond(binaryBody, { headers: targetHeaders });
        },
      });
      nc.subscribe(siblingSubject, {
        callback: (_error, message) => {
          message.respond(siblingBody);
        },
      });
      await nc.flush();

      const gate = runtime.nativeTransportGate();
      // Record every body offered to the predicate so the test can prove it saw
      // the exact framed payload with the HMSG header block and trailing CRLF
      // removed, rather than the whole frame or a decoded text line.
      const seenBodies = new Set<string>();
      const bodyKey = (bytes: Uint8Array) => Array.from(bytes).join(",");
      const binaryKey = bodyKey(binaryBody);
      const barrier = gate.armResponseHold(prefix, undefined, (body) => {
        seenBodies.add(bodyKey(body));
        return bodyKey(body) === binaryKey;
      });
      try {
        const target = nc.request(targetSubject, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        let targetSettled = false;
        target.then(
          () => {
            targetSettled = true;
          },
          () => {
            targetSettled = true;
          },
        );
        const held = await Promise.race([
          barrier.held,
          target.then(() => {
            throw new Error(
              "the target reply was forwarded before the barrier held it",
            );
          }),
        ]);
        assertEquals(held.requestSubject, targetSubject);
        assert(
          held.connectionId > 0,
          "the barrier must attribute the request's physical connection",
        );
        // The target is the only reply the predicate is offered before it
        // selects, and it accepts only the exact header-free body: had the
        // parser offered the HMSG header block plus body, no candidate would
        // ever be accepted and `held` would never resolve.
        assertEquals(seenBodies.size, 1);
        assert(
          seenBodies.has(binaryKey),
          "the predicate must see the exact binary body behind the HMSG headers",
        );

        // An unrelated real reply on the same connection and armed prefix must
        // flow untouched while the target reply stays withheld. `@nats-io/
        // transport-node` hands back a Node `Buffer`, so normalize both sides
        // to a plain `Uint8Array` before asserting byte equality.
        const siblingReply = await nc.request(
          siblingSubject,
          new Uint8Array(0),
          { timeout: REQUEST_TIMEOUT_MS },
        );
        assertEquals(new Uint8Array(siblingReply.data), siblingBody);
        assertEquals(
          targetSettled,
          false,
          "the withheld target reply must not have settled",
        );

        await barrier.release();
        const targetReply = await target;
        assertEquals(new Uint8Array(targetReply.data), binaryBody);
        assertEquals(targetReply.headers?.get("x-hold-proof"), "present");
        assertEquals(targetReply.headers?.get("x-hold-note"), "héllo");
      } finally {
        await barrier.release().catch(() => undefined);
        await nc.close();
      }
    }, { interruptibleNativeProxy: true });
  },
);

Deno.test(
  "a throwing reply predicate rejects that hold, forwards the reply, and never disarms a newer hold",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const proxyUrl = runtime.nativeProxyUrl();
      assert(
        proxyUrl !== runtime.natsUrl,
        "the system connection must not bypass the native proxy",
      );

      const nc = await connect({
        servers: proxyUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            join(
              runtime.workdir,
              "config",
              "trellis",
              "nats",
              "creds",
              "system.creds",
            ),
          ),
        ),
      });
      const prefix = `hold.throw.${crypto.randomUUID()}`;
      const targetSubject = `${prefix}.target`;
      const siblingSubject = `${prefix}.sibling`;
      const targetBody = new TextEncoder().encode("target reply body");
      const siblingBody = new TextEncoder().encode("sibling reply body");
      nc.subscribe(targetSubject, {
        callback: (_error, message) => {
          message.respond(targetBody);
        },
      });
      nc.subscribe(siblingSubject, {
        callback: (_error, message) => {
          message.respond(siblingBody);
        },
      });
      await nc.flush();

      const gate = runtime.nativeTransportGate();
      const predicateFailure = new Error("reply predicate rejected the body");
      const barrier = gate.armResponseHold(prefix, undefined, () => {
        throw predicateFailure;
      });
      try {
        // Both requests are in flight before any reply: whichever reply the
        // predicate throws on, both real replies must still flow unchanged.
        const target = nc.request(targetSubject, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        const sibling = nc.request(siblingSubject, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        const [targetReply, siblingReply] = await Promise.all([
          target,
          sibling,
        ]);
        assertEquals(new Uint8Array(targetReply.data), targetBody);
        assertEquals(new Uint8Array(siblingReply.data), siblingBody);

        // A real NATS round trip yields the macrotask queue, so a rejection that
        // arrived with no handler attached yet would surface here as an
        // unhandled rejection before the caller below reads `held`.
        await nc.flush();

        // The failure is reported through `held`, not swallowed as a non-match.
        // Reading it only after the replies settled deliberately exercises a
        // rejection that arrives before the caller awaits it.
        const rejected = await assertRejects(
          () => barrier.held,
          Error,
          predicateFailure.message,
        );
        assert(
          rejected === predicateFailure,
          "the hold must reject with the original predicate cause",
        );

        // The failed hold must be fully disarmed: a fresh hold can arm and
        // withhold, and releasing the failed hold must not release the fresh one.
        const freshSubject = `${prefix}.fresh`;
        const freshBody = new TextEncoder().encode("fresh held reply body");
        nc.subscribe(freshSubject, {
          callback: (_error, message) => {
            message.respond(freshBody);
          },
        });
        await nc.flush();
        let freshSeenBodyKey: string | undefined;
        const fresh = gate.armResponseHold(prefix, undefined, (body) => {
          freshSeenBodyKey = Array.from(body).join(",");
          return true;
        });
        const freshRequest = nc.request(freshSubject, new Uint8Array(0), {
          timeout: REQUEST_TIMEOUT_MS,
        });
        let freshSettled = false;
        freshRequest.then(
          () => {
            freshSettled = true;
          },
          () => {
            freshSettled = true;
          },
        );
        // Idempotent release of the failed hold must be a no-op for the fresh one.
        await barrier.release();
        const heldFresh = await Promise.race([
          fresh.held,
          freshRequest.then(() => {
            throw new Error(
              "releasing the failed hold forwarded the fresh hold's reply",
            );
          }),
        ]);
        assertEquals(heldFresh.requestSubject, freshSubject);
        assertEquals(freshSeenBodyKey, Array.from(freshBody).join(","));
        assertEquals(freshSettled, false);

        await fresh.release();
        const freshReply = await freshRequest;
        assertEquals(new Uint8Array(freshReply.data), freshBody);
      } finally {
        await barrier.release().catch(() => undefined);
        await nc.close();
      }
    }, { interruptibleNativeProxy: true });
  },
);
