/**
 * Real-boundary live admission acceptance.
 *
 * Proves production admission against a real broker and real issued caller
 * credentials: a correctly signed opening whose reply is foreign is dropped
 * without reflection, and a foreign principal's session control cannot disturb
 * the owner's active observation.
 */

import { headers as natsHeaders, type Msg } from "@nats-io/nats-core";
import { credsAuthenticator } from "@nats-io/nats-core";
import { connect, type NatsConnection } from "@nats-io/transport-node";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { join } from "node:path";
import type { SessionRequest } from "@oatscenter/trellis/auth";
import { connectNativeClient } from "@oatscenter/trellis/native";
import { ulid } from "ulid";
import { apis as webApis } from "../../web/trellis/index.js";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

import { withTrellisRuntime } from "./_support/runtime.ts";
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
    authenticator: credsAuthenticator(
      await readFile(
        join(runtime.workdir, "config/trellis/nats/creds/trellis-auth.creds"),
      ),
    ),
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
