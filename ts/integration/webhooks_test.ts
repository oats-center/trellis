/**
 * Live acceptance for receiving webhooks: a service declares endpoints in its
 * IDL, the test sets their secrets through the ordinary settings RPCs, and
 * real HTTPS requests reach the endpoint URLs Trellis serves. Each scenario
 * checks what the sender gets back, what Trellis records, and what reaches
 * the service's handlers.
 */

import { createHmac } from "node:crypto";
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
import { withTrellisRuntime } from "./_support/runtime.ts";

const participant = participants.WebhookService.participant;
type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

const SIGNING = "whsec_signing-secret";
const TOKEN = "hook-token-value";
const PASSWORD = "partner-password-value";

function sign(text: string, key = SIGNING): string {
  return createHmac("sha256", key).update(text).digest("hex");
}

async function install(runtime: Runtime) {
  const key = await runtime.services.createInstance({
    name: "billing",
    deployment: "billing",
    contract: participant,
    config: {
      values: { partnerUser: "partner" },
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
    const reverified = await runtime.waitFor(async () => {
      const { attempt } = await runtime.callAdminRpc(
        "webhooksAttemptsReverify",
        {
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
    const inspect = (attemptId: string) =>
      runtime.callAdminRpc("webhooksAttemptsInspect", {
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
      const detail = await inspect(refused!.attemptId);
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
      const detail = await inspect(stored.attemptId);
      assert.ok(detail.delivery);
      assert.equal(leaks(detail, TOKEN), false);
    }

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
