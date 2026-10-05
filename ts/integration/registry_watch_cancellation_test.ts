/**
 * Real-boundary functional coverage for cancellation of the authorization
 * revocation watch.
 *
 * `AuthorizationRegistryReader.watchRevocation` creates a real JetStream push
 * consumer and push subscription before it is observable to the caller. This
 * drives it over the real Trellis native proxy against a real NATS server and a
 * temporary real KV bucket, withholding real `$JS.API.CONSUMER.INFO` server
 * replies that watch initialization waits on. The first hold is the info the
 * consumer lookup performs before the watch subscribes; it is released, and the
 * post-subscription info that initialization actually blocks on is then held.
 * Aborting there must settle the caller's setup promise before that reply is
 * released, and must release the watch's own push subscription, so the
 * broker's inventory no longer reports any subscription bound to this watch,
 * while unrelated traffic on the same physical connection keeps flowing.
 * Whether the unowned consumer is reaped or merely left unbound is the
 * broker's own business, because a participant holds no consumer-delete right.
 * A fresh watcher must still initialize and observe a real revocation key
 * update, and aborting an initialized watch must reject its next read and
 * release its subscription.
 *
 * The same real boundary covers `AuthorizationRegistryReader.open` itself: an
 * already-aborted signal must reject before any `$JS.API.INFO` discovery
 * request, and an abort while that discovery reply is withheld must reject the
 * open promise before the reply is released. `open` creates no consumer, so
 * neither path has anything to clean up; releasing the withheld reply must
 * leave a fresh reader and watch fully functional against a real KV update.
 */

import { jetstreamManager } from "@nats-io/jetstream";
import { Kvm } from "@nats-io/kv";
import { credsAuthenticator } from "@nats-io/nats-core";
import { connect } from "@nats-io/transport-node";
import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";

import { AuthorizationRegistryReader } from "../packages/trellis/auth/authorization/nats_registry.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

const STREAM_INFO = "$JS.API.STREAM.INFO.";
const CONSUMER_INFO = "$JS.API.CONSUMER.INFO.";
/** Ordinary request timeout the abort must beat, not extend. */
const REQUEST_TIMEOUT_MS = 2_000;
/** Upper bound on an abort settling at all; never used to widen the watch. */
const ABORT_TIMEOUT_MS = 1_500;
/** Upper bound on waiting for the broker to observe a subscription change. */
const INVENTORY_TIMEOUT_MS = 2_000;

/** Reject with `message` if `promise` has not settled within `ms`. */
async function withTimeout<T>(
  promise: Promise<T>,
  ms: number,
  message: string,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** A random 43-character base64url key accepted as a registry key. */
function registryKey(): string {
  return btoa(
    String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32))),
  ).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** One broker-visible watch consumer and whether it still has a subscription. */
type ObservedConsumer = { name: string; pushBound: boolean };

/**
 * The broker's own consumer inventory for one stream, requiring that every
 * entry is one of this watch's `TrellisAuth` consumers.
 */
async function watchConsumers(
  manager: Awaited<ReturnType<typeof jetstreamManager>>,
  stream: string,
): Promise<ObservedConsumer[]> {
  const consumers: ObservedConsumer[] = [];
  for await (const consumer of manager.consumers.list(stream)) {
    assert(
      consumer.name.startsWith("TrellisAuth"),
      `unexpected consumer on the test stream: ${consumer.name}`,
    );
    // The broker omits `push_bound` entirely when it is false.
    consumers.push({
      name: consumer.name,
      pushBound: consumer.push_bound === true,
    });
  }
  return consumers;
}

/**
 * Bounded wait for the broker's own consumer inventory to satisfy `predicate`.
 * The watch releases its push subscription locally, so the broker stops
 * reporting that consumer as bound; it may keep or reap the unowned consumer.
 */
async function waitForConsumers(
  manager: Awaited<ReturnType<typeof jetstreamManager>>,
  stream: string,
  predicate: (consumers: ObservedConsumer[]) => boolean,
): Promise<ObservedConsumer[]> {
  const deadline = Date.now() + INVENTORY_TIMEOUT_MS;
  let consumers = await watchConsumers(manager, stream);
  while (!predicate(consumers) && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 20));
    consumers = await watchConsumers(manager, stream);
  }
  return consumers;
}

/** Assert that `promise` rejects with the standard abort error. */
async function assertAborted(promise: Promise<unknown>, what: string) {
  let settled = false;
  await promise.then(
    () => {
      settled = true;
    },
    (error) => {
      assert(
        error instanceof Error && error.name === "AbortError",
        `${what} must reject with a standard AbortError, got ${String(error)}`,
      );
    },
  );
  assertEquals(settled, false, `${what} must not resolve`);
}

