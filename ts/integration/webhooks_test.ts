/**
 * Live acceptance for receiving webhooks: a service declares endpoints in its
 * IDL, the test sets their secrets through the ordinary settings RPCs, and
 * real HTTPS requests reach the endpoint URLs Trellis serves. Each scenario
 * checks what the sender gets back, what Trellis records, and what reaches
 * the service's handlers.
 */

import { createHash, createHmac } from "node:crypto";
import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";

import {
  TrellisService,
  type WebhookDelivery,
  webhookResponse,
} from "@oatscenter/trellis/service";
import { codecs } from "@oatscenter/trellis/generated";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as laterParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { apis as adminApis } from "../packages/trellis-testkit/trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const participant = participants.WebhookService.participant;
type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

const SIGNING = "whsec_signing-secret";
const TOKEN = "hook-token-value";
const PASSWORD = "partner-password-value";
const RELAY = "relay-signing-value";
// Where the relay sends from: the test itself, over either loopback.
const RELAY_RANGES = ["127.0.0.0/8", "::1/128"];

function sign(text: string, key = SIGNING): string {
  return createHmac("sha256", key).update(text).digest("hex");
}

async function install(runtime: Runtime) {
  const key = await runtime.services.createInstance({
    name: "billing",
    deployment: "billing",
    contract: participant,
    config: {
      values: { partnerUser: "partner", relayRanges: RELAY_RANGES },
      secrets: {
        signing: SIGNING,
        hookToken: TOKEN,
        partnerPassword: PASSWORD,
      },
    },
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant,
    seed: key.seed,
  }).orThrow();
  const { endpoints } = await runtime.callAdminRpc("webhooksEndpointsList", {
    service: key.deploymentId,
  });
  const url = (name: string) => {
    const endpoint = endpoints.find((entry) => entry.endpoint === name);
    assert.ok(endpoint, `endpoint ${name} is listed`);
    return endpoint.url;
  };
  const attempts = async (endpoint: string) =>
    (
      await runtime.callAdminRpc("webhooksAttemptsQuery", {
        service: key.deploymentId,
        endpoint,
      })
    ).attempts;
  return { key, service, url, attempts };
}

/** Change a deployment's settings and secrets as an administrator does. */
async function configure(
  runtime: Runtime,
  deploymentId: string,
  change: {
    values?: { relayRanges: string[] };
    secrets?: Record<string, string>;
    unset?: string[];
  },
) {
  const target = { deploymentId: codecs.ulid.decode(deploymentId) };
  const { revision } = await runtime.callAdminRpc("authConfigGet", { target });
  await runtime.callAdminRpc("authConfigSet", {
    target,
    ...change,
    ...(revision === undefined ? {} : { expectedRevision: revision }),
    idempotencyKey: codecs.ulid.decode(ulid()),
  });
}

/**
 * Give `endpoint` a 5-second change overlap, and return how to read what it
 * reports about one of its secrets.
 */
async function shortOverlap(
  runtime: Runtime,
  deploymentId: string,
  endpoint: string,
) {
  await runtime.callAdminRpc("authWebhooksUpdate", {
    deploymentId: codecs.ulid.decode(deploymentId),
    endpoint,
    change: "set",
    setting: "rotationOverlap",
    value: 5_000n,
    idempotencyKey: codecs.ulid.decode(ulid()),
  });
  const report = async (secret: string) => {
    const { endpoints } = await runtime.callAdminRpc("webhooksEndpointsList", {
      service: deploymentId,
    });
    const found = endpoints.find((entry) => entry.endpoint === endpoint)!;
    const reported = found.secrets.find((entry) => entry.name === secret)!;
    return {
      overlapMs: found.settings.rotationOverlapMs,
      isSet: reported.isSet,
      overlap: reported.previousAcceptedUntil !== null,
    };
  };
  await runtime.waitFor(
    async () => (await report("relaySigning")).overlapMs === 5_000n,
  );
  return report;
}

/** A relayed request: the sender's credential and the relay's signature. */
function relay(url: string, body: string, headers: Record<string, string>) {
  return fetch(url, {
    method: "POST",
    body,
    headers: { ...headers, "x-relay-signature": `sha256=${sign(body, RELAY)}` },
  });
}

