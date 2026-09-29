import {
  headers as natsHeaders,
  type Msg,
  type NatsConnection,
} from "@nats-io/nats-core";
import { assertEquals } from "@std/assert";

import { encodeEventSubjectParameterToken } from "../helpers.ts";
import { base64urlEncode } from "../auth/utils.ts";
import { LIVE_VERSION } from "./client_open.ts";
import {
  LiveProvider,
  type LiveProviderCaller,
  type LiveProviderHost,
  type LiveProviderIdentity,
  type ProviderAuthorityPort,
} from "./provider.ts";
import { LiveSessionManager } from "./manager.ts";

function digest(kind: string): string {
  const bytes = new Uint8Array(32);
  for (let i = 0; i < kind.length; i++) bytes[i] = kind.charCodeAt(i);
  return base64urlEncode(bytes);
}

const BASE_SUBJECT = `live.v1.route.${
  encodeEventSubjectParameterToken("api")
}.${encodeEventSubjectParameterToken("deploy")}.Watch`;

function identity(kind: string): LiveProviderIdentity {
  return {
    connectionId: `${kind}-conn`,
    sessionKey: `${kind}-session`,
    principalId: `${kind}-principal`,
    participantId: `${kind}-participant`,
    deploymentId: `${kind}-deploy`,
    instanceId: `${kind}-instance`,
  };
}

function caller(kind: string): LiveProviderCaller {
  return {
    ...identity(kind),
    contextDigest: digest(kind),
  };
}

function authority(kind: string): ProviderAuthorityPort {
  return {
    contextDigest: digest(kind),
    identity: { ...identity(kind) },
    checkNow: () => undefined,
    reconcile: async () => undefined,
    rebindCurrentGeneration: () => Promise.resolve(undefined),
    allows: () => true,
    subscribeChanges: () => () => {},
    release: () => {},
    prepareReplacement: async () => authority(kind),
    commitReplacement: () => undefined,
  };
}

function makeProvider(
  nats: NatsConnection,
  manager: LiveSessionManager = new LiveSessionManager(),
): LiveProvider {
  const host: LiveProviderHost = {
    nats,
    identity: identity("provider"),
    sign: async () => new Uint8Array(64),
    ownGuard: authority("provider"),
    permission: {
      target: {
        kind: "apiSurface",
        api: "api@v1",
        surface: "live",
        name: "Watch",
      },
      action: "subscribe",
    },
    refreshOwnAuthority: async () => {},
    retainCallerAuthority: async () => authority("owner"),
    manager,
  };
  return new LiveProvider(host);
}

function msg(args: {
  data: Uint8Array;
  reply?: string;
  headers?: ReturnType<typeof natsHeaders>;
  subject?: string;
}): Msg {
  return {
    subject: args.subject ?? "live.watch",
    data: args.data,
    reply: args.reply,
    headers: args.headers,
    respond: () => false,
    json: () => ({}),
    string: () => "",
    sid: 1,
  } as unknown as Msg;
}

