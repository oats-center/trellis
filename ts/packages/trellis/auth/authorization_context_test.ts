import type {
  Msg,
  NatsConnection,
  Payload,
  Subscription,
} from "@nats-io/nats-core";
import { headers as natsHeaders } from "@nats-io/nats-core";
import { isErr } from "@oatscenter/result";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "npm:@opentelemetry/sdk-metrics@^2.7.0";
import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";

import vectors from "../../../../integration/fixtures/protocol/authorization-context/vectors.json" with {
  type: "json",
};
import type { PermissionAtom as DescriptorPermissionAtom } from "../participant_runtime/api.ts";
import { type VerifiedCaller, verifyLocalAuthorization } from "../session.ts";
import {
  AuthorizationProviderUnavailableError,
} from "./authorization/provider_cache.ts";
import { AuthorizationRegistryReader } from "./authorization/nats_registry.ts";
import {
  type AuthorizationContextBundle,
  AuthorizationContextCache,
  AuthorizationContextRefreshError,
  AuthorizationProviderCache,
  type AuthorizationProviderEvent,
  type AuthorizationProviderRequest,
  type AuthorizationRuntimeBinding,
  startAuthorizationContextRefresh,
} from "./authorization_context.ts";
import type { PermissionAtom } from "./protocol_wasm.ts";
import { createAuth } from "./session_auth.ts";
import { refreshAuthorizationContextWithMetadata } from "./authorization/refresh.ts";
import {
  base64urlDecode,
  base64urlEncode,
  canonicalizeJsonValue,
  sha256,
  utf8,
} from "./utils.ts";

Deno.test("user refresh binds a fresh runtime key under the installation proof", async () => {
  const installation = await createAuth({
    sessionKeySeed: base64urlEncode(new Uint8Array(32).fill(7)),
  });
  const runtime = await createAuth({
    sessionKeySeed: base64urlEncode(new Uint8Array(32).fill(8)),
  });
  let body: Record<string, unknown> | undefined;
  const cache = new AuthorizationContextCache("https://trellis.example");
  await assertRejects(() =>
    refreshAuthorizationContextWithMetadata({
      trellisUrl: "https://trellis.example",
      sessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
      auth: installation,
      sessionKey: runtime.sessionKey,
      cache,
      fetch: async (_input, init) => {
        body = JSON.parse(String(init?.body));
        return new Response(JSON.stringify({ code: "login_not_found" }), {
          status: 401,
          headers: { "content-type": "application/json" },
        });
      },
    })
  );
  assertEquals(body?.sessionKey, runtime.sessionKey);
  assert(body?.sessionKey !== installation.sessionKey);
  assert(typeof body?.proof === "object");
});

const chain = vectors.completeChain;
const policy = vectors.defaults.policy;

function bundle(): AuthorizationContextBundle {
  return {
    context: JSON.parse(chain.contextCanonicalJson),
    issuer: {
      keyId: chain.issuerKeyId,
      publicKey: chain.issuerPublicKey,
      state: "active",
    },
    authorizationRegistry: { contextBucket: "contexts" },
    policy: {
      allowedClockSkewSeconds: policy.allowedClockSkewSeconds,
      maximumContextLifetimeSeconds: policy.maximumContextLifetimeSeconds,
      maximumContextBytes: policy.maximumContextBytes,
      maximumPermissions: policy.maximumPermissions,
      refreshLeadSeconds: 60,
      refreshJitterSeconds: 0,
    },
  };
}

function runtimeBinding(): AuthorizationRuntimeBinding {
  const context = JSON.parse(chain.contextCanonicalJson);
  return {
    connectionId: context.connectionId,
    loginSessionId: context.loginSessionId,
    participantId: context.participantId,
    inboxPrefix: context.inboxPrefix,
    transports: { native: { natsServers: ["nats://127.0.0.1:4222"] } },
  };
}

function cache(fetch: typeof globalThis.fetch = globalThis.fetch) {
  return new AuthorizationContextCache(
    "https://trellis.test",
    fetch,
    () => policy.nowUnixSeconds * 1_000,
  );
}

async function installedCache(fetch?: typeof globalThis.fetch) {
  const value = cache(fetch);
  await value.install(
    bundle(),
    { bootstrapJwt: "route", bootstrapJwtExpiresAt: 2_000 },
    policy.nowUnixSeconds,
    undefined,
    runtimeBinding(),
  );
  return value;
}

Deno.test("authorization refresh can use native bootstrap", async () => {
  const value = await installedCache();
  const auth = await createAuth({ sessionKeySeed: chain.sessionSeed });
  let refreshed!: () => void;
  const didRefresh = new Promise<void>((resolve) => refreshed = resolve);
  const stop = startAuthorizationContextRefresh({
    trellisUrl: "https://trellis.test",
    sessionId: value.current().context.connectionId,
    auth,
    cache: value,
    refresh: async (shouldInstall) => {
      assert(shouldInstall());
      refreshed();
      return value.current();
    },
  });

  value.requestRefresh();
  await didRefresh;
  stop();
  assert(
    new AuthorizationContextRefreshError(401, "identity_not_found").terminal,
  );
  assert(
    new AuthorizationContextRefreshError(401, "identity_inactive").terminal,
  );
});

Deno.test("changed authorization refresh reconnects once while connected", async () => {
  const value = await installedCache();
  const auth = await createAuth({ sessionKeySeed: chain.sessionSeed });
  let reconnects = 0;
  let reconnected!: () => void;
  const didReconnect = new Promise<void>((resolve) => reconnected = resolve);
  const stop = startAuthorizationContextRefresh({
    trellisUrl: "https://trellis.test",
    sessionId: value.current().context.connectionId,
    auth,
    cache: value,
    refresh: () =>
      Promise.resolve({
        ...value.current(),
        contextDigest: "changed-context",
      }),
    onRefresh: () => {
      reconnects += 1;
      reconnected();
    },
  });

  value.requestRefresh();
  await didReconnect;
  stop();
  assertEquals(reconnects, 1);
});

function permission(): PermissionAtom {
  return vectors.defaults.permission as PermissionAtom;
}

function request(): AuthorizationProviderRequest {
  return {
    contextDigest: chain.contextDigest,
    sessionKey: JSON.parse(chain.contextCanonicalJson).sessionKey,
    subject: vectors.defaults.request.subject,
    reply: vectors.defaults.request.reply,
    payload: utf8(vectors.defaults.request.payload),
    iat: vectors.defaults.request.iat,
    requestId: vectors.defaults.request.requestId,
    proof: chain.requestProof,
    requiredPermissions: [permission()],
    requiredCapabilities: [],
  };
}

