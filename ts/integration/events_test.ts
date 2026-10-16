import { credsAuthenticator } from "@nats-io/nats-core";
import { jetstream, jetstreamManager } from "@nats-io/jetstream";
import { connect } from "@nats-io/transport-node";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { env, execPath } from "node:process";
import { Readable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { codecs } from "@oatscenter/trellis/generated";
import { ulid } from "ulid";
import { AuthError, Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { apis as webApis } from "../../web/trellis/index.js";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import { spawnTracked } from "./_support/process.ts";

test("Auth connection lifecycle events survive the fenced outbox", async () => {
  await withTrellisRuntime(async (runtime) => {
    let phase = "initial-lifecycle";
    const admin = await runtime.connectClient({
      name: "auth-events-admin",
      contract: adminParticipant,
    });
    await runtime.registerService({
      name: "auth-events-provider",
      contract: participants.EventService.participant,
    });
    const qualified = "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections";
    const lifecycleRows = async (
      attachmentId: string,
      name: "Opened" | "Closed" | "Kicked",
    ) => {
      const query = await admin.eventsQuery({
        subject: `${qualified}.${name}`,
      });
      if (query.isErr()) {
        console.error(
          JSON.stringify({
            event: "auth-lifecycle-query-failed",
            phase,
            attachmentId,
            name,
            error: query.error.toSerializable(),
          }),
        );
      }
      const rows = query.orThrow().items;
      const matching = [];
      for (const row of rows) {
        const detail = (
          await admin.eventsInspect({ eventId: row.eventId }).orThrow()
        ).event;
        const payload = webApis.auth.API.actions[
          `event:Connections.${name}`
        ].payload.decode(JSON.parse(new TextDecoder().decode(detail.payload)));
        if (payload.attachmentId === attachmentId) {
          matching.push({ row, detail, payload });
        }
      }
      return matching;
    };
    const waitForEvent = async (
      attachmentId: string,
      name: "Opened" | "Closed" | "Kicked",
    ) => {
      const { detail, payload } = await runtime.waitFor(
        async () => {
          const rows = await lifecycleRows(attachmentId, name);
          return rows.length === 1 ? rows[0] : undefined;
        },
        { timeoutMs: 30_000, intervalMs: 100 },
      );
      assert.deepEqual(detail.row.subject, `${qualified}.${name}`);
      assert.deepEqual(payload.attachmentId, attachmentId);
      return payload;
    };
    const connectionFor = async (clientId: string) =>
      await runtime.waitFor(async () => {
        const sessions = (
          await runtime.callAdminRpc("authAuthorizationSessionsList", {
            clientId,
            state: "active",
          })
        ).items;
        for (const session of sessions) {
          const connection = (
            await runtime.callAdminRpc("authConnectionsList", {
              authorizationSessionId: session.authorizationSessionId,
              state: "admitted",
            })
          ).items[0];
          if (connection) {
            return {
              ...connection,
              loginSessionId: session.loginSessionId,
              principalId: session.principalId,
            };
          }
        }
        return false;
      });

    const openedClient = await runtime.connectClient({
      name: "auth-events-open-close",
      contract: apps.Alpha.request,
    });
    const opened = await connectionFor(apps.Alpha.request.id);
    const openedPayload = await waitForEvent(opened.attachmentId, "Opened");
    assert.deepEqual(
      openedPayload.authorizationSessionId,
      opened.authorizationSessionId,
    );
    assert.deepEqual(openedPayload.principalId, opened.principalId);
    assert.deepEqual(
      await lifecycleRows(opened.attachmentId, "Opened").then((r) => r.length),
      1,
    );

    await openedClient.connection.close();
    await waitForEvent(opened.attachmentId, "Closed");
    assert.deepEqual(
      await lifecycleRows(opened.attachmentId, "Closed").then((r) => r.length),
      1,
    );

    const kickedClient = await runtime.connectClient({
      name: "auth-events-kick",
      contract: apps.Beta.request,
    });
    const kicked = await connectionFor(apps.Beta.request.id);
    const session = (
      await runtime.callAdminRpc("authSessionsList", {
        participantId: apps.Beta.request.id,
        state: "active",
      })
    ).items.find((entry) => entry.loginSessionId === kicked.loginSessionId);
    if (!session) throw new Error("kick session missing");
    const revoke = {
      loginSessionId: session.loginSessionId,
      idempotencyKey: ulid(),
    };
    await runtime.callAdminRpc("authSessionsRevoke", revoke);
    await runtime.callAdminRpc("authSessionsRevoke", revoke);
    await waitForEvent(kicked.attachmentId, "Kicked");
    assert.deepEqual(
      await lifecycleRows(kicked.attachmentId, "Kicked").then((r) => r.length),
      1,
    );
    await kickedClient.connection.close().catch(() => undefined);

    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await readFile(
          join(runtime.workdir, "config/trellis/nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    try {
      const manager = await jetstreamManager(nats);
      const eventStream = (await manager.streams.list().next()).find((stream) =>
        stream.config.subjects?.some((subject) =>
          subject.startsWith("events."),
        ),
      );
      if (!eventStream) throw new Error("event stream missing");
      const streamConfig = eventStream.config;
      phase = "outbox-publication-blocked";
      await manager.streams.update(streamConfig.name, {
        ...streamConfig,
        max_msg_size: 1,
      });
      const recoveryClient = await runtime.connectClient({
        name: "auth-events-recovery",
        contract: apps.Alpha.request,
      });
      const recovery = await connectionFor(apps.Alpha.request.id);
      assert.deepEqual(
        (await lifecycleRows(recovery.attachmentId, "Opened")).length,
        0,
      );
      await manager.streams.update(streamConfig.name, streamConfig);
      phase = "host-restarting";
      await runtime.restart();
      phase = "outbox-replay-after-restart";
      await waitForEvent(recovery.attachmentId, "Opened");
      assert.deepEqual(
        await lifecycleRows(recovery.attachmentId, "Opened").then(
          (r) => r.length,
        ),
        1,
      );
      assert.deepEqual(
        new Set([
          opened.attachmentId,
          kicked.attachmentId,
          recovery.attachmentId,
        ]).size,
        3,
      );
      await recoveryClient.connection.close();
    } finally {
      await nats.close();
    }
  });
});

test("runtime Events transports Health.StatusChanged on its qualified subject", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "health-events",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      seed: identity.seed,
      runtime: { health: { publishIntervalMs: 1000 } },
    }).orThrow();
    let healthy = true;
    service.health.add("acceptance", () => ({
      status: healthy ? "ok" : "failed",
    }));
    const exit = service.wait().catch((error: unknown) => error);
    const reader = await runtime.connectClient({
      name: "health-event-reader",
      contract: adminParticipant,
    });
    try {
      const descriptor = webApis.health.API.actions["event:StatusChanged"];
      // The first sample initializes Health; only subsequent transitions emit
      // StatusChanged. Wait for the initial projection before changing it.
      await runtime.waitFor(async () =>
        (await reader.healthQuery({}).orThrow()).items.some(
          (item) =>
            item.contractId === participants.Provider.participant.id &&
            item.effectiveStatus === "healthy",
        ),
      );
      for (const status of ["degraded", "healthy"] as const) {
        healthy = status === "healthy";
        await runtime.waitFor(async () => {
          const rows = await reader
            .eventsQuery({ subject: descriptor.subject })
            .orThrow();
          const observed = [];
          for (const row of rows.items) {
            const detail = await reader
              .eventsInspect({ eventId: row.eventId })
              .orThrow();
            const payload = descriptor.payload.decode(
              JSON.parse(new TextDecoder().decode(detail.event.payload)),
            );
            observed.push({
              instanceId: payload.participant.instanceId,
              status: payload.status,
            });
            if (
              payload.participant.instanceId === identity.instanceId &&
              payload.status === status
            )
              return payload;
          }
          throw new Error(
            JSON.stringify(
              {
                workdir: runtime.workdir,
                expected: { instanceId: identity.instanceId, status },
                health: (await reader.healthQuery({}).orThrow()).items,
                events: observed,
              },
              (_key, value: unknown) =>
                typeof value === "bigint" ? value.toString() : value,
            ),
          );
        });
      }
    } finally {
      await service.stop();
      await exit;
      await reader.connection.close();
    }
  });
});