Deno.test(
  "an aborted revocation watch releases its subscription and leaves normal watches intact",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const proxyUrl = runtime.nativeProxyUrl();
      assert(
        proxyUrl !== runtime.natsUrl,
        "the reader must traverse the native proxy, not bypass it",
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
              "trellis-auth.creds",
            ),
          ),
        ),
      });
      try {
        const manager = await jetstreamManager(nc);
        const bucket = `watch-cancel-${
          crypto.randomUUID().replace(/-/g, "").slice(0, 16)
        }`;
        const stream = `KV_${bucket}`;
        const kv = await new Kvm(nc).create(bucket);
        const reader = await AuthorizationRegistryReader.open(
          nc,
          { contextBucket: bucket },
          "_INBOX.watchcancel",
        );
        const gate = runtime.nativeTransportGate();
        const contextDigest = registryKey();
        // A separate real bucket isolates the recovered open/watch consumers
        // from the original stream's consumer-inventory assertions.
        const recoveredBucket = `watch-open-${
          crypto.randomUUID().replace(/-/g, "").slice(0, 16)
        }`;
        const recoveredKv = await new Kvm(nc).create(recoveredBucket);
        const recoveredDigest = registryKey();

        // --- open(): initialization is itself preemptible ---

        // A pre-aborted signal must reject the open with the standard error and
        // issue no JetStream discovery request at all.
        const infoRequests = () =>
          gate.connections()
            .flatMap((connection) => connection.outboundContexts)
            .filter((entry) => entry.subject === "$JS.API.INFO").length;
        const infoBefore = infoRequests();
        const preAbortedOpen = new AbortController();
        preAbortedOpen.abort();
        await assertAborted(
          AuthorizationRegistryReader.open(
            nc,
            { contextBucket: bucket },
            "_INBOX.watchcancel.preopen",
            preAbortedOpen.signal,
          ),
          "the pre-aborted reader open",
        );
        // Give a wrongly-issued discovery request time to reach the wire, then
        // prove none did: a pre-aborted open performs no registry I/O.
        await new Promise((resolve) => setTimeout(resolve, 50));
        assertEquals(
          infoRequests(),
          infoBefore,
          "a pre-aborted open must not request $JS.API.INFO",
        );
        assertEquals(await watchConsumers(manager, stream), []);

        // Hold the account-info reply that reader initialization waits on, so
        // the open is provably blocked before it creates any consumer.
        const openInfo = gate.armResponseHold("$JS.API.INFO");
        const openAbort = new AbortController();
        const opened = AuthorizationRegistryReader.open(
          nc,
          { contextBucket: bucket },
          "_INBOX.watchcancel.open",
          openAbort.signal,
        );
        const openHeld = await withTimeout(
          openInfo.held,
          REQUEST_TIMEOUT_MS,
          "the reader open discovery reply was never withheld",
        );
        assertEquals(openHeld.requestSubject, "$JS.API.INFO");
        assertEquals(
          await watchConsumers(manager, stream),
          [],
          "an opening reader must not create a consumer",
        );

        // Unrelated real requests on the same physical connection still complete
        // while the discovery reply is withheld.
        const openStreamInfo = await nc.request(
          `${STREAM_INFO}${stream}`,
          new Uint8Array(0),
          { timeout: REQUEST_TIMEOUT_MS },
        );
        const openParsed = JSON.parse(
          new TextDecoder().decode(openStreamInfo.data),
        ) as { error?: unknown; config?: { name?: string } };
        assertEquals(openParsed.error, undefined);
        assertEquals(openParsed.config?.name, stream);

        // The abort must reject the open promise before the held reply is
        // released; no consumer exists because none was created.
        openAbort.abort();
        await withTimeout(
          assertAborted(opened, "the aborted reader open"),
          ABORT_TIMEOUT_MS,
          "the aborted reader open did not settle before its discovery reply was released",
        );
        await openInfo.release();

        // After the withheld discovery reply is released, a fresh reader opens
        // and a fresh watch initializes and observes a real revocation update.
        const recoveredReader = await withTimeout(
          AuthorizationRegistryReader.open(
            nc,
            { contextBucket: recoveredBucket },
            "_INBOX.watchcancel.recovered",
          ),
          REQUEST_TIMEOUT_MS,
          "the fresh reader did not open after the discovery reply was released",
        );
        const recovered = await recoveredReader.watchRevocation(
          recoveredDigest,
        );
        assertEquals(
          (await recovered.iterator.next()).value?.operation,
          "initialized",
          "the recovered watch must initialize normally",
        );
        await recoveredKv.put(
          `revocation.${recoveredDigest}`,
          new TextEncoder().encode(JSON.stringify({ revokedAt: 1_150 })),
        );
        const recoveredUpdate = await withTimeout(
          recovered.iterator.next(),
          REQUEST_TIMEOUT_MS,
          "the recovered watch did not observe the revocation update",
        );
        assertEquals(recoveredUpdate.value?.operation, "put");
        assertEquals(
          recoveredUpdate.value?.key,
          `revocation.${recoveredDigest}`,
        );
        await recovered.close();

        // A signal already aborted before the call must fail immediately with
        // the standard error and create no consumer at all.
        const preAborted = new AbortController();
        preAborted.abort();
        await assertAborted(
          reader.watchRevocation(contextDigest, preAborted.signal),
          "the pre-aborted watch",
        );
        assertEquals(await watchConsumers(manager, stream), []);

        // Hold the consumer-info reply that the consumer lookup performs before
        // the watch subscribes, so the watch is provably blocked mid-setup.
        const lookupInfo = gate.armResponseHold(`${CONSUMER_INFO}${stream}.`);
        const abort = new AbortController();
        const setup = reader.watchRevocation(contextDigest, abort.signal);
        const lookupHeld = await withTimeout(
          lookupInfo.held,
          REQUEST_TIMEOUT_MS,
          "the consumer lookup info reply was never withheld",
        );
        assert(
          lookupHeld.requestSubject.startsWith(`${CONSUMER_INFO}${stream}.`),
          `the hold must capture the consumer lookup request, got ${lookupHeld.requestSubject}`,
        );

        // Release that reply and, in the same synchronous step, hold the
        // post-subscription info reply that initialization actually blocks on.
        // The watch cannot issue it before the released reply is processed, so
        // the second hold is armed in time deterministically.
        const lookupRelease = lookupInfo.release();
        const barrier = gate.armResponseHold(`${CONSUMER_INFO}${stream}.`);
        await lookupRelease;
        const held = await withTimeout(
          barrier.held,
          REQUEST_TIMEOUT_MS,
          "the post-subscription info reply was never withheld",
        );
        assert(
          held.requestSubject.startsWith(`${CONSUMER_INFO}${stream}.`),
          `the hold must capture the post-subscription info request, got ${held.requestSubject}`,
        );

        // The provisional consumer is real, broker-visible, and bound to the
        // watch's own push subscription before the abort.
        const provisional = await watchConsumers(manager, stream);
        assertEquals(
          provisional.length,
          1,
          "the watch must have created its provisional consumer",
        );
        assertEquals(
          provisional[0].pushBound,
          true,
          "the provisional consumer must be bound to the watch subscription",
        );

        // Unrelated real requests on the same physical connection still complete.
        const streamInfo = await nc.request(
          `${STREAM_INFO}${stream}`,
          new Uint8Array(0),
          { timeout: REQUEST_TIMEOUT_MS },
        );
        const parsed = JSON.parse(
          new TextDecoder().decode(streamInfo.data),
        ) as {
          error?: unknown;
          config?: { name?: string };
        };
        assertEquals(parsed.error, undefined);
        assertEquals(parsed.config?.name, stream);

        // The abort must settle setup before the held reply is released, and
        // must release the watch's push subscription. The broker's complete
        // inventory must then contain no subscription bound to this watch.
        // Whether that consumer was reaped or merely left unbound is the
        // broker's concern, not the ownership contract under test.
        abort.abort();
        await withTimeout(
          assertAborted(setup, "the aborted watch setup"),
          ABORT_TIMEOUT_MS,
          "the aborted watch setup did not settle before its reply was released",
        );
        const afterAbort = await waitForConsumers(
          manager,
          stream,
          (consumers) => consumers.every((consumer) => !consumer.pushBound),
        );
        assertEquals(
          afterAbort.filter((consumer) => consumer.pushBound).length,
          0,
          "the aborted watch must leave no broker-bound push subscription",
        );
        await barrier.release();

        // A fresh watch is unaffected by the abandoned consumer: it initializes
        // and observes a real revocation key update written through the KV API.
        const freshAbort = new AbortController();
        const fresh = await reader.watchRevocation(
          contextDigest,
          freshAbort.signal,
        );
        assertEquals(
          (await fresh.iterator.next()).value?.operation,
          "initialized",
        );
        await kv.put(
          `revocation.${contextDigest}`,
          new TextEncoder().encode(JSON.stringify({ revokedAt: 1_150 })),
        );
        const update = await withTimeout(
          fresh.iterator.next(),
          REQUEST_TIMEOUT_MS,
          "the fresh watch did not observe the revocation update",
        );
        assertEquals(update.value?.operation, "put");
        assertEquals(update.value?.key, `revocation.${contextDigest}`);
        const bound = await waitForConsumers(
          manager,
          stream,
          (consumers) =>
            consumers.filter((consumer) => consumer.pushBound).length === 1,
        );
        assertEquals(
          bound.filter((consumer) => consumer.pushBound).length,
          1,
          "the fresh watch must own exactly one bound consumer",
        );

        // Aborting an initialized watch rejects the next read and releases its
        // subscription, leaving no broker-bound consumer.
        freshAbort.abort();
        await withTimeout(
          assertAborted(fresh.iterator.next(), "the aborted initialized watch"),
          ABORT_TIMEOUT_MS,
          "the aborted initialized watch did not reject its next read",
        );
        const drained = await waitForConsumers(
          manager,
          stream,
          (consumers) => consumers.every((consumer) => !consumer.pushBound),
        );
        assertEquals(
          drained.every((consumer) => !consumer.pushBound),
          true,
          "the aborted initialized watch must release its push subscription",
        );
      } finally {
        await nc.close();
      }
    }, { interruptibleNativeProxy: true });
  },
);
