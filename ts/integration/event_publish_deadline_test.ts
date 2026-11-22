/**
 * An event whose signing outlasts the caller's deadline is never published.
 *
 * Signing an event authorizes it against the API's catalog, which a session
 * reads from Auth's material bucket the first time it needs it. A service
 * that publishes right after it connects can need that catalog before its
 * background warm-up has read it, and on a slow network the read can take
 * the whole budget. The native proxy withholds the real reply to that read,
 * so signing ends only after the deadline has passed.
 */
import { credsAuthenticator } from "@nats-io/nats-core";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect } from "@nats-io/transport-node";
import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { isErr } from "@oatscenter/result";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const API = "localhost/runtime-trellis.events@v1";
const SUBJECT =
  "events.v1.bG9jYWxob3N0L3J1bnRpbWUtdHJlbGxpcy5ldmVudHNAdjE.Alpha";
const TIMEOUT_MS = 1_000;

test("an event signed after its deadline is not published and the caller is told it timed out", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.EventService.participant;
      const identity = await runtime.registerService({
        name: "publish-deadline",
        contract,
      });
      const observer = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await runtime.natsCredentials("trellis"),
        ),
      });
      // Withhold the first read of the events API's catalog.
      const hold = runtime
        .nativeTransportGate()
        .armResponseHold(
          "$JS.API.STREAM.MSG.GET.KV_TRELLIS_AUTH_MATERIAL",
          undefined,
          (body) => {
            const reply: { message?: { subject?: string; data?: string } } =
              JSON.parse(new TextDecoder().decode(body));
            const { subject, data } = reply.message ?? {};
            if (!subject?.startsWith("$KV.TRELLIS_AUTH_MATERIAL.api.") || !data)
              return false;
            const catalog: { apiId?: unknown } = JSON.parse(
              Buffer.from(data, "base64").toString("utf8"),
            );
            return catalog.apiId === API;
          },
        );
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
        runtime: { timeout: TIMEOUT_MS },
      }).orThrow();
      try {
        const manager = await jetstreamManager(observer);
        const stream = (await manager.streams.list().next()).find((entry) =>
          entry.config.subjects?.some((filter) => filter.startsWith("events.")),
        );
        assert(stream, "event stream missing");
        const before = (await manager.streams.info(stream.config.name)).state
          .last_seq;

        const publishing = service
          .publishAlpha({ site: "deadline", value: "late" })
          .take();
        await Promise.race([
          hold.held,
          delay(10_000).then(() => {
            throw new Error("the catalog read was not withheld");
          }),
        ]);
        // The publish settles within its budget, not when the read returns.
        const settled = await Promise.race([
          publishing.then(() => true),
          delay(TIMEOUT_MS * 3).then(() => false),
        ]);
        await hold.release();
        const outcome = await publishing;

        // The same session publishes once the catalog has arrived. Messages
        // on one connection arrive in order, so a late send of the first
        // event would sit in the stream before this one.
        await service
          .publishAlpha({ site: "deadline", value: "on time" })
          .orThrow();
        const values: string[] = [];
        const last = (await manager.streams.info(stream.config.name)).state
          .last_seq;
        for (let seq = before + 1; seq <= last; seq++) {
          const message = await manager.streams.getMessage(stream.config.name, {
            seq,
          });
          if (message?.subject === SUBJECT)
            values.push(String(message.json<{ value: string }>().value));
        }
        assert.deepEqual(values, ["on time"]);
        assert(settled, "the publish outlived its budget");
        assert(isErr(outcome), "the late event was reported published");
        assert.equal(
          Reflect.get(outcome.error, "code"),
          "trellis.request.timeout",
        );
      } finally {
        await hold.release();
        await service.stop();
        await observer.close();
      }
    },
    { interruptibleNativeProxy: true },
  );
});