test("Events projector retains rejected unsafe times and advances", async () => {
  await withTrellisRuntime(async (runtime) => {
    const admin = await runtime.connectClient({
      name: "unsafe-event-time-admin",
      contract: adminParticipant,
    });
    await runtime.registerService({
      name: "unsafe-event-time-provider",
      contract: participants.EventService.participant,
    });
    const alpha = await runtime.connectClient({
      name: "unsafe-event-time",
      contract: apps.Alpha.request,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await readFile(
          join(runtime.workdir, "config/trellis/nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    try {
      const subject = "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha";
      await alpha.publishAlpha({ site: "time", value: "source" }).orThrow();
      const manager = await jetstreamManager(nats);
      const stream = (await manager.streams.list().next()).find((entry) =>
        entry.config.subjects?.some((filter) => filter.startsWith("events.")),
      );
      if (!stream) throw new Error("event stream missing");
      const source = await manager.streams.getMessage(stream.config.name, {
        last_by_subj: subject,
      });
      if (!source?.header) throw new Error("source event headers missing");

      const js = jetstream(nats);
      for (const [id, eventTime] of [
        ["unsafe-time-9999", "9999-12-31T23:59:59.999999999Z"],
        ["unsafe-time-garbage", "garbage"],
      ]) {
        source.header.set("Nats-Msg-Id", id);
        source.header.set("Trellis-Event-Time", eventTime);
        await js.publish(subject, source.data, { headers: source.header });
      }
      await alpha
        .publishAlpha({ site: "time", value: "after-unsafe" })
        .orThrow();
      const after = await manager.streams.getMessage(stream.config.name, {
        last_by_subj: subject,
      });
      const afterId = after?.header?.get("Nats-Msg-Id");
      if (!afterId) throw new Error("subsequent event id missing");

      for (const [id, rawTime, safeTime] of [
        [
          "unsafe-time-9999",
          "9999-12-31T23:59:59.999999999Z",
          "2262-04-11T23:47:16.854775807Z",
        ],
        ["unsafe-time-garbage", "garbage", "1970-01-01T00:00:00Z"],
      ]) {
        const row = await runtime.waitFor(
          async () =>
            (await admin.eventsQuery({ search: id }).orThrow()).items[0] ||
            false,
        );
        assert.deepEqual(row.eventTime, safeTime);
        assert.deepEqual(row.verificationStatus === "verified", false);
        const detail = (await admin.eventsInspect({ eventId: id }).orThrow())
          .event;
        assert.deepEqual(detail.headers["Trellis-Event-Time"], rawTime);
      }
      await runtime.waitFor(
        async () =>
          (await admin.eventsQuery({ search: afterId }).orThrow()).items
            .length > 0,
      );
    } finally {
      await nats.close();
    }
  });
});

test("Rust durable events match registrations and retain unhandled messages", async () => {
  for (const reverse of [false, true]) {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: "events",
        contract: participants.EventService.participant,
      });
      const eventsArgv = rustFixtureArgv("events");
      const start = () =>
        spawnTracked(eventsArgv[0], eventsArgv.slice(1), {
          env: {
            ...env,
            TRELLIS_URL: runtime.oauthIssuer,
            TRELLIS_IDENTITY_SEED: identity.seed,
            REVERSE: String(reverse),
          },
          stdio: "inherit",
        });
      let process = start();
      let exited = false;
      let status = process.exited.then((status) => {
        exited = true;
        return status;
      });
      const nats = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await readFile(
            join(
              runtime.workdir,
              "config/trellis/nats/creds/trellis-auth.creds",
            ),
          ),
        ),
      });
      try {
        const alpha = await runtime.connectClient({
          name: "alpha",
          contract: apps.Alpha.request,
        });
        const beta = await runtime.connectClient({
          name: "beta",
          contract: apps.Beta.request,
        });
        await runtime.waitFor(
          async () => {
            if (exited) {
              throw new Error(
                `event service exited: ${JSON.stringify(await status)}`,
              );
            }
            return (await alpha.observed({}, { timeout: 1000 })).isOk();
          },
          { timeoutMs: 120_000 },
        );
        await alpha.publishAlpha({ site: "one", value: "alpha" }).orThrow();
        await beta.publishBeta({ site: "one", value: "beta-one" }).orThrow();
        await beta.publishBeta({ site: "two", value: "beta-two" }).orThrow();
        await runtime
          .waitFor(async () => {
            const observed = await alpha.observed({}).orThrow();
            return observed.values.length === 3 && observed;
          })
          .then((observed) =>
            assert.deepEqual(observed.values, [
              "alpha",
              "beta-one",
              "beta-two",
            ]),
          );

        const manager = await jetstreamManager(nats);
        const streams = await manager.streams.list().next();
        const eventStream = streams.find((stream) =>
          stream.config.subjects?.some((subject) =>
            subject.startsWith("events."),
          ),
        );
        if (!eventStream) throw new Error("event stream missing");
        const consumers = (
          await manager.consumers.list(eventStream.config.name).next()
        ).filter((consumer) =>
          consumer.config.filter_subjects?.includes(
            "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha",
          ),
        );
        assert.deepEqual(consumers.length, 1);
        const revokedAlpha = await runtime.connectClient({
          name: "cold-revoked-alpha",
          contract: apps.Alpha.request,
        });
        const revokedSession = revokedAlpha.ownAuthority.current;
        assert(revokedSession?.loginSessionId);
        await manager.consumers.pause(
          eventStream.config.name,
          consumers[0].name,
          new Date(Date.now() + 60_000),
        );
        await revokedAlpha
          .publishAlpha({
            site: "revoked",
            value: "before-cutoff",
          })
          .orThrow();
        const revokedEvent = await manager.streams.getMessage(
          eventStream.config.name,
          {
            last_by_subj: "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha",
          },
        );
        const revokedDigest = revokedEvent?.header?.get(
          "authorization-context",
        );
        if (!revokedEvent || !revokedDigest) {
          throw new Error("cold revoked Rust event context missing");
        }
        // The signed cutoff has whole-second precision. Put the original
        // broker publication strictly before that boundary, not on either side
        // depending on scheduling within the current second.
        const publishedSecond = Math.floor(Date.now() / 1000);
        await runtime.waitFor(
          () => Math.floor(Date.now() / 1000) > publishedSecond,
        );
        await runtime.callAdminRpc("authSessionsRevoke", {
          loginSessionId: codecs.ulid.decode(revokedSession.loginSessionId),
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(async () =>
          (
            await runtime.callAdminRpc("authAuthorizationSessionsList", {
              principalId: codecs.ulid.decode(revokedSession.principalId),
              state: "revoked",
            })
          ).items.some(
            (session) =>
              session.loginSessionId === revokedSession.loginSessionId,
          ),
        );
        await manager.consumers.resume(
          eventStream.config.name,
          consumers[0].name,
        );
        await runtime.waitFor(
          async () =>
            (
              await manager.consumers.info(
                eventStream.config.name,
                consumers[0].name,
              )
            ).ack_floor.stream_seq >= revokedEvent.seq,
        );
        assert(
          (await alpha.observed({}).orThrow()).values.includes("before-cutoff"),
        );

        // A fresh provider has no cached context or event-ID history. Replay
        // the unchanged signed bytes at a genuinely later broker time; change
        // only the broker deduplication key, which is not publication authority.
        process.kill("SIGTERM");
        await status;
        exited = false;
        process = start();
        status = process.exited.then((status) => {
          exited = true;
          return status;
        });
        await runtime.waitFor(async () =>
          (await alpha.observed({}, { timeout: 1000 })).isOk(),
        );
        assert.deepEqual((await alpha.observed({}).orThrow()).values, []);
        revokedEvent.header.set("Nats-Msg-Id", ulid());
        const replayed = await jetstream(nats).publish(
          revokedEvent.subject,
          revokedEvent.data,
          { headers: revokedEvent.header },
        );
        await beta
          .publishBeta({ site: "after-restart", value: "valid-after-restart" })
          .orThrow();
        await runtime.waitFor(async () =>
          (await alpha.observed({}).orThrow()).values.includes(
            "valid-after-restart",
          ),
        );
        await runtime.waitFor(
          async () =>
            (
              await manager.consumers.info(
                eventStream.config.name,
                consumers[0].name,
              )
            ).ack_floor.stream_seq >= replayed.seq,
        );
        assert.deepEqual((await alpha.observed({}).orThrow()).values, [
          "valid-after-restart",
        ]);
        await alpha.dropAlpha({}).orThrow();
        await alpha.publishAlpha({ site: "one", value: "unhandled" }).orThrow();
        await beta
          .publishBeta({ site: "three", value: "beta-after-drop" })
          .orThrow();
        await runtime.waitFor(async () =>
          (await alpha.observed({}).orThrow()).values.includes(
            "beta-after-drop",
          ),
        );
        await runtime.waitFor(async () => {
          const info = await manager.consumers.info(
            eventStream.config.name,
            consumers[0].name,
          );
          return info.num_ack_pending === 1 && info.num_redelivered > 0;
        });
        assert.deepEqual((await alpha.observed({}).orThrow()).values, [
          "beta-after-drop",
          "valid-after-restart",
        ]);

        // A real consumer failure must terminate the owning service, not a dummy registration task.
        await manager.consumers.delete(
          eventStream.config.name,
          consumers[0].name,
        );
        await runtime.waitFor(() => exited, { timeoutMs: 15_000 });
        assert.notEqual((await status).code, 0);
        await runtime.services.disableInstance({
          instanceId: codecs.ulid.decode(identity.instanceId),
          expectedRevision: 1n,
          idempotencyKey: ulid(),
          reason: "event stream regression complete",
        });
      } finally {
        if (!exited) process.kill("SIGTERM");
        await status;
        await nats.close();
      }
    });
  }
});