test("a signed async endpoint answers at once, verifies afterwards, drops duplicates, and delivers the exact bytes", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url, attempts } = await install(runtime);
    const received: WebhookDelivery[] = [];
    await service.webhooks.signed
      .onReceived(({ event }) => {
        received.push(event);
      })
      .orThrow();
    // Durable deliveries flow once the service serves.
    void service.wait();
    const endpoint = url("signed");
    assert.match(endpoint, /\/webhooks\/billing\/[A-Za-z0-9_-]{22}$/u);

    const post = (body: string, headers: Record<string, string>) =>
      fetch(endpoint, { method: "POST", body, headers });
    const now = () => Math.floor(Date.now() / 1000);
    const signed = (body: string, timestamp = now()) =>
      `t=${timestamp},v1=${sign(`${timestamp}.${body}`)}`;

    // A genuine request becomes an event carrying the body byte for byte.
    const body = '{"id":"evt_1","amount":12.50,  "note":"spaces kept"}';
    const accepted = await post(body, {
      "content-type": "application/json",
      "test-signature": signed(body),
      "x-delivery-id": "delivery-1",
    });
    assert.equal(accepted.status, 200);
    await runtime.waitFor(() => (received.length === 1 ? true : undefined));
    assert.equal(new TextDecoder().decode(received[0]!.body), body);
    assert.equal(received[0]!.endpoint, "signed");
    assert.equal(received[0]!.dedupeKey, "delivery-1");
    assert.deepEqual(
      received[0]!.checks.map((check) => [check.kind, check.key]),
      [["hmac", "current"]],
    );

    // A bad signature still gets 2xx at once: the sender cannot tell, and the
    // record says what failed.
    const forged = await post(body, {
      "test-signature": `t=${now()},v1=${sign(`${now()}.${body}`, "wrong-key")}`,
    });
    assert.equal(forged.status, 200);
    // A replay outside the tolerance fails the same way.
    const stale = await post(body, {
      "test-signature": signed(body, now() - 600),
    });
    assert.equal(stale.status, 200);
    const rejected = await runtime.waitFor(async () => {
      const records = (await attempts("signed")).filter(
        (attempt) => attempt.outcome === "rejected",
      );
      return records.length === 2 ? records : undefined;
    });
    assert.deepEqual(
      rejected
        .map((attempt) => attempt.reason?.replace(/\d+ s/u, "N s"))
        .sort(),
      ["signature does not match", "timestamp N s outside tolerance"],
    );

    // A verified repeat of the same delivery ID is recorded and publishes nothing.
    const repeat = await post(body, {
      "test-signature": signed(body),
      "x-delivery-id": "delivery-1",
    });
    assert.equal(repeat.status, 200);
    await runtime.waitFor(async () =>
      (await attempts("signed")).some(
        (attempt) => attempt.outcome === "duplicate",
      )
        ? true
        : undefined,
    );
    assert.equal(received.length, 1);

    // The body size limit is checked before anything is stored.
    const tooLarge = await post("x".repeat(5 * 1024), {
      "test-signature": signed("x"),
    });
    assert.equal(tooLarge.status, 413);

    // A request rejected for a wrong secret is recovered once the secret is
    // fixed: verifying it again uses its original bytes and receive time.
    const wrongKey = (await attempts("signed")).find(
      (attempt) => attempt.reason === "signature does not match",
    )!;
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const { revision } = await runtime.callAdminRpc("authConfigGet", {
      target,
    });
    await runtime.callAdminRpc("authConfigSet", {
      target,
      secrets: { signing: "wrong-key" },
      ...(revision === undefined ? {} : { expectedRevision: revision }),
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    // A record is verified again only under the endpoint it belongs to.
    await assert.rejects(
      runtime.callAdminRpc("webhooksAttemptsReverify", {
        service: key.deploymentId,
        endpoint: "handshake",
        attemptId: codecs.ulid.decode(wrongKey.attemptId),
      }),
      /no such request/u,
    );
    const reverified = await runtime.waitFor(async () => {
      const { attempt } = await runtime.callAdminRpc(
        "webhooksAttemptsReverify",
        {
          service: key.deploymentId,
          endpoint: "signed",
          attemptId: codecs.ulid.decode(wrongKey.attemptId),
        },
      );
      return attempt.outcome === "accepted" ? attempt : undefined;
    });
    assert.equal(reverified.matchedKey, "current");
    await runtime.waitFor(() => (received.length === 2 ? true : undefined));
    // The previous secret is still accepted during the change overlap.
    const overlap = await post(body, { "test-signature": signed(body) });
    assert.equal(overlap.status, 200);
    await runtime.waitFor(() => (received.length === 3 ? true : undefined));
    assert.deepEqual(
      received[2]!.checks.map((check) => [check.kind, check.key]),
      [["hmac", "previous"]],
    );

    // A disabled deployment's endpoints answer 503 and store nothing.
    const { deployment } = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: codecs.ulid.decode(key.deploymentId),
    });
    await runtime.callAdminRpc("authDeploymentsDisable", {
      deploymentId: codecs.ulid.decode(key.deploymentId),
      expectedRevision: deployment.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await runtime.waitFor(async () => {
      const response = await post(body, { "test-signature": signed(body) });
      return response.status === 503 ? true : undefined;
    });
    const before = (await attempts("signed")).length;
    const disabled = await post(body, { "test-signature": signed(body) });
    assert.equal(disabled.status, 503);
    assert.equal(await disabled.text(), "");
    assert.equal((await attempts("signed")).length, before);
    await service.stop();
  });
});

test("an acknowledged async request still reaches the service after an approved change to sync mode", async () => {
  await withTrellisRuntime(async (runtime) => {
    // The signing secret is not set yet, so the worker keeps the request
    // pending, as it does in production until an administrator sets it.
    const key = await runtime.services.createInstance({
      name: "billing",
      deployment: "billing",
      contract: participant,
    });
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const endpoints = async () =>
      (
        await runtime.callAdminRpc("webhooksEndpointsList", {
          service: key.deploymentId,
        })
      ).endpoints;
    // Intake serves the endpoint once its directory sees the new deployment.
    const signed = await runtime.waitFor(async () =>
      (await endpoints()).find((endpoint) => endpoint.endpoint === "signed"),
    );
    assert.equal(signed.mode, "async");
    const body = '{"id":"evt_queued"}';
    const timestamp = Math.floor(Date.now() / 1000);
    const response = await fetch(signed.url, {
      method: "POST",
      body,
      headers: {
        "test-signature": `t=${timestamp},v1=${sign(`${timestamp}.${body}`)}`,
      },
    });
    assert.equal(response.status, 200);
    const attempts = async () =>
      (
        await runtime.callAdminRpc("webhooksAttemptsQuery", {
          service: key.deploymentId,
          endpoint: "signed",
        })
      ).attempts;
    const held = await runtime.waitFor(async () => {
      const [attempt] = await attempts();
      return attempt?.reason === "secret signing not set" ? attempt : undefined;
    });
    assert.equal(held.outcome, "pending");

    // The service's next build answers the endpoint in sync mode.
    const connecting = TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: laterParticipants.WebhookService.participant,
      seed: key.seed,
    }).orThrow();
    const update = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find(
        (item) =>
          item.deploymentId === key.deploymentId && item.state === "pending",
      );
    });
    await runtime.callAdminRpc("authServiceUpdatesDecide", {
      updateId: update.updateId,
      decision: "approved",
      expectedRevision: update.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await runtime.waitFor(async () =>
      (await endpoints()).some(
        (endpoint) =>
          endpoint.endpoint === "signed" && endpoint.mode === "sync",
      )
        ? true
        : undefined,
    );

    // Once the secret is set the queued request is verified and delivered.
    const { revision } = await runtime.callAdminRpc("authConfigGet", {
      target,
    });
    await runtime.callAdminRpc("authConfigSet", {
      target,
      secrets: { signing: SIGNING },
      ...(revision === undefined ? {} : { expectedRevision: revision }),
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    const service = await connecting;
    const received: WebhookDelivery[] = [];
    await service.webhooks.signed
      .onReceived(({ event }) => {
        received.push(event);
      })
      .orThrow();
    void service.wait();
    // The worker checks a request waiting for its secret every 10 seconds.
    await runtime.waitFor(() => (received.length === 1 ? true : undefined), {
      timeoutMs: 20_000,
    });
    assert.equal(new TextDecoder().decode(received[0]!.body), body);
    assert.equal(received[0]!.deliveryId, held.attemptId);
    assert.deepEqual(
      received[0]!.checks.map((check) => [check.kind, check.key]),
      [["hmac", "current"]],
    );
    const [attempt] = await attempts();
    assert.equal(attempt?.outcome, "accepted");
    await service.stop();
  });
});

