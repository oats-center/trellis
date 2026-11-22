/**
 * Real-boundary live admission acceptance.
 *
 * Proves production admission against a real broker and real issued caller
 * credentials: a correctly signed opening whose reply is foreign is dropped
 * without reflection, and a foreign principal's session control cannot disturb
 * the owner's active observation. A provider holding its per-caller session
 * bound refuses that caller's next opening with a typed, retryable code.
 */

import { headers as natsHeaders, type Msg } from "@nats-io/nats-core";
import { credsAuthenticator } from "@nats-io/nats-core";
import { connect, type NatsConnection } from "@nats-io/transport-node";
import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { TransportError } from "@oatscenter/trellis";
import type { SessionRequest } from "@oatscenter/trellis/auth";
import { TrellisService } from "@oatscenter/trellis/service";
import { connectNativeClient } from "@oatscenter/trellis/native";
import { ulid } from "ulid";
import { apis as webApis } from "../packages/trellis/internal_sdk/generated/index.js";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import {
  admitRawNativeClient,
  type RawNativeConnection,
} from "./_support/client_session.ts";

const HEALTH_API = "trellis.health@v1";
const NATIVE_REQUEST = apps.Observer.request;

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/**
 * A fresh destination outside the caller's inbox prefix. The health service
 * continuously publishes real `StatusChanged` events, so the foreign probe
 * must use a quiet subject to distinguish reflection from ordinary traffic.
 */
function foreignReplySubject(): string {
  return `live.v1.data.p07.${ulid()}`;
}

function nonce(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

function openBody(): Uint8Array {
  return new TextEncoder().encode(
    JSON.stringify({
      format: "trellis.live.v1",
      type: "open",
      openId: nonce(),
      receiveMaxPayloadBytes: 1_048_576,
      input: {},
    }),
  );
}

function closeControlBody(sessionId: string): Uint8Array {
  return new TextEncoder().encode(
    JSON.stringify({
      format: "trellis.live.v1",
      type: "control",
      sessionId,
      controlSeq: "1",
      action: "close",
      reason: "cancelled",
      receivedSeq: "0",
      consumedSeq: "0",
    }),
  );
}

/** First message on a subscription iterator within a bounded wait, or undefined. */
function firstWithin(
  iterator: AsyncIterator<Msg>,
  timeoutMs: number,
): Promise<Msg | undefined> {
  return Promise.race([
    iterator.next().then((next) => (next.done ? undefined : next.value)),
    new Promise<undefined>((resolve) =>
      setTimeout(() => resolve(undefined), timeoutMs),
    ),
  ]);
}

/** Signs one live request body with the caller's ordinary issued proof. */
async function signedHeaders(
  session: RawNativeConnection,
  subject: string,
  reply: string,
  body: Uint8Array,
) {
  const iat = session.auth.currentIat();
  const requestId = ulid();
  const api = session.authority.apis.find((api) => api.apiId === HEALTH_API);
  assert(api);
  const request: SessionRequest = {
    authorityDigest: session.contextDigest,
    apiId: api.apiId,
    apiGeneration: api.generation,
    acceptedRevision: api.acceptedRevision,
    action: "Watch",
    subject,
    replySubject: reply,
    requestId,
    issuedAt: iat,
  };
  const { proof } = await session.auth.createProof(request, body);
  const headers = natsHeaders();
  headers.set("proof", proof);
  headers.set("iat", String(iat));
  headers.set("request-id", requestId);
  headers.set("authorization-context", session.contextDigest);
  headers.set("session-key", session.auth.sessionKey);
  headers.set("Trellis-Session-Request", JSON.stringify(request));
  headers.set("Trellis-Api-Generation", api.generation);
  headers.set("Trellis-Accepted-Revision", api.acceptedRevision);
  return headers;
}

/**
 * Reads the owner's live offer.
 *
 * A connection-scoped inbox prefix also carries the owner's own live traffic,
 * so the observer scans until it sees the offer rather than assuming it is the
 * first message.
 */
async function firstLiveOfferWithin(
  iterator: AsyncIterator<Msg>,
  timeoutMs: number,
): Promise<{ sessionId?: string; controlSubject?: string } | undefined> {
  const deadline = Date.now() + timeoutMs;
  while (true) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) return undefined;
    const next = await firstWithin(iterator, remaining);
    if (!next) return undefined;
    try {
      const body = JSON.parse(new TextDecoder().decode(next.data)) as {
        type?: string;
        sessionId?: string;
        controlSubject?: string;
      };
      if (body.type === "offer") return body;
    } catch {
      // Not a live protocol message; keep scanning.
    }
  }
}