for (const sdk of ["rust", "typescript"] as const) {
  test(`${sdk} durable delivery leases, DLQ replay, and crash recovery`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: `delivery-${sdk}`,
        contract: participants.EventService.participant,
      });
      const start = (crash: boolean) => {
        if (sdk === "rust") {
          const eventsArgv = rustFixtureArgv("events");
          return spawnTracked(eventsArgv[0], eventsArgv.slice(1), {
            env: {
              ...env,
              TRELLIS_URL: runtime.oauthIssuer,
              TRELLIS_IDENTITY_SEED: identity.seed,
              REVERSE: "false",
              CRASH: String(crash),
            },
            stdio: "inherit",
          });
        }
        return spawnTracked(
          execPath,
          [
            "--experimental-transform-types",
            fileURLToPath(
              new URL(
                "../../integration/fixtures/runtime/events.ts",
                import.meta.url,
              ),
            ),
            runtime.oauthIssuer,
            identity.seed,
          ],
          {
            env: { ...env, CRASH: String(crash) },
            stdio: "inherit",
          },
        );
      };

      const alpha = await runtime.connectClient({
        name: `delivery-alpha-${sdk}`,
        contract: apps.Alpha.request,
      });
      let process = start(true);
      try {
        await runtime.waitFor(
          async () =>
            (await alpha.deliveryStats({}, { timeout: 1_000 })).isOk(),
          { timeoutMs: 120_000 },
        );
        const consumer = await runtime.waitFor(async () => {
          const entry = (await runtime.events.consumersQuery({})).items.find(
            (item) =>
              item.filterSubjects.includes(
                "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha",
              ),
          );
          return entry || false;
        });
        assert(consumer.ackWaitMs !== undefined);

        await alpha.publishAlpha({ site: sdk, value: "slow" }).orThrow();
        const slow = await runtime.waitFor(async () => {
          const stats = await alpha.deliveryStats({}).orThrow();
          return stats.successes.includes("slow") && stats;
        });
        assert.deepEqual(slow.maxActive, 1n);
        assert.deepEqual(slow.attempts.length, 1);

        await alpha.publishAlpha({ site: sdk, value: "fail" }).orThrow();
        const retried = await runtime.waitFor(async () => {
          const result = await alpha.deliveryStats({}, { timeout: 1_000 });
          if (!result.isOk()) return false;
          const stats = result.orThrow();
          return stats.attempts.length >= 4 ? stats : false;
        });
        assert.deepEqual(retried.attempts.length, 4);
        const failed = await runtime.waitFor(async () => {
          const rows = (
            await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
              state: ["dead"],
            })
          ).items;
          return rows[0] || false;
        });
        assert.equal(failed.state, "dead");
        const afterFailure = await alpha.deliveryStats({}).orThrow();
        const retryTimes = afterFailure.attempts.slice(-3).map(Number);
        const [first, second, third] = retryTimes;
        if (
          first === undefined ||
          second === undefined ||
          third === undefined ||
          second - first < 100 ||
          third - second < 220
        ) {
          throw new Error(
            `declared backoff not observed: ${retryTimes.join(",")}`,
          );
        }
        assert.deepEqual(
          (
            await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
            })
          ).items.filter((row) => row.deadLetterId === failed.deadLetterId)
            .length,
          1,
        );

        await alpha.publishAlpha({ site: sdk, value: "replay" }).orThrow();
        const replayable = await runtime.waitFor(async () => {
          const rows = (
            await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
              state: ["dead"],
            })
          ).items;
          return (
            rows.find((row) => row.deadLetterId !== failed.deadLetterId) ||
            false
          );
        });
        let flooding = true;
        const flood = (async () => {
          let sequence = 0;
          while (flooding) {
            const published = `original-${sequence++}`;
            await alpha
              .publishAlpha({
                site: sdk,
                value: published,
              })
              .orThrow();
            await runtime.waitFor(async () =>
              (await alpha.deliveryStats({}).orThrow()).successes.includes(
                published,
              ),
            );
          }
        })();
        const replayStartedAt = Date.now();
        try {
          await runtime.events.deadLettersReplay({
            resourceId: consumer.resourceId,
            deadLetterId: replayable.deadLetterId,
            expectedRevision: replayable.revision,
            requestId: crypto.randomUUID(),
          });
          const activeReplay = await runtime.waitFor(async () => {
            const row = (
              await runtime.events.deadLettersQuery({
                resourceId: consumer.resourceId,
              })
            ).items.find((row) => row.deadLetterId === replayable.deadLetterId);
            return row &&
              row.generation === replayable.generation + 1 &&
              ["replaying", "resolved"].includes(row.state)
              ? row
              : false;
          });
          assert.deepEqual(activeReplay.generation, replayable.generation + 1);
          await runtime.waitFor(
            async () => {
              const result = await alpha.deliveryStats({}, { timeout: 1_000 });
              return (
                result.isOk() && result.orThrow().successes.includes("replay")
              );
            },
            { timeoutMs: 5_000 },
          );
        } finally {
          flooding = false;
          await flood;
        }
        if (Date.now() - replayStartedAt > 5_000) {
          throw new Error(`${sdk} replay was starved by original traffic`);
        }
        await runtime.waitFor(async () =>
          (
            await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
              state: ["resolved"],
            })
          ).items.some((row) => row.deadLetterId === replayable.deadLetterId),
        );
        assert.deepEqual(
          (
            await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
              state: ["dead"],
            })
          ).items.find((row) => row.deadLetterId === failed.deadLetterId)
            ?.state,
          "dead",
        );
        assert.deepEqual(
          (await alpha.deliveryStats({}).orThrow()).successes.filter(
            (value) => value === "replay",
          ).length,
          1,
        );

        await alpha.publishAlpha({ site: sdk, value: "crash" }).orThrow();
        await runtime.waitFor(
          async () => (await alpha.deliveryStats({}).orThrow()).active === 1n,
        );
        process.kill("SIGKILL");
        await process.exited;
        process = start(false);
        await runtime.waitFor(
          async () =>
            (await alpha.deliveryStats({}, { timeout: 1_000 })).isOk(),
          { timeoutMs: 120_000 },
        );
        // SIGKILL cannot acknowledge its in-flight delivery. Recovery waits
        // for the real broker lease rather than the harness's shorter default.
        await runtime.waitFor(
          async () =>
            (await alpha.deliveryStats({}).orThrow()).successes.includes(
              "crash",
            ),
          { timeoutMs: Number(consumer.ackWaitMs) * 2 },
        );
      } finally {
        try {
          process.kill("SIGTERM");
        } catch {
          /* already exited */
        }
        await process.exited;
      }
    });
  });
}