test("token and Basic credentials are checked at intake, refused with 401, and never stored", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url, attempts } = await install(runtime);
    const received: WebhookDelivery[] = [];
    for (const name of ["token", "query", "partner"] as const) {
      await service.webhooks[name]
        .onReceived(({ event }) => {
          received.push(event);
        })
        .orThrow();
    }
    void service.wait();
    const inspect = (endpoint: string, attemptId: string) =>
      runtime.callAdminRpc("webhooksAttemptsInspect", {
        service: key.deploymentId,
        endpoint,
        attemptId: codecs.ulid.decode(attemptId),
      });
    const leaks = (value: unknown, secret: string) =>
      JSON.stringify(value, (_, item: unknown) =>
        typeof item === "bigint" ? item.toString() : item,
      ).includes(secret);

    const wrongHeader = await fetch(url("token"), {
      method: "POST",
      body: "{}",
      headers: { "x-hook-token": "wrong-token-value" },
    });
    assert.equal(wrongHeader.status, 401);
    const wrongQuery = await fetch(
      `${url("query")}?token=wrong-token-value&keep=1`,
      {
        method: "POST",
        body: "{}",
      },
    );
    assert.equal(wrongQuery.status, 401);
    const wrongBasic = await fetch(url("partner"), {
      method: "POST",
      body: "{}",
      headers: {
        authorization: `Basic ${btoa("partner:wrong-password-value")}`,
      },
    });
    assert.equal(wrongBasic.status, 401);
    for (const name of ["token", "query", "partner"]) {
      const [refused] = await attempts(name);
      assert.equal(refused?.outcome, "rejected");
      assert.equal(refused?.stored, false);
      const detail = await inspect(name, refused!.attemptId);
      assert.equal(detail.delivery, null);
      assert.equal(leaks(detail, "wrong-"), false);
    }

    // The right credentials get through, and are removed before anything is written.
    assert.equal(
      (
        await fetch(url("token"), {
          method: "POST",
          body: '{"n":1}',
          headers: { "x-hook-token": TOKEN },
        })
      ).status,
      200,
    );
    assert.equal(
      (
        await fetch(`${url("query")}?token=${TOKEN}&keep=1`, {
          method: "POST",
          body: '{"n":2}',
        })
      ).status,
      200,
    );
    assert.equal(
      (
        await fetch(url("partner"), {
          method: "POST",
          body: '{"n":3}',
          headers: { authorization: `Basic ${btoa(`partner:${PASSWORD}`)}` },
        })
      ).status,
      200,
    );
    await runtime.waitFor(() => (received.length === 3 ? true : undefined));
    for (const event of received) {
      assert.equal(leaks(event, TOKEN) || leaks(event, PASSWORD), false);
      assert.ok(
        !event.headers.some(
          (header) =>
            header.name === "x-hook-token" || header.name === "authorization",
        ),
      );
    }
    const queried = received.find((event) => event.endpoint === "query")!;
    assert.match(queried.url, /\?keep=1$/u);
    for (const name of ["token", "query"]) {
      const stored = (await attempts(name)).find((attempt) => attempt.stored);
      assert.ok(stored, `${name} kept the accepted request`);
      const detail = await inspect(name, stored.attemptId);
      assert.ok(detail.delivery);
      assert.equal(leaks(detail, TOKEN), false);
    }
    // A record opens only under the endpoint it belongs to.
    const tokenRecord = (await attempts("token")).find(
      (attempt) => attempt.stored,
    )!;
    await assert.rejects(
      inspect("query", tokenRecord.attemptId),
      (error) => error instanceof adminApis.webhooks.NotFoundError,
    );

    // The endpoint's request budget is its own runtime setting.
    for (const [setting, value] of [
      ["requestsPerSecond", 1n],
      ["burst", 1n],
    ] as const) {
      await runtime.callAdminRpc("authWebhooksUpdate", {
        deploymentId: codecs.ulid.decode(key.deploymentId),
        endpoint: "token",
        change: "set",
        setting,
        value,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
    }
    const limited = await runtime.waitFor(async () => {
      const statuses = [];
      for (let index = 0; index < 3; index++) {
        const response = await fetch(url("token"), {
          method: "POST",
          body: "{}",
          headers: { "x-hook-token": TOKEN },
        });
        statuses.push(response);
      }
      return statuses.find((response) => response.status === 429);
    });
    assert.equal(limited.headers.get("retry-after"), "1");
    await service.stop();
  });
});