/** Opens a privileged ordinary connection for observation and injection. */
async function platformConnection(runtime: Runtime): Promise<NatsConnection> {
  return await connect({
    servers: runtime.natsUrl,
    authenticator: credsAuthenticator(await runtime.natsCredentials("trellis")),
  });
}

test("P07 a signed open with a foreign reply is dropped without reflection", async () => {
  await withTrellisRuntime(async (runtime) => {
    const session = await admitRawNativeClient(
      await runtime.registerClient({
        name: "p07-caller",
        contract: NATIVE_REQUEST,
      }),
    );

    // The caller's own issued connection injects the request, so it is
    // admitted as the caller. A privileged ordinary connection observes the
    // foreign destination, which the caller's ACL cannot subscribe to.
    const callerNats = session.nc;
    const observer = await platformConnection(runtime);
    const base = webApis.health.API.actions["live:Watch"].subject;
    const foreign = foreignReplySubject();
    try {
      const foreignIterator = observer
        .subscribe(foreign)
        [Symbol.asyncIterator]();
      const inboxIterator = observer
        .subscribe(`${session.inboxPrefix}.p07`)
        [Symbol.asyncIterator]();
      await callerNats.flush();
      await observer.flush();

      // Prove the foreign destination is observable before injecting.
      observer.publish(foreign, new Uint8Array());
      const sentinel = await firstWithin(foreignIterator, 5_000);
      assert(sentinel, "foreign callback subject must be observable");

      // The caller's authenticated inbox receives a signed offer, proving the
      // signed-open path itself is admitted.
      const inboxReply = `${session.inboxPrefix}.p07`;
      const validBody = openBody();
      callerNats.publish(base, validBody, {
        headers: await signedHeaders(session, base, inboxReply, validBody),
        reply: inboxReply,
      });
      await callerNats.flush();

      const offer = await firstWithin(inboxIterator, 10_000);
      assert(offer, "the caller's authenticated inbox must receive the offer");
      const offerBody = JSON.parse(new TextDecoder().decode(offer.data)) as {
        sessionId?: string;
      };
      assert(offerBody.sessionId, "the admitted offer carries a session id");

      const foreignBody = openBody();
      callerNats.publish(base, foreignBody, {
        headers: await signedHeaders(session, base, foreign, foreignBody),
        reply: foreign,
      });
      await callerNats.flush();

      const reflected = await firstWithin(foreignIterator, 3_000);
      assert(
        reflected === undefined,
        `a foreign-reply open must not reflect an error or application frame: ${
          reflected?.subject ?? ""
        } ${reflected ? new TextDecoder().decode(reflected.data) : ""}`,
      );
    } finally {
      await callerNats.close().catch(() => undefined);
      await observer.close().catch(() => undefined);
    }
  });
});

test("P08 another authenticated runtime cannot close the owner's observation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const login = await runtime.registerClient({
      name: "p08-owner",
      contract: NATIVE_REQUEST,
    });
    const owner = await connectNativeClient({
      ...login,
      request: NATIVE_REQUEST,
    }).orThrow();
    const own = login.session.auth.authorityOwner?.own();
    assert(own);
    const inboxPrefix = own.authority.inboxPrefix;
    own.release();
    const observer = await platformConnection(runtime);
    const offerIterator = observer
      .subscribe(`${inboxPrefix}.>`)
      [Symbol.asyncIterator]();
    await observer.flush();

    // The real owner opens the built-in Health feed; the observer captures the
    // offer to learn the session and control route.
    const feed = await owner.healthWatch({}).orThrow();
    const offer = await firstLiveOfferWithin(offerIterator, 10_000);
    assert(offer, "the owner offer must be observable");
    assert(
      offer.sessionId && offer.controlSubject,
      "the offer carries the session route",
    );

    // Activate the owner observation with a real frame before the foreign
    // control: a prepared session is not an active observation.
    const ownerIterator = feed[Symbol.asyncIterator]();
    const firstFrame = await ownerIterator.next();
    assert(!firstFrame.done, "the owner must receive a real activation frame");

    // A different admitted runtime must not control this session, even when
    // both ordinary logins belong to the same administrator.
    const intruderSession = await admitRawNativeClient(
      await runtime.registerClient({
        name: "p08-intruder",
        contract: NATIVE_REQUEST,
      }),
    );
    const intruderNats = intruderSession.nc;
    try {
      const intruderInbox = `${intruderSession.inboxPrefix}.p08`;
      const control = closeControlBody(offer.sessionId);
      intruderNats.publish(offer.controlSubject, control, {
        headers: await signedHeaders(
          intruderSession,
          offer.controlSubject,
          intruderInbox,
          control,
        ),
        reply: intruderInbox,
      });
      await intruderNats.flush();

      // The owner's observation must survive the foreign control: an unrelated
      // RPC still succeeds and the session is not closed.
      const unrelated = await owner.jobsSummary({}).orThrow();
      assert(unrelated !== undefined, "an unrelated RPC must still succeed");
      const settled = await Promise.race([
        feed.closed.then(() => "closed" as const),
        new Promise<"open">((resolve) =>
          setTimeout(() => resolve("open"), 4_000),
        ),
      ]);
      assert(
        settled === "open",
        "a foreign principal's control must not close the owner observation",
      );
    } finally {
      await intruderNats.close().catch(() => undefined);
      await observer.close().catch(() => undefined);
      try {
        await feed.close().orThrow();
      } catch {
        // The owner feed may already be settling after the assertion.
      }
      await owner.connection.close().catch(() => undefined);
    }
  });
});