function event(): AuthorizationProviderEvent {
  return {
    contextDigest: chain.contextDigest,
    sessionKey: JSON.parse(chain.contextCanonicalJson).sessionKey,
    descriptorIdentity: vectors.defaults.event.descriptorIdentity,
    subject: vectors.defaults.event.subject,
    payload: utf8(vectors.defaults.event.payload),
    eventId: vectors.defaults.event.eventId,
    eventTime: vectors.defaults.event.eventTime,
    proof: chain.eventProof,
    requiredCapabilities: [],
  };
}

async function signedContext(
  connectionId: string,
  issuerSeed = chain.issuerSeed,
): Promise<[string, string]> {
  const context = JSON.parse(chain.contextCanonicalJson) as Record<
    string,
    unknown
  >;
  delete context.signature;
  context.connectionId = connectionId;
  const issuer = await createAuth({ sessionKeySeed: issuerSeed });
  context.issuerKeyId = base64urlEncode(
    await sha256(base64urlDecode(issuer.sessionKey)),
  );
  const domain = utf8("trellis.authorization-context.v1");
  const canonical = utf8(canonicalizeJsonValue(context));
  const input = new Uint8Array(8 + domain.length + canonical.length);
  const view = new DataView(input.buffer);
  view.setUint32(0, domain.length);
  input.set(domain, 4);
  view.setUint32(4 + domain.length, canonical.length);
  input.set(canonical, 8 + domain.length);
  const signed = {
    ...context,
    signature: base64urlEncode(await issuer.sign(await sha256(input))),
  };
  const json = canonicalizeJsonValue(signed);
  return [base64urlEncode(await sha256(utf8(json))), json];
}

type Registry = {
  contexts: Map<string, string>;
  revocations?: Map<string, number>;
  deletedRevocations?: Set<string>;
  revocationOperations?: Map<string, string>;
  reads: string[];
  contextReadBarrier?: Promise<void>;
  watchUnavailable?: boolean;
  flushUnavailable?: boolean;
  watchCloses?: number;
  watchClosed?: Promise<void>;
  putWatches?: (revokedAt: number) => void;
  heartbeatWatches?: (lastConsumerSequence: number) => void;
  deleteWatches?: () => void;
  closeWatches?: () => void;
};