test("a sync endpoint's decide handler answers a handshake with its exact body", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { service, url, attempts } = await install(runtime);
    const received: WebhookDelivery[] = [];
    await service.webhooks.handshake
      .onReceived(({ event }) => {
        received.push(event);
      })
      .orThrow();
    await service.webhooks.handshake
      .decide((request) => {
        const body = JSON.parse(
          new TextDecoder().decode(request.delivery.body),
        );
        if (body.type === "url_verification") {
          return {
            verdict: "ignore",
            response: webhookResponse({
              contentType: "text/plain",
              body: body.challenge,
            }),
          };
        }
        return { verdict: "accept", dedupeKey: body.event_id };
      })
      .orThrow();
    void service.wait();
    const post = (body: string, signature = sign(body)) =>
      fetch(url("handshake"), {
        method: "POST",
        body,
        headers: { "x-body-signature": `sha256=${signature}` },
      });

    const challenge = "3eZbrw1aBm2rZgRNFdxV2595E9CY3gmdALWMmHkvFXO7tYXAYM8P";
    const handshake = await post(
      JSON.stringify({ type: "url_verification", challenge }),
    );
    assert.equal(handshake.status, 200);
    assert.equal(await handshake.text(), challenge);
    assert.equal(handshake.headers.get("content-type"), "text/plain");
    assert.equal(
      handshake.headers.get("content-security-policy"),
      "sandbox; default-src 'none'",
    );

    const callback = await post(
      JSON.stringify({ type: "event_callback", event_id: "Ev1" }),
    );
    assert.equal(callback.status, 200);
    await runtime.waitFor(() => (received.length === 1 ? true : undefined));
    assert.equal(received[0]!.dedupeKey, "Ev1");

    // In sync mode a bad signature is refused before the handler sees it.
    const forged = await post("{}", sign("{}", "wrong-key"));
    assert.equal(forged.status, 401);
    const outcomes = (await attempts("handshake")).map(
      (attempt) => attempt.outcome,
    );
    assert.deepEqual(outcomes.sort(), ["accepted", "ignored", "rejected"]);
    await service.stop();
  });
});

test("a changed secret's overlap accepts only the secret it replaced, for every verifier, until that revision is deleted", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url, attempts } = await install(runtime);
    void service.wait();
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    // Two changes after the first secrets: the overlap is with the second.
    for (const round of ["second", "third"]) {
      const { revision } = await runtime.callAdminRpc("authConfigGet", {
        target,
      });
      await runtime.callAdminRpc("authConfigSet", {
        target,
        secrets: {
          signing: `${round}-signing`,
          hookToken: `${round}-token`,
          partnerPassword: `${round}-password`,
        },
        ...(revision === undefined ? {} : { expectedRevision: revision }),
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
    }
    const now = () => Math.floor(Date.now() / 1000);
    const send = {
      signed: (secret: string, id: string) => {
        const body = JSON.stringify({ id });
        const timestamp = now();
        return fetch(url("signed"), {
          method: "POST",
          body,
          headers: {
            "test-signature": `t=${timestamp},v1=${sign(`${timestamp}.${body}`, secret)}`,
          },
        });
      },
      token: (secret: string) =>
        fetch(url("token"), {
          method: "POST",
          body: "{}",
          headers: { "x-hook-token": secret },
        }),
      partner: (secret: string) =>
        fetch(url("partner"), {
          method: "POST",
          body: "{}",
          headers: { authorization: `Basic ${btoa(`partner:${secret}`)}` },
        }),
    };
    // The outcome of a signed request, once verified after the 2xx.
    const signedOutcome = async (secret: string) => {
      const id = ulid();
      assert.equal((await send.signed(secret, id)).status, 200);
      const sha = createHash("sha256")
        .update(JSON.stringify({ id }))
        .digest("hex");
      return await runtime.waitFor(async () => {
        const record = (await attempts("signed")).find(
          (attempt) => attempt.bodySha256 === sha,
        );
        return record && record.outcome !== "pending"
          ? [record.outcome, record.matchedKey]
          : undefined;
      });
    };
    const overlap = async () => {
      const { endpoints } = await runtime.callAdminRpc(
        "webhooksEndpointsList",
        { service: key.deploymentId },
      );
      const entries = endpoints.flatMap((endpoint) =>
        endpoint.secrets.map(
          (secret) =>
            [
              `${endpoint.endpoint}:${secret.name}`,
              secret.previousAcceptedUntil !== null,
            ] as const,
        ),
      );
      // Each secret is listed once per endpoint, even when several of its
      // checks read it.
      const names = entries.map(([name]) => name);
      assert.deepEqual(names, [...new Set(names)]);
      return Object.fromEntries(entries);
    };

    await runtime.waitFor(async () =>
      (await send.token("third-token")).status === 200 ? true : undefined,
    );
    // Every endpoint that reports an overlap honours it, with the secret the
    // newest change replaced and never an older one.
    assert.deepEqual(await overlap(), {
      "handshake:signing": true,
      "layered:hookToken": true,
      "layered:signing": true,
      "partner:partnerPassword": true,
      "query:hookToken": true,
      "receipt:signing": true,
      "relayed:relaySigning": false,
      "relayed:signing": true,
      "relayedToken:hookToken": true,
      "relayedToken:relaySigning": false,
      "relayedTokens:hookToken": true,
      "relayedTokens:relaySigning": false,
      "signed:signing": true,
      "token:hookToken": true,
    });
    assert.equal((await send.token("second-token")).status, 200);
    assert.equal((await send.token(TOKEN)).status, 401);
    assert.equal((await send.partner("second-password")).status, 200);
    assert.equal((await send.partner(PASSWORD)).status, 401);
    assert.deepEqual(await signedOutcome("second-signing"), [
      "accepted",
      "previous",
    ]);
    assert.deepEqual(await signedOutcome(SIGNING), ["rejected", null]);

    // Deleting the revision that holds the replaced secret ends the overlap
    // at once; the first secrets, in an older revision, stay refused.
    const { items } = await runtime.callAdminRpc("authConfigRevisionsList", {
      target,
    });
    const [, replaced] = items;
    await runtime.callAdminRpc("authConfigRevisionsDelete", {
      target,
      revision: replaced!.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await runtime.waitFor(async () =>
      (await send.token("second-token")).status === 401 ? true : undefined,
    );
    assert.equal((await send.token(TOKEN)).status, 401);
    assert.equal((await send.partner(PASSWORD)).status, 401);
    assert.deepEqual(await signedOutcome(SIGNING), ["rejected", null]);
    assert.equal((await send.partner("second-password")).status, 401);
    assert.deepEqual(await signedOutcome("second-signing"), ["rejected", null]);
    assert.ok(Object.values(await overlap()).every((reported) => !reported));
    assert.equal((await send.partner("third-password")).status, 200);
    assert.deepEqual(await signedOutcome("third-signing"), [
      "accepted",
      "current",
    ]);
    await service.stop();
  });
});