test("explicit ephemeral delivery works alongside a declared durable consumer", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `ephemeral-both-${crypto.randomUUID()}`,
      contract: participants.EventServiceBoth.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.EventServiceBoth.participant,
      seed: identity.seed,
    }).orThrow();
    try {
      const received: string[] = [];
      await service
        .onAlpha(
          ({ event }) => {
            received.push(event.value);
          },
          {},
          { mode: "ephemeral" },
        )
        .orThrow();
      await service.onBeta(() => {}, {}, { mode: "ephemeral" }).orThrow();
      await service.handleObserved(() => Result.ok({ values: [...received] }));

      const alpha = await runtime.connectClient({
        name: `ephemeral-both-alpha-${crypto.randomUUID()}`,
        contract: apps.Alpha.request,
      });
      await alpha.publishAlpha({ site: "both", value: "ephemeral" }).orThrow();
      const observed = await runtime.waitFor(async () => {
        const value = await alpha.observed({}).orThrow();
        return value.values.includes("ephemeral") ? value : false;
      });
      assert.deepEqual(observed.values, ["ephemeral"]);
      // The declared durable consumer is provisioned by bootstrap and stays
      // independent of the explicit ephemeral listener.
      const consumers = await runtime.events.consumersQuery({});
      assert.deepEqual(
        consumers.items.some((item) =>
          item.filterSubjects.includes(
            "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha",
          ),
        ),
        true,
        "the declared durable consumer remains provisioned",
      );
    } finally {
      await service.stop();
    }
  });
});