Deno.test("NX04 foreign controls are dropped without reflection", async () => {
  const published: { subject: string; data: Uint8Array }[] = [];
  const nats = {
    publish(subject: string, data?: Uint8Array) {
      if (data) published.push({ subject, data });
    },
    info: { max_payload: 1_048_576 },
  } as unknown as NatsConnection;
  const provider = makeProvider(nats);
  const owner = caller("owner");
  let sourceStarts = 0;
  await provider.offer(
    msg({ data: new Uint8Array(), reply: "_INBOX.owner" }),
    BASE_SUBJECT,
    { openId: "open-1", receiveMaxPayloadBytes: 1_048_576 },
    owner,
    async () => {
      sourceStarts += 1;
    },
  );
  const offerJson = JSON.parse(
    new TextDecoder().decode(published.at(-1)!.data),
  );
  assertEquals(offerJson.kind, "standalone");
  const sessionId = offerJson.sessionId as string;
  const encode = (value: unknown) =>
    new TextEncoder().encode(JSON.stringify(value));
  const closeBody = encode({
    format: LIVE_VERSION,
    type: "control",
    sessionId,
    controlSeq: "1",
    action: "close",
    reason: "cancelled",
    receivedSeq: "0",
    consumedSeq: "0",
  });
  const ownerHeaders = natsHeaders();
  ownerHeaders.set("proof", "owner-proof");
  ownerHeaders.set("authorization-context", owner.contextDigest);
  ownerHeaders.set("session-key", owner.sessionKey);
  const allow = async (request: Msg) =>
    request.reply === "_INBOX.owner" &&
      request.headers?.get("proof") === "owner-proof" &&
      request.headers.get("authorization-context") === owner.contextDigest &&
      request.headers.get("session-key") === owner.sessionKey
      ? owner
      : undefined;

  await provider.handleControl(msg({ data: closeBody }), allow);
  await provider.handleControl(
    msg({
      data: closeBody,
      reply: "_INBOX.attacker",
      headers: ownerHeaders,
    }),
    allow,
  );
  const foreign = natsHeaders();
  foreign.set("proof", "not-a-proof");
  foreign.set("authorization-context", "attacker-digest");
  foreign.set("session-key", "attacker-session");
  await provider.handleControl(
    msg({
      data: closeBody,
      reply: "_INBOX.attacker",
      headers: foreign,
    }),
    allow,
  );
  await provider.handleControl(
    msg({ data: closeBody, reply: "_INBOX.owner", headers: ownerHeaders }),
    async () => ({ ...owner, connectionId: "owner-second-connection" }),
  );
  await provider.handleControl(
    msg({ data: closeBody, reply: "_INBOX.owner", headers: ownerHeaders }),
    async () => caller("other-principal"),
  );
  assertEquals(sourceStarts, 0);
  assertEquals(published.length, 1);
  const activateBody = encode({
    format: LIVE_VERSION,
    type: "control",
    sessionId,
    controlSeq: "1",
    action: "activate",
    receivedSeq: "0",
    consumedSeq: "0",
  });
  await provider.handleControl(
    msg({
      data: activateBody,
      reply: "_INBOX.owner",
      headers: ownerHeaders,
    }),
    allow,
  );
  const challenge = published
    .map((frame) => JSON.parse(new TextDecoder().decode(frame.data)))
    .find((value) => value.type === "challenge");
  assertEquals(typeof challenge.challengeId, "string");
  const pulseBody = encode({
    format: LIVE_VERSION,
    type: "control",
    sessionId,
    controlSeq: "2",
    action: "pulse",
    challengeId: challenge.challengeId,
    receivedSeq: "0",
    consumedSeq: "0",
  });
  await provider.handleControl(
    msg({
      data: pulseBody,
      reply: "_INBOX.owner",
      headers: ownerHeaders,
    }),
    allow,
  );
  assertEquals(sourceStarts, 1);
  await provider.handleControl(
    msg({
      data: pulseBody,
      reply: "_INBOX.owner",
      headers: ownerHeaders,
    }),
    allow,
  );
  assertEquals(sourceStarts, 1);
});

Deno.test("operation-watch offers advertise operation-watch kind", async () => {
  const encodedOffers: Uint8Array[] = [];
  const nats = {
    publish(_subject: string, data?: Uint8Array) {
      if (data) encodedOffers.push(data);
    },
    info: { max_payload: 1_048_576 },
  } as unknown as NatsConnection;
  const provider = makeProvider(nats);
  const operationSubject = `operation.v1.${
    encodeEventSubjectParameterToken("api")
  }.${encodeEventSubjectParameterToken("deploy")}.Run`;
  await provider.offer(
    msg({ data: new Uint8Array(), reply: "_INBOX.owner" }),
    operationSubject,
    { openId: "open-2", receiveMaxPayloadBytes: 1_048_576 },
    caller("owner"),
    async () => {},
    "operation",
  );
  const offerJson = JSON.parse(new TextDecoder().decode(encodedOffers.at(-1)!));
  assertEquals(offerJson.kind, "operation");
  assertEquals(offerJson.baseSubject, operationSubject);
});