function providerNats(registry: Registry): NatsConnection {
  type TestStatus = ReturnType<NatsConnection["status"]> extends
    AsyncIterable<infer T> ? T : never;
  type TestSubscription = Subscription & { deliver(message: Msg): void };
  type TestConsumer = {
    stream: string;
    name: string;
    config: Record<string, unknown>;
    pending: Array<{
      key: string;
      value: Uint8Array;
      revision: number;
      removed?: boolean;
      operation?: string;
    }>;
    delivered: number;
  };
  const consumers = new Map<string, TestConsumer>();
  const subscriptions = new Map<string, TestSubscription>();
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  let revision = 0;

  const record = (key: string) => {
    const digest = key.startsWith("revocation.")
      ? key.slice("revocation.".length)
      : key;
    const value = key.startsWith("revocation.")
      ? registry.revocations?.get(digest) === undefined
        ? undefined
        : JSON.stringify({ revokedAt: registry.revocations?.get(digest) })
      : registry.contexts.get(digest);
    if (
      key.startsWith("revocation.") && registry.deletedRevocations?.has(digest)
    ) {
      return {
        key,
        value: new Uint8Array(),
        revision: ++revision,
        removed: true,
      };
    }
    const operation = registry.revocationOperations?.get(digest);
    return value === undefined && operation === undefined ? undefined : {
      key,
      value: encoder.encode(value ?? ""),
      revision: ++revision,
      ...(operation === undefined ? {} : { operation }),
    };
  };
  const response = (value: unknown): Msg => {
    const data = encoder.encode(JSON.stringify(value));
    return {
      subject: "_INBOX.response",
      sid: 1,
      data,
      headers: natsHeaders(),
      respond: () => true,
      json: <T>() => value as T,
      string: () => decoder.decode(data),
    };
  };
  const noMessage = () =>
    response({
      error: { code: 404, err_code: 10037, description: "no messages" },
    });
  const message = (
    consumer: TestConsumer,
    item: {
      key: string;
      value: Uint8Array;
      revision: number;
      removed?: boolean;
      operation?: string;
    },
  ): Msg => {
    const headers = natsHeaders();
    if (item.operation !== undefined) {
      headers.set("KV-Operation", item.operation);
    } else if (item.removed) headers.set("KV-Operation", "DEL");
    return {
      subject: `$KV.contexts.${item.key}`,
      sid: 1,
      data: item.value,
      reply:
        `$JS.ACK._.account.${consumer.stream}.${consumer.name}.1.${item.revision}.${consumer.delivered}.1.0`,
      headers,
      respond: () => true,
      json: <T>() => JSON.parse(decoder.decode(item.value)) as T,
      string: () => decoder.decode(item.value),
    };
  };
  const status = () => {
    let done = false;
    let wake: (() => void) | undefined;
    const iterator = {
      next: async (): Promise<IteratorResult<TestStatus>> => {
        if (done) return { done: true, value: undefined as never };
        await new Promise<void>((resolve) => wake = resolve);
        return { done: true, value: undefined as never };
      },
      return: async (): Promise<IteratorResult<TestStatus>> => {
        done = true;
        wake?.();
        return { done: true, value: undefined as never };
      },
      [Symbol.asyncIterator]() {
        return iterator;
      },
      stop() {
        void iterator.return();
      },
    };
    return iterator as ReturnType<NatsConnection["status"]>;
  };

  registry.closeWatches = () => {
    for (const subscription of subscriptions.values()) {
      subscription.unsubscribe();
    }
  };

  return {
    info: undefined,
    options: { inboxPrefix: "_INBOX.test" },
    closed: () => Promise.resolve(undefined),
    close: () => Promise.resolve(),
    publish: () => {},
    publishMessage: () => {},
    respondMessage: () => true,
    subscribe: (
      subject: string,
      options?: { callback?: (error: Error | null, message: Msg) => void },
    ) => {
      if (
        registry.watchUnavailable &&
        subject.startsWith("$KV.contexts.revocation.")
      ) {
        throw new Error("watch unavailable");
      }
      let closed = false;
      const queued: Msg[] = [];
      let wake = () => {};
      let resolveClosed = () => {};
      const closedPromise = new Promise<void>((resolve) =>
        resolveClosed = resolve
      );
      const watchConsumer = [...consumers.values()].find((consumer) =>
        consumer.config.deliver_subject === subject
      );
      const revocationWatch = watchConsumer !== undefined;
      if (revocationWatch) {
        registry.watchClosed = closedPromise;
        registry.putWatches = (revokedAt) => {
          watchConsumer.delivered += 1;
          const filter = watchConsumer.config.filter_subject ??
            (watchConsumer.config.filter_subjects as string[] | undefined)
              ?.[0] ??
            "";
          subscription.deliver(message(watchConsumer, {
            key: String(filter).replace("$KV.contexts.", ""),
            value: encoder.encode(JSON.stringify({ revokedAt })),
            revision: ++revision,
          }));
        };
        registry.heartbeatWatches = (lastConsumerSequence) => {
          const headers = natsHeaders(100, "Idle Heartbeat");
          headers.set("Nats-Last-Consumer", String(lastConsumerSequence));
          headers.set("Nats-Last-Stream", String(revision));
          subscription.deliver({
            ...response({}),
            subject,
            data: new Uint8Array(),
            headers,
          });
        };
        registry.heartbeatWatches = (lastConsumerSequence) => {
          const headers = natsHeaders(100, "Idle Heartbeat");
          headers.set("Nats-Last-Consumer", String(lastConsumerSequence));
          headers.set("Nats-Last-Stream", String(revision));
          subscription.deliver({
            ...response({}),
            subject,
            data: new Uint8Array(),
            headers,
          });
        };
        registry.deleteWatches = () => {
          watchConsumer.delivered += 1;
          const headers = natsHeaders();
          headers.set("KV-Operation", "DEL");
          const filter = watchConsumer.config.filter_subject ??
            (watchConsumer.config.filter_subjects as string[] | undefined)
              ?.[0] ??
            "";
          const item = {
            key: String(filter).replace(
              "$KV.contexts.",
              "",
            ),
            value: new Uint8Array(),
            revision: ++revision,
          };
          subscription.deliver({ ...message(watchConsumer, item), headers });
        };
      }
      const close = () => {
        if (closed) return;
        closed = true;
        if (revocationWatch) {
          registry.watchCloses = (registry.watchCloses ?? 0) + 1;
        }
        wake();
        resolveClosed();
      };
      const subscription: TestSubscription = {
        closed: closedPromise,
        unsubscribe: close,
        drain: () => {
          close();
          return Promise.resolve();
        },
        [Symbol.asyncDispose]: () => {
          close();
          return Promise.resolve();
        },
        isDraining: () => false,
        isClosed: () => closed,
        callback: options?.callback ?? (() => {}),
        getSubject: () => subject,
        getReceived: () => 0,
        getProcessed: () => 0,
        getPending: () => 0,
        getID: () => 1,
        getMax: () => undefined,
        [Symbol.asyncIterator]: () => {
          const iterator: AsyncIterableIterator<Msg> = {
            next: async () => {
              while (!closed && queued.length === 0) {
                await new Promise<void>((resolve) => wake = resolve);
              }
              const value = queued.shift();
              return value === undefined
                ? { done: true, value: undefined }
                : { done: false, value };
            },
            return: async () => {
              close();
              return { done: true, value: undefined };
            },
            [Symbol.asyncIterator]: () => iterator,
          };
          return iterator;
        },
        deliver: (value) => options?.callback?.(null, value),
      };
      subscriptions.set(subject, subscription);
      queueMicrotask(() => {
        for (const consumer of consumers.values()) {
          if (consumer.config.deliver_subject !== subject) continue;
          for (const item of consumer.pending.splice(0)) {
            consumer.delivered += 1;
            subscription.deliver(message(consumer, item));
          }
        }
      });
      return subscription;
    },
    request: async (subject: string, payload?: Payload): Promise<Msg> => {
      if (subject === "$JS.API.INFO") return response({ type: "account_info" });
      if (subject.startsWith("$JS.API.DIRECT.GET.")) {
        const marker = subject.indexOf(".$KV.contexts.");
        const key = marker < 0
          ? ""
          : subject.slice(marker + ".$KV.contexts.".length);
        registry.reads.push(key);
        if (!key.startsWith("revocation.")) {
          await registry.contextReadBarrier;
        }
        const deletedDigest = key.startsWith("revocation.")
          ? key.slice("revocation.".length)
          : undefined;
        if (deletedDigest && registry.deletedRevocations?.has(deletedDigest)) {
          const headers = natsHeaders();
          headers.set("Nats-Stream", "KV_contexts");
          headers.set("Nats-Sequence", String(++revision));
          headers.set("Nats-Time-Stamp", new Date(0).toISOString());
          headers.set("Nats-Subject", `$KV.contexts.${key}`);
          headers.set("KV-Operation", "DEL");
          return { ...response({}), data: new Uint8Array(), headers };
        }
        const item = record(key);
        if (!item) {
          return { ...response({}), headers: natsHeaders(404, "No Messages") };
        }
        const headers = natsHeaders();
        headers.set("Nats-Stream", "KV_contexts");
        headers.set("Nats-Sequence", String(item.revision));
        headers.set("Nats-Time-Stamp", new Date(0).toISOString());
        headers.set("Nats-Subject", `$KV.contexts.${key}`);
        return { ...response({}), data: item.value, headers };
      }
      if (subject.startsWith("$JS.API.STREAM.MSG.GET.")) {
        const body = JSON.parse(decoder.decode(payload as Uint8Array)) as {
          last_by_subj?: string;
        };
        const key = body.last_by_subj?.replace("$KV.contexts.", "") ?? "";
        registry.reads.push(key);
        const item = record(key);
        return item
          ? response({
            message: {
              subject: `$KV.contexts.${key}`,
              seq: item.revision,
              time: new Date(0).toISOString(),
              data: btoa(String.fromCharCode(...item.value)),
            },
          })
          : noMessage();
      }
      if (subject.startsWith("$JS.API.CONSUMER.CREATE.")) {
        if (registry.watchUnavailable) {
          throw new Error("revocation registry unavailable");
        }
        const body = JSON.parse(decoder.decode(payload as Uint8Array)) as {
          config: Record<string, unknown>;
        };
        const stream = subject.slice("$JS.API.CONSUMER.CREATE.".length).split(
          ".",
        )[0] ?? "";
        const name = String(body.config.name ?? `consumer-${consumers.size}`);
        const filter = body.config.filter_subject ??
          (body.config.filter_subjects as string[] | undefined)?.[0] ?? "";
        const key = String(filter).replace(
          "$KV.contexts.",
          "",
        );
        const pending = record(key);
        consumers.set(`${stream}:${name}`, {
          stream,
          name,
          config: body.config,
          pending: pending ? [pending] : [],
          delivered: 0,
        });
        return response({
          stream_name: stream,
          name,
          config: body.config,
          num_pending: pending ? 1 : 0,
        });
      }
      if (subject.startsWith("$JS.API.CONSUMER.INFO.")) {
        const [stream, name] = subject.slice(
          "$JS.API.CONSUMER.INFO.".length,
        ).split(".", 2);
        const consumer = consumers.get(`${stream}:${name}`);
        return consumer
          ? response({
            stream_name: stream,
            name,
            config: consumer.config,
            delivered: {
              consumer_seq: consumer.delivered,
              stream_seq: revision,
            },
            num_pending: consumer.pending.length,
          })
          : noMessage();
      }
      return response({});
    },
    requestMany: () => Promise.resolve((async function* () {})()),
    flush: () =>
      registry.flushUnavailable
        ? Promise.reject(new Error("flush unavailable"))
        : Promise.resolve(),
    drain: () => Promise.resolve(),
    isClosed: () => false,
    isDraining: () => false,
    getServer: () => "nats://127.0.0.1:4222",
    getServerVersion: () => "2.10.0",
    status,
    stats: () => ({ inBytes: 0, outBytes: 0, inMsgs: 0, outMsgs: 0 }),
    rtt: () => Promise.resolve(0),
    reconnect: () => Promise.resolve(),
    setServers: () => {},
    getServers: () => [],
    features: { get: () => ({ min: "2.10.0", ok: true }) },
    _resub: () => {},
    [Symbol.asyncDispose]: () => Promise.resolve(),
  } as NatsConnection;
}

