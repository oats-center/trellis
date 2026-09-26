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
    maintenance: () => false,
    reconcile: async () => undefined,
    rebindCurrentGeneration: () => Promise.resolve(undefined),
    allows: () => true,
    subscribeChanges: () => () => {},
    release: () => {},
    prepareReplacement: async () => authority(kind),
    commitReplacement: () => undefined,
  };
}

function makeProvider(nats: NatsConnection): LiveProvider {
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
    manager: new LiveSessionManager(),
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