test("a declared durable consumer without Event Subscribe rejects explicit ephemeral", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `consumer-only-${crypto.randomUUID()}`,
      contract: participants.EventServiceConsumerOnly.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.EventServiceConsumerOnly.participant,
      seed: identity.seed,
    }).orThrow();
    try {
      let failure: unknown;
      try {
        await service.onAlpha(() => {}, {}, { mode: "ephemeral" }).orThrow();
      } catch (error) {
        failure = error;
      }
      assert.deepEqual(
        failure instanceof AuthError,
        true,
        "explicit ephemeral without Event Subscribe must fail with a typed AuthError",
      );
      if (failure instanceof AuthError) {
        assert.ok(
          failure.message.includes("Event Subscribe"),
          "the failure names the missing authority",
        );
      }
      // The default durable listener still works for the declared consumer.
      await service
        .onAlpha(() => {}, {}, { mode: "durable", group: "events" })
        .orThrow();
    } finally {
      await service.stop();
    }
  });
});

test("Rust consumer-only service rejects explicit ephemeral at the runtime boundary", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `consumer-only-rust-${crypto.randomUUID()}`,
      contract: participants.EventServiceConsumerOnly.participant,
    });
    const eventsArgv = rustFixtureArgv("events");
    const process = spawnTracked(eventsArgv[0], eventsArgv.slice(1), {
      env: {
        ...env,
        TRELLIS_URL: runtime.oauthIssuer,
        TRELLIS_IDENTITY_SEED: identity.seed,
        CONSUMER_ONLY: "true",
        EPHEMERAL: "true",
      },
      stdio: ["ignore", "pipe", "inherit"],
    });
    try {
      assert(process.stdout);
      const reader = Readable.toWeb(process.stdout)
        .pipeThrough(new TextDecoderStream())
        .getReader();
      let output = "";
      while (!output.includes("EPHEMERAL_REJECTED")) {
        const chunk = await reader.read();
        if (chunk.done) break;
        output += chunk.value;
      }
      assert.ok(
        output.includes("EPHEMERAL_REJECTED"),
        "Rust explicit ephemeral must fail fast",
      );
      assert.ok(
        output.includes("Event Subscribe"),
        "Rust failure names the missing authority",
      );
      assert.deepEqual(output.includes("EPHEMERAL_ACCEPTED"), false);
    } finally {
      try {
        process.kill("SIGTERM");
      } catch {
        /* already exited */
      }
      await process.exited;
    }
  });
});

test("a subscriber-only service receives explicit ephemeral delivery with no durable consumer", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `subscriber-only-${crypto.randomUUID()}`,
      contract: participants.EventSubscriberService.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.EventSubscriberService.participant,
      seed: identity.seed,
    }).orThrow();
    try {
      const received: string[] = [];
      await service
        .onAlpha(
          ({ event }) => {
            received.push(event.value);
          },
          {},
          { mode: "ephemeral" },
        )
        .orThrow();
      const alpha = await runtime.connectClient({
        name: `subscriber-only-publisher-${crypto.randomUUID()}`,
        contract: apps.Alpha.request,
      });
      const value = `ephemeral-${crypto.randomUUID()}`;
      await alpha.publishAlpha({ site: "test", value }).orThrow();
      await runtime.waitFor(() => received.includes(value), {
        timeoutMs: 15_000,
      });
      const consumers = await runtime.events.consumersQuery({});
      assert.deepEqual(
        consumers.items.some((item) =>
          item.filterSubjects.includes(
            "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha",
          ),
        ),
        false,
        "explicit ephemeral must not create a durable consumer",
      );
    } finally {
      await service.stop();
    }
  });
});