/** Connects one ordinary caller of the runtime fixture's `Watch`. */
function connectCaller(runtime: Runtime, name: string) {
  return runtime.connectClient({ name, contract: apps.Caller.request });
}

type Caller = Awaited<ReturnType<typeof connectCaller>>;

/**
 * Fills one provider's per-caller bound of 16 sessions, then proves the next
 * opening is refused as `resource_exhausted` rather than as a provider fault,
 * and that closing one session admits a new opening.
 */
async function assertPerCallerBound(caller: Caller): Promise<void> {
  const feeds: Awaited<ReturnType<Caller["runtimeWatch"]>>[] = [];
  try {
    for (let index = 0; index < 16; index += 1) {
      const feed = await caller.runtimeWatch({});
      assert(feed.isOk(), `opening ${index + 1} of 16: ${String(feed)}`);
      feeds.push(feed);
    }
    const refused = await caller.runtimeWatch({});
    assert(refused.isErr(), "the 17th opening must be refused");
    assert(refused.error instanceof TransportError, String(refused.error));
    assert.equal(refused.error.code, "trellis.live.resource_exhausted");

    await feeds.pop()!.orThrow().close().orThrow();
    const admitted = await caller.runtimeWatch({});
    assert(admitted.isOk(), `opening after a close: ${String(admitted)}`);
    feeds.push(admitted);
  } finally {
    await Promise.all(
      feeds.map(async (feed) => {
        if (feed.isOk()) await feed.orThrow().close();
      }),
    );
  }
}

test("a TypeScript Live provider holding one caller's session bound refuses the next opening as resource_exhausted", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "ts-bound-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    try {
      await service.handleWatch(async ({ emit }) => {
        await emit({ value: "ready" }).orThrow();
      });
      const caller = await connectCaller(runtime, "ts-bound-caller");
      await assertPerCallerBound(caller);
    } finally {
      await service.stop();
      await service.wait();
    }
  });
});

test("a Rust Live provider holding one caller's session bound refuses the next opening as resource_exhausted", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust-bound-provider",
      contract: participants.OperationProvider.participant,
    });
    const provider = spawnTracked(
      "setsid",
      rustFixtureArgv("trellis-runtime-acceptance"),
      {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificatePath,
          TRELLIS_IDENTITY_SEED: identity.seed,
          CARGO_TARGET_DIR: fileURLToPath(
            new URL("../../target", import.meta.url),
          ),
        },
      },
    );
    provider.stderr.pipe(process.stderr);
    let exited = false;
    const status = provider.exited.then((value) => {
      exited = true;
      return value;
    });
    try {
      const caller = await connectCaller(runtime, "rust-bound-caller");
      await runtime.waitFor(
        async () => {
          if (exited) {
            throw new Error(
              `Rust provider exited: ${JSON.stringify(await status)}`,
            );
          }
          return (
            await caller.echo({ value: "from TypeScript" }, { timeout: 1000 })
          ).isOk();
        },
        { timeoutMs: 120_000 },
      );
      await assertPerCallerBound(caller);
    } finally {
      if (!exited && provider.pid !== undefined)
        process.kill(-provider.pid, "SIGTERM");
      await status;
    }
  });
});