async function provider(
  registry: Registry,
  issuerFetch: typeof globalThis.fetch = () => {
    throw new Error("unexpected issuer fetch");
  },
  installed?: AuthorizationContextCache,
  now: () => number = () => policy.nowUnixSeconds,
) {
  const cache = installed ?? await installedCache(issuerFetch);
  const value = await AuthorizationProviderCache.attach(
    providerNats(registry),
    cache.bundle().authorizationRegistry,
    "_INBOX.test",
    cache,
    { now },
  );
  value.start();
  await value.waitReady();
  return value;
}

Deno.test("online issuer context verification rejects signed-content tampering", async () => {
  const value = cache();
  const verified = await value.install(
    bundle(),
    { bootstrapJwt: "route", bootstrapJwtExpiresAt: 2_000 },
    policy.nowUnixSeconds,
  );
  assertEquals(verified.contextDigest, chain.contextDigest);
  assertEquals(verified.context.connectionId, "01JY0000000000000000000001");

  const tampered = bundle();
  (tampered.context as { principalId: string }).principalId = "tampered";
  await assertRejects(() =>
    cache().install(
      tampered,
      { bootstrapJwt: "route", bootstrapJwtExpiresAt: 2_000 },
      policy.nowUnixSeconds,
    )
  );
});

Deno.test("authorization context cache installs, binds runtime, and clears in memory", async () => {
  const value = await installedCache();
  assertEquals(
    value.current(policy.nowUnixSeconds).contextDigest,
    chain.contextDigest,
  );
  assertEquals(value.runtimeBinding(), runtimeBinding());
  assertEquals(value.routingJwt(), "route");

  await value.clear();
  assertThrows(() => value.current(policy.nowUnixSeconds));
  assertThrows(() => value.bundle());
  assertEquals(value.runtimeBinding(), runtimeBinding());
});

Deno.test("authorization context installs additional state in the same commit", async () => {
  const value = cache();
  let additionalInstalled = false;
  await value.install(
    bundle(),
    { bootstrapJwt: "route", bootstrapJwtExpiresAt: 2_000 },
    policy.nowUnixSeconds,
    undefined,
    runtimeBinding(),
    (verified) => {
      assertThrows(() => value.current(policy.nowUnixSeconds));
      assertEquals(verified.contextDigest, chain.contextDigest);
      additionalInstalled = true;
    },
  );
  assertEquals(additionalInstalled, true);
  assertEquals(
    value.current(policy.nowUnixSeconds).contextDigest,
    chain.contextDigest,
  );
});

Deno.test("provider resolves a cold context once and reuses the verified hot entry", async () => {
  const registry: Registry = {
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  };
  const value = await provider(registry);
  try {
    await value.resolveContext(chain.contextDigest);
    const cold = value.ioCounters();
    await value.resolveContext(chain.contextDigest);
    assertEquals(cold.contextGets, 1);
    assertEquals(cold.contextVerifications, 1);
    assertEquals(value.ioCounters(), cold);
  } finally {
    value.stop();
  }
  await registry.watchClosed;
  assertEquals(registry.watchCloses, 1);
});

Deno.test("provider reconnect resolves a fresh context and revocation watch", async () => {
  const registry: Registry = {
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  };
  const value = await provider(registry);
  try {
    await value.resolveContext(chain.contextDigest);
    value.observeConnectionPhase("disconnected");
    value.observeConnectionPhase("connected");
    await value.resolveContext(chain.contextDigest);
    assertEquals(value.ioCounters().contextGets, 2);
  } finally {
    value.stop();
  }
});

Deno.test("provider coalesces one pending context resolution", async () => {
  const registry: Registry = {
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  };
  const value = await provider(registry);
  try {
    await Promise.all(
      Array.from(
        { length: 16 },
        () => value.resolveContext(chain.contextDigest),
      ),
    );
    assertEquals(value.ioCounters().contextGets, 1);
    assertEquals(value.ioCounters().contextVerifications, 1);
  } finally {
    value.stop();
  }
});

Deno.test("provider discards a pending resolution from an old connection generation", async () => {
  let release = () => {};
  const registry: Registry = {
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
    contextReadBarrier: new Promise<void>((resolve) => release = resolve),
  };
  const value = await provider(registry);
  try {
    const stale = value.resolveContext(chain.contextDigest);
    while (!registry.reads.includes(chain.contextDigest)) {
      await Promise.resolve();
    }
    value.observeConnectionPhase("disconnected");
    value.observeConnectionPhase("connected");
    release();
    await assertRejects(
      () => stale,
      AuthorizationProviderUnavailableError,
    );
    registry.contextReadBarrier = undefined;
    await value.resolveContext(chain.contextDigest);
    assertEquals(value.ioCounters().contextGets, 2);
  } finally {
    value.stop();
  }
});