Deno.test("NX05 a terminal close is acknowledged only by the owning provider", async () => {
  const manager = new LiveSessionManager();
  const publishedOwner: { subject: string; data: Uint8Array }[] = [];
  const publishedOther: { subject: string; data: Uint8Array }[] = [];
  const capture = (sink: { subject: string; data: Uint8Array }[]) =>
    ({
      publish(subject: string, data?: Uint8Array) {
        if (data) sink.push({ subject, data });
      },
      info: { max_payload: 1_048_576 },
    }) as unknown as NatsConnection;

  const owner = caller("owner");
  const hdrs = () => {
    const headers = natsHeaders();
    headers.set("proof", "owner-proof");
    headers.set("authorization-context", owner.contextDigest);
    headers.set("session-key", owner.sessionKey);
    return headers;
  };
  const allow = (request: Msg) =>
    Promise.resolve(
      request.reply === "_INBOX.owner" &&
        request.headers?.get("proof") === "owner-proof"
        ? owner
        : undefined,
    );
  const encode = (value: unknown) =>
    new TextEncoder().encode(JSON.stringify(value));

  const providerOwner = makeProvider(capture(publishedOwner), manager);
  const providerOther = makeProvider(capture(publishedOther), manager);

  await providerOwner.offer(
    msg({ data: new Uint8Array(), reply: "_INBOX.owner" }),
    BASE_SUBJECT,
    { openId: "open-1", receiveMaxPayloadBytes: 1_048_576 },
    owner,
    () => Promise.resolve(),
  );
  const sessionId = JSON.parse(
    new TextDecoder().decode(publishedOwner.at(-1)!.data),
  ).sessionId as string;

  const closeBody = encode({
    format: LIVE_VERSION,
    type: "control",
    sessionId,
    controlSeq: "1",
    action: "close",
    reason: "cancelled",
    receivedSeq: "0",
    consumedSeq: "0",
  });
  const closeMsg = () =>
    msg({ data: closeBody, reply: "_INBOX.owner", headers: hdrs() });

  // The owner closes the session; it terminates and retains a terminal receipt.
  publishedOwner.length = 0;
  await providerOwner.handleControl(closeMsg(), allow);
  const receiptDeadline = Date.now() + 15_000;
  while (
    manager.receipt(sessionId) === undefined && Date.now() < receiptDeadline
  ) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assertEquals(
    manager.receipt(sessionId) !== undefined,
    true,
    "a terminal session leaves a retained receipt",
  );
  assertEquals(
    publishedOwner.length > 0,
    true,
    "the owner answers the close",
  );

  // A non-owner must stay silent through the retained terminal window rather
  // than racing the owner's receipt acknowledgement with session_not_found.
  publishedOther.length = 0;
  await providerOther.handleControl(closeMsg(), allow);
  assertEquals(
    publishedOther.length,
    0,
    "a non-owner never answers a terminal close",
  );

  // The owner answers a close retry from its retained receipt.
  publishedOwner.length = 0;
  await providerOwner.handleControl(closeMsg(), allow);
  assertEquals(
    publishedOwner.length > 0,
    true,
    "the owner acknowledges the close retry from its receipt",
  );

  // A genuinely unknown session still gets the signed unknown-session error.
  const unknownBody = encode({
    format: LIVE_VERSION,
    type: "control",
    sessionId: base64urlEncode(new Uint8Array(16).fill(9)),
    controlSeq: "1",
    action: "close",
    reason: "cancelled",
    receivedSeq: "0",
    consumedSeq: "0",
  });
  publishedOther.length = 0;
  await providerOther.handleControl(
    msg({ data: unknownBody, reply: "_INBOX.owner", headers: hdrs() }),
    allow,
  );
  assertEquals(
    publishedOther.length > 0,
    true,
    "an unknown session keeps the signed unknown-session error",
  );
});