test("each secret reports when its own current and previous values last matched, since its newest change", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url } = await install(runtime);
    void service.wait();
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const changeSigning = async (signing: string) => {
      const { revision } = await runtime.callAdminRpc("authConfigGet", {
        target,
      });
      await runtime.callAdminRpc("authConfigSet", {
        target,
        secrets: { signing },
        ...(revision === undefined ? {} : { expectedRevision: revision }),
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
    };
    // One request carrying both credentials, and the time window it was
    // received in.
    const send = async (token: string, signing: string) => {
      const body = JSON.stringify({ id: ulid() });
      const before = Date.now();
      const response = await fetch(url("layered"), {
        method: "POST",
        body,
        headers: {
          "x-hook-token": token,
          "x-body-signature": `sha256=${sign(body, signing)}`,
        },
      });
      const window = { status: response.status, before, after: Date.now() };
      await response.body?.cancel();
      return window;
    };
    const reported = async () => {
      const { endpoints } = await runtime.callAdminRpc(
        "webhooksEndpointsList",
        { service: key.deploymentId },
      );
      const layered = endpoints.find((entry) => entry.endpoint === "layered")!;
      const time = (value: unknown) =>
        value === null ? null : Date.parse(String(value));
      return Object.fromEntries(
        layered.secrets.map((secret) => [
          secret.name,
          {
            current: time(secret.currentLastMatchedAt),
            previous: time(secret.previousLastMatchedAt),
          },
        ]),
      );
    };
    const within = (
      time: number | null,
      window: { before: number; after: number },
    ) => {
      assert.ok(
        time !== null && time >= window.before && time <= window.after,
        `${time} is within ${window.before}..${window.after}`,
      );
    };

    // Nothing has matched yet.
    assert.deepEqual(await reported(), {
      hookToken: { current: null, previous: null },
      signing: { current: null, previous: null },
    });
    // Intake follows settings as they change; wait until it holds them.
    const first = await runtime.waitFor(async () => {
      const window = await send(TOKEN, SIGNING);
      return window.status === 200 ? window : undefined;
    });
    let now = await reported();
    within(now.hookToken!.current, first);
    within(now.signing!.current, first);
    assert.equal(now.hookToken!.previous, null);
    assert.equal(now.signing!.previous, null);

    // Changing the signing secret starts its record afresh; the token keeps
    // its own.
    await changeSigning("second-signing");
    now = await runtime.waitFor(async () => {
      const status = await reported();
      return status.signing!.current === null ? status : undefined;
    });
    within(now.hookToken!.current, first);
    assert.equal(now.signing!.previous, null);
    // A request with the replaced signing secret is that secret's previous
    // use, and never the token's.
    const old = await send(TOKEN, SIGNING);
    assert.equal(old.status, 200);
    now = await reported();
    within(now.signing!.previous, old);
    assert.equal(now.signing!.current, null);
    within(now.hookToken!.current, old);
    assert.equal(now.hookToken!.previous, null);

    // A second change: the previous value is now the second secret, which no
    // request has used, so the earlier previous use does not carry over.
    await changeSigning("third-signing");
    now = await runtime.waitFor(async () => {
      const status = await reported();
      return status.signing!.previous === null ? status : undefined;
    });
    assert.equal(now.signing!.current, null);
    const second = await send(TOKEN, "second-signing");
    assert.equal(second.status, 200);
    const third = await send(TOKEN, "third-signing");
    assert.equal(third.status, 200);
    now = await reported();
    within(now.signing!.previous, second);
    within(now.signing!.current, third);
    within(now.hookToken!.current, third);
    assert.equal(now.hookToken!.previous, null);
    await service.stop();
  });
});

