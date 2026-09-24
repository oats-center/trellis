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
import { assert } from "@std/assert";
import { join } from "@std/path";
import { TrellisClient } from "@oatscenter/trellis";
import { participants as webParticipants } from "trellis-web-generated";

import { withTrellisRuntime } from "./_support/runtime.ts";
import {
  type ClientSession,
  clientSession,
} from "./_support/client_session.ts";

const HEALTH_API = "trellis.health@v1";
const HEALTH_DEPLOYMENT = "dep_trellis_health_runtime";
const CONSOLE = webParticipants.Console.participant;

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

function b64url(value: string): string {
  return btoa(String.fromCharCode(...new TextEncoder().encode(value)))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

function liveBaseSubject(): string {
  return `live.v1.route.${b64url(HEALTH_API)}.${
    b64url(HEALTH_DEPLOYMENT)
  }.Watch`;
}

/**
 * A fresh destination outside the caller's inbox prefix. The health service
 * continuously publishes real `StatusChanged` events, so the foreign probe
 * must use a quiet subject to distinguish reflection from ordinary traffic.
 */
function foreignReplySubject(): string {
  return `live.v1.data.p07.${crypto.randomUUID().replaceAll("-", "")}`;
}

function nonce(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

function openBody(): Uint8Array {
  return new TextEncoder().encode(JSON.stringify({
    format: "trellis.live.v1",
    type: "open",
    openId: nonce(),
    receiveMaxPayloadBytes: 1_048_576,
    input: {},
  }));
}

function closeControlBody(sessionId: string): Uint8Array {
  return new TextEncoder().encode(JSON.stringify({
    format: "trellis.live.v1",
    type: "control",
    sessionId,
    controlSeq: "1",
    action: "close",
    reason: "cancelled",
    receivedSeq: "0",
    consumedSeq: "0",
  }));
}

/** First message on a subscription iterator within a bounded wait, or undefined. */
function firstWithin(
  iterator: AsyncIterator<Msg>,
  timeoutMs: number,
): Promise<Msg | undefined> {
  return Promise.race([
    iterator.next().then((next) => next.done ? undefined : next.value),
    new Promise<undefined>((resolve) =>
      setTimeout(() => resolve(undefined), timeoutMs)
    ),
  ]);
}

/** Signs one live request body with the caller's ordinary issued proof. */
async function signedHeaders(
  session: ClientSession,
  subject: string,
  reply: string,
  body: Uint8Array,
) {
  const iat = session.auth.currentIat();
  const requestId = crypto.randomUUID();
  const payloadHash = new Uint8Array(
    await crypto.subtle.digest("SHA-256", Uint8Array.from(body)),
  );
  const proof = await session.auth.createProof(
    subject,
    payloadHash,
    reply,
    requestId,
    iat,
  );
  const headers = natsHeaders();
  headers.set("proof", proof);
  headers.set("iat", String(iat));
  headers.set("request-id", requestId);
  headers.set("authorization-context", session.contextDigest);
  headers.set("session-key", session.auth.sessionKey);
  return headers;
}

/** Registers and connects one ordinary Console caller plus its issued session. */
async function consoleCaller(runtime: Runtime, name: string) {
  const key = await runtime.registerClient({ name, contract: CONSOLE });
  const client = await TrellisClient.connect({
    name,
    trellisUrl: runtime.trellisUrl,
    participant: CONSOLE,
    ...runtime.clientAuth(key),
  }).orThrow();
  const session = await clientSession(runtime, key);
  return { client, session };
}

/** Opens a privileged ordinary connection for observation and injection. */
async function platformConnection(runtime: Runtime): Promise<NatsConnection> {
  return await connect({
    servers: runtime.natsUrl,
    authenticator: credsAuthenticator(
      await Deno.readFile(
        join(runtime.workdir, "nats/creds/trellis-auth.creds"),
      ),
    ),
  });
}

Deno.test("P07 a signed open with a foreign reply is dropped without reflection", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { client, session } = await consoleCaller(runtime, "p07-caller");

    // The caller's own issued connection injects the request, so it is
    // admitted as the caller. A privileged ordinary connection observes the
    // foreign destination, which the caller's ACL cannot subscribe to.
    const callerNats = await connect({
      servers: runtime.natsUrl,
      authenticator: session.authenticator,
    });
    const observer = await platformConnection(runtime);
    const base = liveBaseSubject();
    const foreign = foreignReplySubject();
    try {
      const foreignIterator = observer.subscribe(foreign)
        [Symbol.asyncIterator]();
      const inboxIterator = observer.subscribe(
        `${session.inboxPrefix}.p07`,
      )[Symbol.asyncIterator]();
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
      await client.connection.close().catch(() => undefined);
    }
  });
});

Deno.test("P08 a foreign principal's session control is denied and the owner continues", async () => {
  await withTrellisRuntime(async (runtime) => {
    const owner = await consoleCaller(runtime, "p08-owner");
    const observer = await platformConnection(runtime);
    const offerIterator = observer.subscribe(
      `${owner.session.inboxPrefix}.>`,
    )[Symbol.asyncIterator]();
    await observer.flush();

    // The real owner opens the built-in Health feed; the observer captures the
    // offer to learn the session and control route.
    const feed = await owner.client.healthWatch({}).orThrow();
    const offerMessage = await firstWithin(offerIterator, 10_000);
    assert(offerMessage, "the owner offer must be observable");
    const offer = JSON.parse(new TextDecoder().decode(offerMessage.data)) as {
      sessionId?: string;
      controlSubject?: string;
    };
    assert(
      offer.sessionId && offer.controlSubject,
      "the offer carries the session route",
    );

    // Activate the owner observation with a real frame before the foreign
    // control: a prepared session is not an active observation.
    const ownerIterator = feed[Symbol.asyncIterator]();
    const firstFrame = await ownerIterator.next();
    assert(
      !firstFrame.done,
      "the owner must receive a real activation frame",
    );

    // A different Console principal signs a close control for the owner session.
    const intruder = await consoleCaller(runtime, "p08-intruder");
    const intruderNats = await connect({
      servers: runtime.natsUrl,
      authenticator: intruder.session.authenticator,
    });
    try {
      const intruderInbox = `${intruder.session.inboxPrefix}.p08`;
      const control = closeControlBody(offer.sessionId);
      intruderNats.publish(offer.controlSubject, control, {
        headers: await signedHeaders(
          intruder.session,
          offer.controlSubject,
          intruderInbox,
          control,
        ),
        reply: intruderInbox,
      });
      await intruderNats.flush();

      // The owner's observation must survive the foreign control: an unrelated
      // RPC still succeeds and the session is not closed.
      const unrelated = await owner.client.sessionsList({}).orThrow();
      assert(unrelated !== undefined, "an unrelated RPC must still succeed");
      const settled = await Promise.race([
        feed.closed.then(() => "closed" as const),
        new Promise<"open">((resolve) =>
          setTimeout(() => resolve("open"), 4_000)
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
      await intruder.client.connection.close().catch(() => undefined);
      await owner.client.connection.close().catch(() => undefined);
    }
  });
});