Deno.test("provider rejects a registry key whose signed context has another digest", async () => {
  const [otherDigest, otherContext] = await signedContext("other-connection");
  assert(otherDigest !== chain.contextDigest);
  const value = await provider({
    contexts: new Map([[chain.contextDigest, otherContext]]),
    reads: [],
  });
  try {
    const error = await value.verifyRequest(request());
    assert(!error.ok);
    assertEquals(error.error.code, "InvalidInput");
  } finally {
    value.stop();
  }
});

Deno.test("provider treats a missing context registry entry as unavailable", async () => {
  const registry: Registry = { contexts: new Map(), reads: [] };
  const value = await provider(registry);
  try {
    for (let attempt = 0; attempt < 3; attempt += 1) {
      await assertRejects(
        () => value.resolveContext(chain.contextDigest),
        AuthorizationProviderUnavailableError,
      );
    }
    assertEquals(registry.watchCloses, 3);
  } finally {
    value.stop();
  }
});

Deno.test("provider rejects a malformed context digest before registry I/O", async () => {
  const registry: Registry = { contexts: new Map(), reads: [] };
  const value = await provider(registry);
  try {
    await assertRejects(
      () => value.resolveContext("short"),
      Error,
      "authorization context digest is invalid",
    );
    assertEquals(registry.reads, []);
  } finally {
    value.stop();
  }
});

Deno.test("provider fails unavailable after watch loss and resynchronizes", async () => {
  const registry: Registry = {
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  };
  const value = await provider(registry);
  try {
    await value.resolveContext(chain.contextDigest);
    registry.watchUnavailable = true;
    registry.closeWatches?.();
    let failure: unknown;
    for (let attempt = 0; attempt < 100 && !failure; attempt += 1) {
      try {
        await value.resolveContext(chain.contextDigest);
        await Promise.resolve();
      } catch (error) {
        failure = error;
      }
    }
    assert(failure instanceof AuthorizationProviderUnavailableError);
    registry.watchUnavailable = false;
    await value.resolveContext(chain.contextDigest);
    assertEquals(value.ioCounters().contextGets, 2);
  } finally {
    value.stop();
  }
});

Deno.test("provider invalidates coverage when revocation evidence disappears", async () => {
  const registry: Registry = {
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  };
  const value = await provider(registry);
  try {
    await value.resolveContext(chain.contextDigest);
    registry.watchUnavailable = true;
    registry.deleteWatches?.();
    let failure: unknown;
    for (let attempt = 0; attempt < 100 && !failure; attempt += 1) {
      try {
        await value.resolveContext(chain.contextDigest);
        await Promise.resolve();
      } catch (error) {
        failure = error;
      }
    }
    assert(failure instanceof AuthorizationProviderUnavailableError);
  } finally {
    value.stop();
  }
});

Deno.test("provider observes revocation after watch initialization", async () => {
  const registry: Registry = {
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  };
  const value = await provider(registry);
  try {
    await value.resolveContext(chain.contextDigest);
    registry.putWatches?.(1_150);
    let result = await value.verifyRequest(request());
    for (let attempt = 0; result.ok && attempt < 100; attempt += 1) {
      await Promise.resolve();
      result = await value.verifyRequest(request());
    }
    assert(!result.ok);
    assertEquals(result.error.code, "PermissionDenied");
  } finally {
    value.stop();
  }
});

Deno.test("revocation watch accepts consecutive ordered updates", async () => {
  const registry: Registry = { contexts: new Map(), reads: [] };
  const reader = await AuthorizationRegistryReader.open(
    providerNats(registry),
    { contextBucket: "contexts" },
    "_INBOX.test",
  );
  const watch = await reader.watchRevocation(chain.contextDigest);
  try {
    assertEquals((await watch.iterator.next()).value?.operation, "initialized");
    registry.putWatches?.(1_150);
    assertEquals((await watch.iterator.next()).value?.operation, "put");
    registry.putWatches?.(1_151);
    assertEquals((await watch.iterator.next()).value?.operation, "put");
  } finally {
    watch.close();
  }
});

Deno.test("revocation watch rejects heartbeat progress without received data", async () => {
  const registry: Registry = { contexts: new Map(), reads: [] };
  const reader = await AuthorizationRegistryReader.open(
    providerNats(registry),
    { contextBucket: "contexts" },
    "_INBOX.test",
  );
  const watch = await reader.watchRevocation(chain.contextDigest);
  try {
    assertEquals((await watch.iterator.next()).value?.operation, "initialized");
    registry.heartbeatWatches?.(1);
    await assertRejects(
      () => watch.iterator.next(),
      Error,
      "sequence gap",
    );
  } finally {
    await watch.close();
  }
});

Deno.test("revocation watch accepts heartbeat progress for received queued data", async () => {
  const registry: Registry = { contexts: new Map(), reads: [] };
  const reader = await AuthorizationRegistryReader.open(
    providerNats(registry),
    { contextBucket: "contexts" },
    "_INBOX.test",
  );
  const watch = await reader.watchRevocation(chain.contextDigest);
  try {
    assertEquals((await watch.iterator.next()).value?.operation, "initialized");
    registry.putWatches?.(1_150);
    registry.heartbeatWatches?.(1);
    registry.putWatches?.(1_151);
    assertEquals((await watch.iterator.next()).value?.operation, "put");
    assertEquals((await watch.iterator.next()).value?.operation, "put");
  } finally {
    await watch.close();
  }
});

Deno.test("revocation watch cleans up when setup fails after consumption", async () => {
  const registry: Registry = {
    contexts: new Map(),
    reads: [],
    flushUnavailable: true,
  };
  const reader = await AuthorizationRegistryReader.open(
    providerNats(registry),
    { contextBucket: "contexts" },
    "_INBOX.test",
  );
  await assertRejects(
    () => reader.watchRevocation(chain.contextDigest),
    Error,
    "flush unavailable",
  );
  await registry.watchClosed;
  assertEquals(registry.watchCloses, 1);
});

Deno.test("provider treats an existing revocation tombstone as unavailable", async () => {
  const value = await provider({
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    deletedRevocations: new Set([chain.contextDigest]),
    reads: [],
  });
  try {
    await assertRejects(
      () => value.resolveContext(chain.contextDigest),
      AuthorizationProviderUnavailableError,
    );
  } finally {
    value.stop();
  }
});

Deno.test("provider treats an invalid revocation operation as unavailable", async () => {
  const value = await provider({
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    revocationOperations: new Map([[chain.contextDigest, "UNKNOWN"]]),
    reads: [],
  });
  try {
    await assertRejects(
      () => value.resolveContext(chain.contextDigest),
      AuthorizationProviderUnavailableError,
    );
  } finally {
    value.stop();
  }
});