Deno.test("NX07 retained-receipt failover stays on the session's exact route", async () => {
  const manager = new LiveSessionManager();
  const publishedOwner: { subject: string; data: Uint8Array }[] = [];
  const publishedSameRoute: { subject: string; data: Uint8Array }[] = [];
  const publishedUnrelated: { subject: string; data: Uint8Array }[] = [];
  const capture = (sink: { subject: string; data: Uint8Array }[]) =>
    ({
      publish(subject: string, data?: Uint8Array) {
        if (data) sink.push({ subject, data });
      },
      info: { max_payload: 1_048_576 },
    }) as unknown as NatsConnection;
  const ROUTE_B = `live.v1.route.${encodeEventSubjectParameterToken("api-b")}.${
    encodeEventSubjectParameterToken("deploy-b")
  }.Watch`;

  const owner = caller("owner");
  const hdrs = () => {
    const headers = natsHeaders();
    headers.set("proof", "owner-proof");
    headers.set("authorization-context", owner.contextDigest);
    headers.set("session-key", owner.sessionKey);
    return headers;
  };
  const allow = (request: Msg) =>
    Promise.resolve(
      request.reply === "_INBOX.owner" &&
        request.headers?.get("proof") === "owner-proof"
        ? owner
        : undefined,
    );
  const encode = (value: unknown) =>
    new TextEncoder().encode(JSON.stringify(value));
  const closeBodyFor = (sessionId: string) =>
    encode({
      format: LIVE_VERSION,
      type: "control",
      sessionId,
      controlSeq: "1",
      action: "close",
      reason: "cancelled",
      receivedSeq: "0",
      consumedSeq: "0",
    });
  const waitForReceipt = async (sessionId: string) => {
    const deadline = Date.now() + 15_000;
    while (manager.receipt(sessionId) === undefined && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  };

  const providerOwner = makeProvider(capture(publishedOwner), manager);
  const providerSameRoute = makeProvider(capture(publishedSameRoute), manager);
  const providerUnrelated = makeProvider(capture(publishedUnrelated), manager);

  // The unrelated route registers first and is live the whole time.
  await providerUnrelated.offer(
    msg({ data: new Uint8Array(), reply: "_INBOX.owner" }),
    ROUTE_B,
    { openId: "open-b", receiveMaxPayloadBytes: 1_048_576 },
    owner,
    () => Promise.resolve(),
  );
  // A same-route provider that has served this route (a surviving generation).
  await providerSameRoute.offer(
    msg({ data: new Uint8Array(), reply: "_INBOX.owner" }),
    BASE_SUBJECT,
    { openId: "open-a2", receiveMaxPayloadBytes: 1_048_576 },
    owner,
    () => Promise.resolve(),
  );
  await providerOwner.offer(
    msg({ data: new Uint8Array(), reply: "_INBOX.owner" }),
    BASE_SUBJECT,
    { openId: "open-a1", receiveMaxPayloadBytes: 1_048_576 },
    owner,
    () => Promise.resolve(),
  );
  const sessionId = JSON.parse(
    new TextDecoder().decode(publishedOwner.at(-1)!.data),
  ).sessionId as string;
  const closeBody = closeBodyFor(sessionId);
  await providerOwner.handleControl(
    msg({ data: closeBody, reply: "_INBOX.owner", headers: hdrs() }),
    allow,
  );
  await waitForReceipt(sessionId);
  assertEquals(
    manager.receipt(sessionId) !== undefined,
    true,
    "a terminal session leaves a retained receipt",
  );

  await providerOwner.dispose();

  // The unrelated route never receives this frame and must stay silent.
  publishedUnrelated.length = 0;
  await providerUnrelated.handleControl(
    msg({ data: closeBody, reply: "_INBOX.owner", headers: hdrs() }),
    allow,
  );
  assertEquals(
    publishedUnrelated.length,
    0,
    "an unrelated-route provider never answers this session's close",
  );

  // The same-route survivor is the one authoritative responder.
  publishedSameRoute.length = 0;
  await providerSameRoute.handleControl(
    msg({ data: closeBody, reply: "_INBOX.owner", headers: hdrs() }),
    allow,
  );
  assertEquals(
    publishedSameRoute.length > 0,
    true,
    "the same-route survivor answers the retained receipt",
  );
});

Deno.test("NX08 an installed-but-unoffered same-route provider answers a retained receipt", async () => {
  const manager = new LiveSessionManager();
  const publishedOwner: { subject: string; data: Uint8Array }[] = [];
  const publishedSurvivor: { subject: string; data: Uint8Array }[] = [];
  const capture = (sink: { subject: string; data: Uint8Array }[]) =>
    ({
      publish(subject: string, data?: Uint8Array) {
        if (data) sink.push({ subject, data });
      },
      info: { max_payload: 1_048_576 },
    }) as unknown as NatsConnection;

  const owner = caller("owner");
  const hdrs = () => {
    const headers = natsHeaders();
    headers.set("proof", "owner-proof");
    headers.set("authorization-context", owner.contextDigest);
    headers.set("session-key", owner.sessionKey);
    return headers;
  };
  const allow = (request: Msg) =>
    Promise.resolve(
      request.reply === "_INBOX.owner" &&
        request.headers?.get("proof") === "owner-proof"
        ? owner
        : undefined,
    );
  const encode = (value: unknown) =>
    new TextEncoder().encode(JSON.stringify(value));

  const providerOwner = makeProvider(capture(publishedOwner), manager);
  const providerSurvivor = makeProvider(capture(publishedSurvivor), manager);
  // The survivor is installed for the route but has never served a session:
  // it must still be in the ownership registry from readiness, not from
  // incidental first traffic.
  providerSurvivor.registerRoute(BASE_SUBJECT);

  await providerOwner.offer(
    msg({ data: new Uint8Array(), reply: "_INBOX.owner" }),
    BASE_SUBJECT,
    { openId: "open-1", receiveMaxPayloadBytes: 1_048_576 },
    owner,
    () => Promise.resolve(),
  );
  const sessionId = JSON.parse(
    new TextDecoder().decode(publishedOwner.at(-1)!.data),
  ).sessionId as string;
  const closeBody = encode({
    format: LIVE_VERSION,
    type: "control",
    sessionId,
    controlSeq: "1",
    action: "close",
    reason: "cancelled",
    receivedSeq: "0",
    consumedSeq: "0",
  });
  await providerOwner.handleControl(
    msg({ data: closeBody, reply: "_INBOX.owner", headers: hdrs() }),
    allow,
  );
  const deadline = Date.now() + 15_000;
  while (manager.receipt(sessionId) === undefined && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assertEquals(
    manager.receipt(sessionId) !== undefined,
    true,
    "a terminal session leaves a retained receipt",
  );

  await providerOwner.dispose();
  publishedSurvivor.length = 0;
  await providerSurvivor.handleControl(
    msg({ data: closeBody, reply: "_INBOX.owner", headers: hdrs() }),
    allow,
  );
  assertEquals(
    publishedSurvivor.length > 0,
    true,
    "the installed-but-unoffered same-route survivor answers the receipt",
  );
});