test("a held request that arrived before its secret changed is no use of the value the change replaced, even within the same second", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url, attempts } = await install(runtime);
    const received: WebhookDelivery[] = [];
    await service.webhooks.relayed
      .onReceived(({ event }) => {
        received.push(event);
      })
      .orThrow();
    void service.wait();
    const reported = async () => {
      const { endpoints } = await runtime.callAdminRpc(
        "webhooksEndpointsList",
        { service: key.deploymentId },
      );
      const signing = endpoints
        .find((endpoint) => endpoint.endpoint === "relayed")!
        .secrets.find((secret) => secret.name === "signing")!;
      return {
        current: signing.currentLastMatchedAt,
        previous: signing.previousLastMatchedAt,
        overlap: signing.previousAcceptedUntil !== null,
      };
    };
    const verified = (body: string) => {
      const sha = createHash("sha256").update(body).digest("hex");
      return runtime.waitFor(
        async () => {
          const record = (await attempts("relayed")).find(
            (attempt) => attempt.bodySha256 === sha,
          );
          return record && record.outcome !== "pending" ? record : undefined;
        },
        // The worker checks a request waiting for its secret every 10 seconds.
        { timeoutMs: 20_000 },
      );
    };
    const signed = (body: string) => ({
      "x-body-signature": `sha256=${sign(body)}`,
    });

    // The relay's secret is not set, so the worker holds the request. It
    // arrives at the start of a second and the signing secret changes in
    // that same second, so receive and change times cannot order them.
    await new Promise((resolve) =>
      setTimeout(resolve, 1000 - (Date.now() % 1000)),
    );
    const held = JSON.stringify({ id: ulid() });
    assert.equal((await relay(url("relayed"), held, signed(held))).status, 200);
    await configure(runtime, key.deploymentId, {
      secrets: { signing: "second-signing" },
    });
    await runtime.waitFor(async () => (await reported()).overlap || undefined);
    await configure(runtime, key.deploymentId, {
      secrets: { relaySigning: RELAY },
    });
    assert.equal((await verified(held)).outcome, "accepted");
    // It matched the replaced secret, which it used while that was current.
    await runtime.waitFor(() => received.length === 1);
    assert.deepEqual(
      received[0]!.checks.map((check) => [check.kind, check.key]),
      [
        ["address", null],
        ["hmac", "current"],
        ["hmac", "previous"],
      ],
    );
    assert.deepEqual(await reported(), {
      current: null,
      previous: null,
      overlap: true,
    });

    // A request that arrives after the change with the replaced secret is
    // that change's previous use.
    const late = JSON.stringify({ id: ulid() });
    const before = Date.now();
    assert.equal((await relay(url("relayed"), late, signed(late))).status, 200);
    assert.equal((await verified(late)).outcome, "accepted");
    const now = await reported();
    assert.equal(now.current, null);
    assert.ok(
      now.previous !== null && Date.parse(String(now.previous)) >= before,
      `${now.previous} is after ${before}`,
    );
    await service.stop();
  });
});

test("each change of a secret is a generation of its own, and deleting revisions never brings back an older value", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url } = await install(runtime);
    void service.wait();
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const token = async (secret: string) => {
      const response = await fetch(url("token"), {
        method: "POST",
        body: "{}",
        headers: { "x-hook-token": secret },
      });
      await response.body?.cancel();
      return response.status;
    };
    const reported = async () => {
      const { endpoints } = await runtime.callAdminRpc(
        "webhooksEndpointsList",
        { service: key.deploymentId },
      );
      const secret = (endpoint: string, name: string) =>
        endpoints
          .find((entry) => entry.endpoint === endpoint)!
          .secrets.find((entry) => entry.name === name)!;
      const hookToken = secret("token", "hookToken");
      return {
        current: hookToken.currentLastMatchedAt !== null,
        previous: hookToken.previousLastMatchedAt !== null,
        overlap: hookToken.previousAcceptedUntil !== null,
        signingOverlap:
          secret("signed", "signing").previousAcceptedUntil !== null,
      };
    };

    // Revision 2 sets a token that a request then uses.
    await configure(runtime, key.deploymentId, {
      secrets: { hookToken: "x-token" },
    });
    await runtime.waitFor(async () => (await token("x-token")) === 200);
    assert.equal((await reported()).current, true);
    // Revision 4 sets that same value again, and revision 5 changes only
    // another secret.
    await configure(runtime, key.deploymentId, {
      secrets: { hookToken: "y-token" },
    });
    await configure(runtime, key.deploymentId, {
      secrets: { hookToken: "x-token" },
    });
    await configure(runtime, key.deploymentId, {
      secrets: { signing: "other-signing" },
    });
    // The equal value is a new generation: nothing has used it, and the
    // value it replaced is accepted.
    await runtime.waitFor(async () => (await reported()).signingOverlap);
    assert.deepEqual(await reported(), {
      current: false,
      previous: false,
      overlap: true,
      signingOverlap: true,
    });
    assert.equal(await token("y-token"), 200);
    assert.equal(await token(TOKEN), 401);

    // Deleting revisions 3 and 4 ends the token's overlap, and the signing
    // secret's too, since revision 4 also held the signing secret revision 5
    // replaced. The first token, which revision 2 replaced, stays refused,
    // and the first generation's use is not reported for the current one.
    const { items } = await runtime.callAdminRpc("authConfigRevisionsList", {
      target,
    });
    for (const deleted of [items[1]!, items[2]!]) {
      await runtime.callAdminRpc("authConfigRevisionsDelete", {
        target,
        revision: deleted.revision,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
    }
    await runtime.waitFor(async () => (await token("y-token")) === 401);
    assert.equal(await token(TOKEN), 401);
    assert.deepEqual(await reported(), {
      current: false,
      previous: true,
      overlap: false,
      signingOverlap: false,
    });
    assert.equal(await token("x-token"), 200);
    assert.equal((await reported()).current, true);
    await service.stop();
  });
});