Deno.test("provider treats a non-positive revocation time as unavailable", async () => {
  const value = await provider({
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    revocations: new Map([[chain.contextDigest, 0]]),
    reads: [],
  });
  try {
    await assertRejects(
      () => value.resolveContext(chain.contextDigest),
      AuthorizationProviderUnavailableError,
    );
  } finally {
    value.stop();
  }
});

Deno.test("provider retries a temporary issuer outage without poisoning the digest", async () => {
  const issuerSeed = base64urlEncode(new Uint8Array(32).fill(3));
  const issuerAuth = await createAuth({ sessionKeySeed: issuerSeed });
  const issuerKeyId = base64urlEncode(
    await sha256(base64urlDecode(issuerAuth.sessionKey)),
  );
  const [contextDigest, context] = await signedContext(
    "foreign-issuer",
    issuerSeed,
  );
  let fetches = 0;
  const registry: Registry = {
    contexts: new Map([[contextDigest, context]]),
    reads: [],
  };
  const value = await provider(
    registry,
    () => {
      fetches += 1;
      if (fetches === 1) return Promise.reject(new Error("issuer offline"));
      if (fetches === 2) {
        return Promise.resolve(new Response(null, { status: 404 }));
      }
      if (fetches === 3) return Promise.resolve(new Response("{"));
      if (fetches === 4) {
        return Promise.resolve(Response.json({
          keyId: "A".repeat(43),
          publicKey: issuerAuth.sessionKey,
          state: "active",
        }));
      }
      return Promise.resolve(Response.json({
        keyId: issuerKeyId,
        publicKey: issuerAuth.sessionKey,
        state: "active",
      }));
    },
  );
  try {
    await assertRejects(
      () => value.resolveContext(contextDigest),
      AuthorizationProviderUnavailableError,
    );
    await assertRejects(
      () => value.resolveContext(contextDigest),
      AuthorizationProviderUnavailableError,
    );
    await assertRejects(
      () => value.resolveContext(contextDigest),
      AuthorizationProviderUnavailableError,
    );
    await assertRejects(
      () => value.resolveContext(contextDigest),
      AuthorizationProviderUnavailableError,
    );
    assertEquals(registry.watchCloses, 4);
    await value.resolveContext(contextDigest);
    assertEquals(fetches, 5);
  } finally {
    value.stop();
  }
  await registry.watchClosed;
  assertEquals(registry.watchCloses, 5);
});

Deno.test("provider treats a missing local verification policy as unavailable", async () => {
  const installed = await installedCache();
  const value = await provider(
    {
      contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
      reads: [],
    },
    undefined,
    installed,
  );
  try {
    await installed.clear();
    await assertRejects(
      () => value.verifyEvent(event()),
      AuthorizationProviderUnavailableError,
    );
  } finally {
    value.stop();
  }
});

Deno.test("outer verifier rejects an invalid proof with telemetry disabled and installed", async () => {
  const message = () => ({
    data: new Uint8Array(),
    headers: (() => {
      const headers = natsHeaders();
      headers.set("authorization-context", chain.contextDigest);
      headers.set("session-key", request().sessionKey);
      headers.set("proof", "invalid");
      headers.set("iat", String(vectors.defaults.request.iat));
      headers.set("request-id", vectors.defaults.request.requestId);
      return headers;
    })(),
    reply: vectors.defaults.request.reply,
    subject: vectors.defaults.request.subject,
  });
  const permission = {
    apiId: "documents",
    apiVersion: "v1",
    surfaceKind: "rpc",
    surfaceName: "Documents.Get",
    action: "call",
  } satisfies DescriptorPermissionAtom;
  // No collector reader is installed: the optional observation must not
  // change the verifier's rejected outcome.
  metrics.disable();
  const value = await provider({
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  });
  try {
    const disabled = await verifyLocalAuthorization({
      kind: "request",
      cache: value,
      message: message(),
      permission,
      requiredCapabilities: [],
    });
    assertEquals(disabled.isErr(), true);
  } finally {
    value.stop();
  }
  const verifierExporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const verifierProvider = new MeterProvider({
    readers: [
      new PeriodicExportingMetricReader({
        exporter: verifierExporter,
        exportIntervalMillis: 60_000,
      }),
    ],
  });
  metrics.setGlobalMeterProvider(verifierProvider);
  const installed = await provider({
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  });
  try {
    const withTelemetry = await verifyLocalAuthorization({
      kind: "request",
      cache: installed,
      message: message(),
      permission,
      requiredCapabilities: [],
    });
    assertEquals(withTelemetry.isErr(), true);
    // Without an installed provider cache the outer boundary reports an
    // explicit unavailable outcome rather than silently succeeding.
    const unavailable = await verifyLocalAuthorization({
      kind: "request",
      cache: undefined,
      message: message(),
      permission,
      requiredCapabilities: [],
    });
    assertEquals(unavailable.isErr(), true);
    // A valid proof through the same outer boundary records success.
    const validHeaders = natsHeaders();
    validHeaders.set("authorization-context", chain.contextDigest);
    validHeaders.set("session-key", request().sessionKey);
    validHeaders.set("proof", chain.requestProof);
    validHeaders.set("iat", String(vectors.defaults.request.iat));
    validHeaders.set("request-id", vectors.defaults.request.requestId);
    const success = await verifyLocalAuthorization({
      kind: "request",
      cache: installed,
      message: {
        data: utf8(vectors.defaults.request.payload),
        headers: validHeaders,
        reply: vectors.defaults.request.reply,
        subject: vectors.defaults.request.subject,
      },
      permission,
      requiredCapabilities: [],
    });
    assertEquals(success.isOk(), true);
    await verifierProvider.forceFlush();
    const points = verifierExporter.getMetrics().at(-1)?.scopeMetrics
      .flatMap((scope) => scope.metrics)
      .find((metric) =>
        metric.descriptor.name === "trellis.auth.verification.duration"
      )
      ?.dataPoints ?? [];
    const outcomes = new Set(
      points.map((point) =>
        `${point.attributes["trellis.purpose"]}:${
          point.attributes["trellis.outcome"]
        }`
      ),
    );
    assertEquals(outcomes.has("request:invalid"), true);
    assertEquals(outcomes.has("request:unavailable"), true);
    assertEquals(outcomes.has("request:ok"), true);
    // The Event path through the same outer boundary records its own purpose.
    const eventHeaders = natsHeaders();
    eventHeaders.set("authorization-context", chain.contextDigest);
    eventHeaders.set("session-key", event().sessionKey);
    eventHeaders.set("proof", chain.eventProof);
    eventHeaders.set("Nats-Msg-Id", vectors.defaults.event.eventId);
    eventHeaders.set("Trellis-Event-Time", vectors.defaults.event.eventTime);
    eventHeaders.set(
      "Trellis-Event-Descriptor",
      vectors.defaults.event.descriptorIdentity,
    );
    const eventSuccess = await verifyLocalAuthorization({
      kind: "event",
      cache: installed,
      message: {
        data: utf8(vectors.defaults.event.payload),
        headers: eventHeaders,
        subject: vectors.defaults.event.subject,
      },
      permission: {
        apiId: "documents",
        apiVersion: "v1",
        surfaceKind: "event",
        surfaceName: "Documents.Changed",
        action: "publish",
      } satisfies DescriptorPermissionAtom,
      descriptorIdentity: vectors.defaults.event.descriptorIdentity,
      requiredCapabilities: [],
    });
    assert(eventSuccess.isOk(), JSON.stringify(eventSuccess));
    await verifierProvider.forceFlush();
    const eventPoints = verifierExporter.getMetrics().at(-1)?.scopeMetrics
      .flatMap((scope) => scope.metrics)
      .find((metric) =>
        metric.descriptor.name === "trellis.auth.verification.duration"
      )
      ?.dataPoints ?? [];
    assertEquals(
      eventPoints.some((point) =>
        point.attributes["trellis.purpose"] === "event" &&
        point.attributes["trellis.outcome"] === "ok"
      ),
      true,
    );
  } finally {
    installed.stop();
    await verifierProvider.shutdown();
    metrics.disable();
  }
});

Deno.test("lean request and event verifier outputs are enriched from cached context", async () => {
  const value = await provider({
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    reads: [],
  });
  try {
    const requestResult = await value.verifyRequest(request());
    assert(requestResult.ok, JSON.stringify(requestResult));
    assertEquals(
      requestResult.context.principalId,
      "01JY0000000000000000000002",
    );

    const eventResult = await value.verifyEvent(event());
    assert(eventResult.ok, JSON.stringify(eventResult));
    assertEquals(
      eventResult.context.connectionId,
      "01JY0000000000000000000001",
    );
    assertEquals(
      eventResult.publisher.connectionId,
      eventResult.context.connectionId,
    );

    const mismatchedRequest = request();
    mismatchedRequest.sessionKey =
      "UAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    assert(!(await value.verifyRequest(mismatchedRequest)).ok);
    const mismatchedEvent = event();
    mismatchedEvent.sessionKey =
      "UAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    assert(!(await value.verifyEvent(mismatchedEvent)).ok);

    const headers = natsHeaders();
    headers.set("authorization-context", chain.contextDigest);
    headers.set("session-key", request().sessionKey);
    headers.set("proof", chain.requestProof);
    headers.set("iat", String(vectors.defaults.request.iat));
    headers.set("request-id", vectors.defaults.request.requestId);
    const local = await verifyLocalAuthorization({
      kind: "request",
      cache: value,
      message: {
        data: utf8(vectors.defaults.request.payload),
        headers,
        reply: vectors.defaults.request.reply,
        subject: vectors.defaults.request.subject,
      },
      permission: {
        apiId: "documents",
        apiVersion: "v1",
        surfaceKind: "rpc",
        surfaceName: "Documents.Get",
        action: "call",
      } satisfies DescriptorPermissionAtom,
      requiredCapabilities: [],
    });
    const caller = local.take();
    if (isErr(caller)) throw caller.error;
    assertEquals(
      (caller as VerifiedCaller).connectionId,
      "01JY0000000000000000000001",
    );
  } finally {
    value.stop();
  }
});

Deno.test("provider explicitly denies a revoked context", async () => {
  const value = await provider({
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    revocations: new Map([[chain.contextDigest, 1_150]]),
    reads: [],
  });
  try {
    const requestResult = await value.verifyRequest(request());
    assert(!requestResult.ok);
    assertEquals(requestResult.error.code, "PermissionDenied");
    const eventResult = await value.verifyEvent(event());
    assert(!eventResult.ok);
    assertEquals(eventResult.error.code, "EventRevoked");
  } finally {
    value.stop();
  }
});

Deno.test("provider LRU stays at 256 entries and evicts the oldest context", async () => {
  const contexts = new Map<string, string>();
  for (let index = 0; index < 257; index += 1) {
    const [digest, context] = await signedContext(`connection-${index}`);
    contexts.set(digest, context);
  }
  let release = () => {};
  const registry: Registry = {
    contexts,
    reads: [],
    contextReadBarrier: new Promise<void>((resolve) => release = resolve),
  };
  const value = await provider(registry);
  try {
    const digests = [...contexts.keys()];
    const first = digests[0];
    const last = digests.at(-1);
    assert(first && last);
    const pending = digests.slice(0, 256).map((digest) =>
      value.resolveContext(digest)
    );
    await assertRejects(
      () => value.resolveContext(last),
      AuthorizationProviderUnavailableError,
    );
    release();
    await Promise.all(pending);
    registry.contextReadBarrier = undefined;
    await value.resolveContext(last);
    assertEquals(value.ioCounters().contextGets, 257);
    await value.resolveContext(last);
    assertEquals(value.ioCounters().contextGets, 257);
    await value.resolveContext(first);
    assertEquals(value.ioCounters().contextGets, 258);
  } finally {
    value.stop();
  }
});