test("a queued request keeps its token check under the policy it was accepted with, past the overlap's end, until the matched value's revision is deleted", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url, attempts } = await install(runtime);
    void service.wait();
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const record = async (body: string) => {
      const sha = createHash("sha256").update(body).digest("hex");
      return (await attempts("relayedToken")).find(
        (attempt) => attempt.bodySha256 === sha,
      );
    };
    const verified = (body: string) =>
      runtime.waitFor(
        async () => {
          const found = await record(body);
          return found && found.outcome !== "pending" ? found : undefined;
        },
        { timeoutMs: 20_000 },
      );
    const send = async (token: string) => {
      const body = JSON.stringify({ id: ulid() });
      const response = await relay(url("relayedToken"), body, {
        "x-hook-token": token,
      });
      assert.equal(response.status, 200);
      return body;
    };
    const report = await shortOverlap(
      runtime,
      key.deploymentId,
      "relayedToken",
    );
    const held = (body: string) =>
      runtime.waitFor(
        async () =>
          (await record(body))?.reason === "secret relaySigning not set",
      );
    const relaySet = async (isSet: boolean) => {
      await configure(
        runtime,
        key.deploymentId,
        isSet
          ? { secrets: { relaySigning: RELAY } }
          : { unset: ["relaySigning"] },
      );
      await runtime.waitFor(
        async () => (await report("relaySigning")).isSet === isSet,
      );
    };
    const changeToken = async (hookToken: string) => {
      await configure(runtime, key.deploymentId, { secrets: { hookToken } });
      await runtime.waitFor(async () => (await report("hookToken")).overlap);
    };
    const overlapOver = () =>
      runtime.waitFor(async () => !(await report("hookToken")).overlap);

    // Received with the replaced token during the overlap and held until
    // after it ends: the match stands.
    await changeToken("next-token");
    const late = await send(TOKEN);
    await held(late);
    await overlapOver();
    await relaySet(true);
    assert.equal((await verified(late)).outcome, "accepted");

    // The same, but the revision holding the replaced token is deleted
    // before the request is processed: it is refused.
    await relaySet(false);
    await changeToken("third-token");
    const revoked = await send("next-token");
    await held(revoked);
    const { items } = await runtime.callAdminRpc("authConfigRevisionsList", {
      target,
    });
    // Newest first: the change to third-token, then the revision before it,
    // which holds next-token.
    await runtime.callAdminRpc("authConfigRevisionsDelete", {
      target,
      revision: items[1]!.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await overlapOver();
    await relaySet(true);
    const refused = await verified(revoked);
    assert.deepEqual(
      [refused.outcome, refused.reason],
      [
        "rejected",
        "check 2 (Token in the x-hook-token header) matched the previous value of secret hookToken, which changed after intake and no longer accepts it; the credential check can't be rerun",
      ],
    );

    // Held until the relay's secret is set, which comes with new relay
    // addresses: its token was checked under the old ones.
    await relaySet(false);
    const queued = await send("third-token");
    await held(queued);
    await configure(runtime, key.deploymentId, {
      values: { relayRanges: [...RELAY_RANGES, "192.0.2.0/24"] },
      secrets: { relaySigning: RELAY },
    });
    const changed = await verified(queued);
    assert.deepEqual(
      [changed.outcome, changed.reason],
      [
        "rejected",
        "verifier policy changed after intake; credential checks can't be rerun",
      ],
    );
    await service.stop();
  });
});

test("a queued request whose two checks of one secret matched different values is refused once either value's revision is deleted", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url, attempts } = await install(runtime);
    const received: WebhookDelivery[] = [];
    await service.webhooks.relayedTokens
      .onReceived(({ event }) => {
        received.push(event);
      })
      .orThrow();
    void service.wait();
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const record = async (body: string) => {
      const sha = createHash("sha256").update(body).digest("hex");
      return (await attempts("relayedTokens")).find(
        (attempt) => attempt.bodySha256 === sha,
      );
    };
    const verified = (body: string) =>
      runtime.waitFor(
        async () => {
          const found = await record(body);
          return found && found.outcome !== "pending" ? found : undefined;
        },
        { timeoutMs: 20_000 },
      );
    // The header check runs first, the query check second.
    const send = async (header: string, query: string) => {
      const body = JSON.stringify({ id: ulid() });
      const response = await relay(
        `${url("relayedTokens")}?token=${encodeURIComponent(query)}`,
        body,
        { "x-hook-token": header },
      );
      assert.equal(response.status, 200);
      return body;
    };
    const report = await shortOverlap(
      runtime,
      key.deploymentId,
      "relayedTokens",
    );
    const held = (body: string) =>
      runtime.waitFor(
        async () =>
          (await record(body))?.reason === "secret relaySigning not set",
      );
    const relaySet = async (isSet: boolean) => {
      await configure(
        runtime,
        key.deploymentId,
        isSet
          ? { secrets: { relaySigning: RELAY } }
          : { unset: ["relaySigning"] },
      );
      await runtime.waitFor(
        async () => (await report("relaySigning")).isSet === isSet,
      );
    };
    const changeToken = async (hookToken: string) => {
      await configure(runtime, key.deploymentId, { secrets: { hookToken } });
      await runtime.waitFor(async () => (await report("hookToken")).overlap);
    };
    const overlapOver = () =>
      runtime.waitFor(async () => !(await report("hookToken")).overlap);

    // The header carries the replaced token and the query the new one;
    // both values are still held when the request is processed, so it is
    // delivered.
    await changeToken("next-token");
    const kept = await send(TOKEN, "next-token");
    await held(kept);
    await overlapOver();
    await relaySet(true);
    assert.equal((await verified(kept)).outcome, "accepted");
    await runtime.waitFor(() => (received.length === 1 ? true : undefined));
    assert.equal(new TextDecoder().decode(received[0]!.body), kept);
    assert.deepEqual(
      received[0]!.checks.map((check) => [check.kind, check.key]),
      [
        ["token", "previous"],
        ["token", "current"],
        ["hmac", "current"],
      ],
    );

    // The same, but the revision holding the token the header matched is
    // deleted before processing. The query's later match of the current
    // token does not stand in for it: the request is refused, naming the
    // header check.
    await relaySet(false);
    await changeToken("third-token");
    const revoked = await send("next-token", "third-token");
    await held(revoked);
    const { items } = await runtime.callAdminRpc("authConfigRevisionsList", {
      target,
    });
    // Newest first: the change to third-token, then the revision before it,
    // which holds next-token.
    await runtime.callAdminRpc("authConfigRevisionsDelete", {
      target,
      revision: items[1]!.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await overlapOver();
    await relaySet(true);
    const refused = await verified(revoked);
    assert.deepEqual(
      [refused.outcome, refused.reason],
      [
        "rejected",
        "check 1 (Token in the x-hook-token header) matched the previous value of secret hookToken, which changed after intake and no longer accepts it; the credential check can't be rerun",
      ],
    );
    assert.equal(received.length, 1);
    await service.stop();
  });
});

test("a queued signed request keeps a replaced secret matched during its overlap until that revision is deleted, and is verified under the policy in force", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { key, service, url, attempts } = await install(runtime);
    void service.wait();
    const record = async (body: string) => {
      const sha = createHash("sha256").update(body).digest("hex");
      return (await attempts("relayed")).find(
        (attempt) => attempt.bodySha256 === sha,
      );
    };
    const send = async (signing: string) => {
      const body = JSON.stringify({ id: ulid() });
      const response = await relay(url("relayed"), body, {
        "x-body-signature": `sha256=${sign(body, signing)}`,
      });
      assert.equal(response.status, 200);
      return body;
    };

    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const report = await shortOverlap(runtime, key.deploymentId, "relayed");
    const verified = (body: string) =>
      runtime.waitFor(
        async () => {
          const found = await record(body);
          return found && found.outcome !== "pending" ? found : undefined;
        },
        { timeoutMs: 20_000 },
      );
    const held = (body: string) =>
      runtime.waitFor(
        async () =>
          (await record(body))?.reason === "secret relaySigning not set",
      );
    const relaySet = async (isSet: boolean) => {
      await configure(
        runtime,
        key.deploymentId,
        isSet
          ? { secrets: { relaySigning: RELAY } }
          : { unset: ["relaySigning"] },
      );
      await runtime.waitFor(
        async () => (await report("relaySigning")).isSet === isSet,
      );
    };
    const changeSigning = async (signing: string) => {
      await configure(runtime, key.deploymentId, { secrets: { signing } });
      await runtime.waitFor(async () => (await report("signing")).overlap);
    };
    const overlapOver = () =>
      runtime.waitFor(async () => !(await report("signing")).overlap);

    // Signed with the replaced secret, received during the overlap, and
    // held until after it ends: it is accepted.
    await changeSigning("second-signing");
    const late = await send(SIGNING);
    await held(late);
    await overlapOver();
    await relaySet(true);
    const honoured = await verified(late);
    assert.deepEqual([honoured.outcome, honoured.reason], ["accepted", null]);

    // The same, but the revision holding the replaced secret is deleted
    // before the request is processed: it is refused.
    await relaySet(false);
    await changeSigning("third-signing");
    const revoked = await send("second-signing");
    await held(revoked);
    const { items } = await runtime.callAdminRpc("authConfigRevisionsList", {
      target,
    });
    // Newest first: the change to third-signing, then the revision before
    // it, which holds second-signing.
    await runtime.callAdminRpc("authConfigRevisionsDelete", {
      target,
      revision: items[1]!.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await overlapOver();
    await relaySet(true);
    const refused = await verified(revoked);
    assert.deepEqual(
      [refused.outcome, refused.reason],
      ["rejected", "signature does not match"],
    );

    // Both are held until the relay's secret is set, which comes with new
    // relay addresses and a new signing secret.
    await relaySet(false);
    const renewed = await send("next-signing");
    const forged = await send("forged-signing");
    await runtime.waitFor(async () => {
      const held = [await record(renewed), await record(forged)];
      return held.every(
        (found) => found?.reason === "secret relaySigning not set",
      );
    });
    await configure(runtime, key.deploymentId, {
      values: { relayRanges: [...RELAY_RANGES, "192.0.2.0/24"] },
      secrets: { signing: "next-signing", relaySigning: RELAY },
    });
    const outcomes = await runtime.waitFor(
      async () => {
        const found = [await record(renewed), await record(forged)];
        return found.every((entry) => entry && entry.outcome !== "pending")
          ? found.map((entry) => [entry!.outcome, entry!.reason])
          : undefined;
      },
      { timeoutMs: 20_000 },
    );
    assert.deepEqual(outcomes, [
      ["accepted", null],
      ["rejected", "signature does not match"],
    ]);

    // Once the relay addresses no longer include the sender, verifying the
    // accepted request again refuses it.
    await configure(runtime, key.deploymentId, {
      values: { relayRanges: ["192.0.2.0/24"] },
    });
    const accepted = (await record(renewed))!;
    const excluded = await runtime.waitFor(async () => {
      const { attempt } = await runtime.callAdminRpc(
        "webhooksAttemptsReverify",
        {
          service: key.deploymentId,
          endpoint: "relayed",
          attemptId: codecs.ulid.decode(accepted.attemptId),
        },
      );
      return attempt.outcome === "rejected" ? attempt : undefined;
    });
    assert.equal(excluded.reason, "client address outside the allowed ranges");
    await service.stop();
  });
});

test("requests with the same delivery ID that arrive together publish one event", async () => {
  await withTrellisRuntime(async (runtime) => {
    const { service, url, attempts } = await install(runtime);
    const received: WebhookDelivery[] = [];
    await service.webhooks.receipt
      .onReceived(({ event }) => {
        received.push(event);
      })
      .orThrow();
    void service.wait();
    const post = (body: string, delivery: string) =>
      fetch(url("receipt"), {
        method: "POST",
        body,
        headers: {
          "x-body-signature": `sha256=${sign(body)}`,
          "x-delivery-id": delivery,
        },
      });

    // A sender retrying in parallel: every copy passes verification at once.
    const copies = await Promise.all(
      Array.from({ length: 8 }, () => post('{"n":1}', "delivery-1")),
    );
    assert.deepEqual(
      copies.map((answer) => answer.status),
      Array(8).fill(200),
    );
    const outcomes = (await attempts("receipt")).map(
      (attempt) => attempt.outcome,
    );
    assert.deepEqual(outcomes.sort(), [
      "accepted",
      ...Array(7).fill("duplicate"),
    ]);
    // Events arrive in order, so once a later one is here any second event
    // for delivery-1 would be too.
    assert.equal((await post('{"n":2}', "delivery-2")).status, 200);
    await runtime.waitFor(() =>
      received.some((event) => event.dedupeKey === "delivery-2")
        ? true
        : undefined,
    );
    assert.deepEqual(
      received.map((event) => event.dedupeKey),
      ["delivery-1", "delivery-2"],
    );
    await service.stop();
  });
});