Deno.test("provider coverage gauge follows the retained own and peer installation", async () => {
  const coverageExporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const coverageMeterProvider = new MeterProvider({
    readers: [
      new PeriodicExportingMetricReader({
        exporter: coverageExporter,
        exportIntervalMillis: 60_000,
      }),
    ],
  });
  metrics.setGlobalMeterProvider(coverageMeterProvider);
  const installed = await installedCache();
  const [peerDigest, peerContext] = await signedContext("peer-connection");
  const registry: Registry = {
    contexts: new Map([
      [chain.contextDigest, chain.contextCanonicalJson],
      [peerDigest, peerContext],
    ]),
    reads: [],
  };
  const value = await provider(registry, undefined, installed);
  const coverage = async () => {
    await coverageMeterProvider.forceFlush();
    const points = coverageExporter.getMetrics().at(-1)?.scopeMetrics
      .flatMap(
        (scope) => scope.metrics,
      )
      .find((metric) =>
        metric.descriptor.name === "trellis.auth.coverage.count"
      )
      ?.dataPoints ?? [];
    return Object.fromEntries(
      points.map((point) => [
        `${point.attributes["trellis.kind"]}:${
          point.attributes["trellis.state"]
        }`,
        point.value,
      ]),
    );
  };
  try {
    await value.waitReady();
    await value.retainOwnContext();
    assertEquals(await coverage(), {
      "own:covered": 1,
      "own:unavailable": 0,
      "peer:covered": 0,
      "peer:unavailable": 0,
    });
    await value.resolveContext(peerDigest);
    assertEquals(await coverage(), {
      "own:covered": 1,
      "own:unavailable": 0,
      "peer:covered": 1,
      "peer:unavailable": 0,
    });
    value.observeConnectionPhase("disconnected");
    assertEquals(await coverage(), {
      "own:covered": 0,
      "own:unavailable": 1,
      "peer:covered": 0,
      "peer:unavailable": 0,
    });
    value.observeConnectionPhase("connected");
    await value.waitReady();
    await value.retainOwnContext();
    assertEquals(await coverage(), {
      "own:covered": 1,
      "own:unavailable": 0,
      "peer:covered": 0,
      "peer:unavailable": 0,
    });
  } finally {
    value.stop();
    await coverageMeterProvider.shutdown();
    metrics.disable();
  }
});
Deno.test("provider coverage gauge marks a retained peer unavailable before eviction", async () => {
  const exporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const meterProvider = new MeterProvider({
    readers: [
      new PeriodicExportingMetricReader({
        exporter,
        exportIntervalMillis: 60_000,
      }),
    ],
  });
  metrics.setGlobalMeterProvider(meterProvider);
  let now = policy.nowUnixSeconds;
  const installed = await installedCache();
  const [peerDigest, peerContext] = await signedContext("peer-unavailable");
  const registry: Registry = {
    contexts: new Map([
      [chain.contextDigest, chain.contextCanonicalJson],
      [peerDigest, peerContext],
    ]),
    reads: [],
  };
  const value = await provider(registry, undefined, installed, () => now);
  const coverage = async () => {
    await meterProvider.forceFlush();
    const points = exporter.getMetrics().at(-1)?.scopeMetrics
      .flatMap((scope) => scope.metrics)
      .find((metric) =>
        metric.descriptor.name === "trellis.auth.coverage.count"
      )
      ?.dataPoints ?? [];
    return Object.fromEntries(
      points.map((point) => [
        `${point.attributes["trellis.kind"]}:${
          point.attributes["trellis.state"]
        }`,
        point.value,
      ]),
    );
  };
  try {
    await value.waitReady();
    await value.retainOwnContext();
    await value.resolveContext(peerDigest);
    assertEquals(await coverage(), {
      "own:covered": 1,
      "own:unavailable": 0,
      "peer:covered": 1,
      "peer:unavailable": 0,
    });
    // Advance the controllable clock past the retained peer's validity: it
    // stays indexed but is reported unavailable instead of covered.
    now = policy.nowUnixSeconds + 10_000_000;
    assertEquals(await coverage(), {
      "own:covered": 0,
      "own:unavailable": 1,
      "peer:covered": 0,
      "peer:unavailable": 1,
    });
    // Removing the source (evicting the cache) returns unavailable to zero.
    value.stop();
    assertEquals(await coverage(), {
      "own:covered": 0,
      "own:unavailable": 0,
      "peer:covered": 0,
      "peer:unavailable": 0,
    });
  } finally {
    value.stop();
    await meterProvider.shutdown();
    metrics.disable();
  }
});
Deno.test("outer verifier records bounded outcomes for request and event verification", async () => {
  const verifierExporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const verifierProvider = new MeterProvider({
    readers: [
      new PeriodicExportingMetricReader({
        exporter: verifierExporter,
        exportIntervalMillis: 60_000,
      }),
    ],
  });
  metrics.setGlobalMeterProvider(verifierProvider);
  const value = await provider({
    contexts: new Map([[chain.contextDigest, chain.contextCanonicalJson]]),
    revocations: new Map([[chain.contextDigest, 1_150]]),
    reads: [],
  });
  const verificationCounts = async () => {
    await verifierProvider.forceFlush();
    const points = verifierExporter.getMetrics().at(-1)?.scopeMetrics
      .flatMap((scope) => scope.metrics)
      .find((metric) =>
        metric.descriptor.name === "trellis.auth.verification.duration"
      )
      ?.dataPoints ?? [];
    const counts = new Map<string, number>();
    for (const point of points) {
      const label = `${point.attributes["trellis.purpose"]}:${
        point.attributes["trellis.outcome"]
      }`;
      counts.set(
        label,
        (counts.get(label) ?? 0) +
          (typeof point.value === "number" ? 1 : point.value.count),
      );
    }
    return counts;
  };
  try {
    const denied = await verifyLocalAuthorization({
      kind: "request",
      cache: value,
      message: {
        data: new Uint8Array(),
        headers: natsHeaders(),
        reply: "_INBOX.test.reply",
        subject: "rpc.v1.documents.Documents.Get",
      },
      permission: undefined,
      requiredCapabilities: [],
    });
    assertEquals(denied.isErr(), true);
    const missing = await verifyLocalAuthorization({
      kind: "request",
      cache: value,
      message: {
        data: new Uint8Array(),
        headers: natsHeaders(),
        reply: "_INBOX.test.reply",
        subject: "rpc.v1.documents.Documents.Get",
      },
      permission: {
        apiId: "documents",
        apiVersion: "v1",
        surfaceKind: "rpc",
        surfaceName: "Documents.Get",
        action: "call",
      } satisfies DescriptorPermissionAtom,
      requiredCapabilities: [],
    });
    assertEquals(missing.isErr(), true);
    const revoked = await verifyLocalAuthorization({
      kind: "event",
      cache: value,
      message: {
        data: new Uint8Array(),
        headers: natsHeaders(),
        subject: "events.v1.documents.Documents.Changed",
      },
      permission: undefined,
      descriptorIdentity: "documents@v1",
      requiredCapabilities: [],
    });
    assertEquals(revoked.isErr(), true);
    const counts = await verificationCounts();
    assertEquals((counts.get("request:denied") ?? 0) >= 1, true);
    assertEquals((counts.get("request:invalid") ?? 0) >= 1, true);
    assertEquals(
      (counts.get("event:denied") ?? 0) + (counts.get("event:revoked") ?? 0) >=
        1,
      true,
    );
  } finally {
    value.stop();
    await verifierProvider.shutdown();
    metrics.disable();
  }
});
