import { credsAuthenticator } from "@nats-io/nats-core";
import { jetstream, jetstreamManager } from "@nats-io/jetstream";
import { connect } from "@nats-io/transport-node";
import { assertEquals, assertStringIncludes } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import { AuthError, Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apis as webApis,
  participants as webParticipants,
} from "trellis-web-generated";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("Auth connection lifecycle events survive the fenced outbox", async () => {
  await withTrellisRuntime(async (runtime) => {
    const admin = await runtime.connectClient({
      name: "auth-events-admin",
      contract: webParticipants.Console.participant,
    });
    await runtime.registerService({
      name: "auth-events-provider",
      contract: participants.EventService.participant,
    });
    const qualified = "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections";
    const lifecycleRows = async (connectionId: string, name: string) => {
      const rows = (await admin.eventsQuery({
        subject: `${qualified}.${name}`,
      }).orThrow()).items;
      const matching = [];
      for (const row of rows) {
        const detail =
          (await admin.eventsInspect({ eventId: row.eventId }).orThrow()).event;
        const payload = JSON.parse(new TextDecoder().decode(detail.payload));
        if (payload.connectionId === connectionId) {
          matching.push({ row, detail, payload });
        }
      }
      return matching;
    };
    const waitForEvent = async (connectionId: string, name: string) => {
      let row;
      const deadline = Date.now() + 30_000;
      while (!row && Date.now() < deadline) {
        const rows = await lifecycleRows(connectionId, name);
        if (rows.length === 1) row = rows[0];
        else await new Promise((resolve) => setTimeout(resolve, 100));
      }
      if (!row) throw new Error(`Timed out waiting for Connections.${name}`);
      const { detail, payload } = row;
      assertEquals(detail.row.subject, `${qualified}.${name}`);
      webApis.auth.API.actions[
        `event:Connections.${name}` as
          | "event:Connections.Opened"
          | "event:Connections.Closed"
          | "event:Connections.Kicked"
      ].payload.decode(payload);
      assertEquals(payload.connectionId, connectionId);
      return payload;
    };
    const connectionFor = async (participantId: string) =>
      await runtime.waitFor(async () => {
        const sessions = (await admin.sessionsList({
          participantId,
          state: "active",
        }).orThrow()).items;
        for (const session of sessions) {
          const connection = (await admin.connectionsList({
            sessionId: session.sessionId,
          }).orThrow()).items[0];
          if (connection) {
            return { ...connection, principalId: session.principalId };
          }
        }
        return false;
      });

    const openedClient = await runtime.connectClient({
      name: "auth-events-open-close",
      contract: participants.Alpha.participant,
    });
    const opened = await connectionFor(participants.Alpha.participant.id);
    const openedPayload = await waitForEvent(opened.connectionId, "Opened");
    assertEquals(
      openedPayload.participantId,
      participants.Alpha.participant.id,
    );
    assertEquals(openedPayload.principalId, opened.principalId);
    assertEquals(
      await lifecycleRows(opened.connectionId, "Opened").then((r) => r.length),
      1,
    );

    await openedClient.connection.close();
    await waitForEvent(opened.connectionId, "Closed");
    assertEquals(
      await lifecycleRows(opened.connectionId, "Closed").then((r) => r.length),
      1,
    );

    const kickedClient = await runtime.connectClient({
      name: "auth-events-kick",
      contract: participants.Beta.participant,
    });
    const kicked = await connectionFor(participants.Beta.participant.id);
    const session = (await admin.sessionsList({
      participantId: participants.Beta.participant.id,
      state: "active",
    }).orThrow()).items.find((entry) =>
      entry.sessionId === kicked.loginSessionId
    );
    if (!session) throw new Error("kick session missing");
    const revoke = {
      sessionId: session.sessionId,
      expectedVersion: session.version,
      idempotencyKey: crypto.randomUUID(),
      reason: "auth lifecycle event test",
    };
    await admin.sessionsRevoke(revoke).orThrow();
    await admin.sessionsRevoke(revoke).orThrow();
    const kickedPayload = await waitForEvent(kicked.connectionId, "Kicked");
    assertEquals(kickedPayload.reason, revoke.reason);
    assertEquals(
      await lifecycleRows(kicked.connectionId, "Kicked").then((r) => r.length),
      1,
    );
    await kickedClient.connection.close().catch(() => undefined);

    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    try {
      const manager = await jetstreamManager(nats);
      const eventStream = (await manager.streams.list().next()).find((stream) =>
        stream.config.subjects?.some((subject) => subject.startsWith("events."))
      );
      if (!eventStream) throw new Error("event stream missing");
      const streamConfig = eventStream.config;
      await manager.streams.update(streamConfig.name, {
        ...streamConfig,
        max_msg_size: 1,
      });
      const recoveryClient = await runtime.connectClient({
        name: "auth-events-recovery",
        contract: participants.Alpha.participant,
      });
      const recovery = await connectionFor(participants.Alpha.participant.id);
      assertEquals(
        (await lifecycleRows(recovery.connectionId, "Opened")).length,
        0,
      );
      await manager.streams.update(streamConfig.name, streamConfig);
      await runtime.restartControlPlane();
      await waitForEvent(recovery.connectionId, "Opened");
      assertEquals(
        await lifecycleRows(recovery.connectionId, "Opened").then((r) =>
          r.length
        ),
        1,
      );
      assertEquals(
        new Set([
          opened.connectionId,
          kicked.connectionId,
          recovery.connectionId,
        ]).size,
        3,
      );
      await recoveryClient.connection.close();
    } finally {
      await nats.close();
    }
  });
});

Deno.test("runtime Events transports Health.StatusChanged on its qualified subject", async () => {
  await withTrellisRuntime(async (runtime) => {
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    try {
      const manager = await jetstreamManager(nats);
      const subject = "events.v1.dHJlbGxpcy5oZWFsdGhAdjE.StatusChanged";
      const now = new Date().toISOString();
      const heartbeat = {
        sample: { id: "01J00000000000000000000000", time: now },
        participant: {
          name: "Health transport test",
          kind: "service",
          instanceId: "instance",
          contractId: "acme.test@v1",
          contractDigest: "digest",
          startedAt: now,
          publishIntervalMs: "30000",
          runtime: "typescript",
        },
        reportedStatus: "healthy",
        checks: [],
      };
      const js = jetstream(nats);
      await js.publish(
        "health.v1.heartbeat.service.YWNtZS50ZXN0QHYx.ZGlnZXN0.ZGVwbG95bWVudA.aW5zdGFuY2U.session",
        new TextEncoder().encode(JSON.stringify(heartbeat)),
      );
      heartbeat.sample.id = "01J00000000000000000000001";
      heartbeat.reportedStatus = "unhealthy";
      await js.publish(
        "health.v1.heartbeat.service.YWNtZS50ZXN0QHYx.ZGlnZXN0.ZGVwbG95bWVudA.aW5zdGFuY2U.session",
        new TextEncoder().encode(JSON.stringify(heartbeat)),
      );
      const message = await runtime.waitFor(async () => {
        const streams = await manager.streams.list().next();
        const stream = streams.find((entry) =>
          entry.config.subjects?.some((filter) => filter.startsWith("events."))
        );
        if (!stream) return false;
        try {
          return await manager.streams.getMessage(stream.config.name, {
            last_by_subj: subject,
          });
        } catch {
          return false;
        }
      });
      assertEquals(message.subject, subject);
      assertEquals(
        JSON.parse(new TextDecoder().decode(message.data)).status,
        "unhealthy",
      );
    } finally {
      await nats.close();
    }
  });
});

Deno.test("Events projector retains rejected unsafe times and advances", async () => {
  await withTrellisRuntime(async (runtime) => {
    const admin = await runtime.connectClient({
      name: "unsafe-event-time-admin",
      contract: webParticipants.Console.participant,
    });
    await runtime.registerService({
      name: "unsafe-event-time-provider",
      contract: participants.EventService.participant,
    });
    const alpha = await runtime.connectClient({
      name: "unsafe-event-time",
      contract: participants.Alpha.participant,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    try {
      const subject = "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha";
      await alpha.publishAlpha({ site: "time", value: "source" }).orThrow();
      const manager = await jetstreamManager(nats);
      const stream = (await manager.streams.list().next()).find((entry) =>
        entry.config.subjects?.some((filter) => filter.startsWith("events."))
      );
      if (!stream) throw new Error("event stream missing");
      const source = await manager.streams.getMessage(stream.config.name, {
        last_by_subj: subject,
      });
      if (!source?.header) throw new Error("source event headers missing");

      const js = jetstream(nats);
      for (
        const [id, eventTime] of [
          ["unsafe-time-9999", "9999-12-31T23:59:59.999999999Z"],
          ["unsafe-time-garbage", "garbage"],
        ]
      ) {
        source.header.set("Nats-Msg-Id", id);
        source.header.set("Trellis-Event-Time", eventTime);
        await js.publish(subject, source.data, { headers: source.header });
      }
      await alpha.publishAlpha({ site: "time", value: "after-unsafe" })
        .orThrow();
      const after = await manager.streams.getMessage(stream.config.name, {
        last_by_subj: subject,
      });
      const afterId = after?.header?.get("Nats-Msg-Id");
      if (!afterId) throw new Error("subsequent event id missing");

      for (
        const [id, rawTime, safeTime] of [
          [
            "unsafe-time-9999",
            "9999-12-31T23:59:59.999999999Z",
            "2262-04-11T23:47:16.854775807Z",
          ],
          ["unsafe-time-garbage", "garbage", "1970-01-01T00:00:00Z"],
        ]
      ) {
        const row = await runtime.waitFor(async () =>
          (await admin.eventsQuery({ search: id }).orThrow()).items[0] || false
        );
        assertEquals(row.eventTime, safeTime);
        assertEquals(row.verificationStatus === "verified", false);
        const detail = (await admin.eventsInspect({ eventId: id }).orThrow())
          .event;
        assertEquals(detail.headers["Trellis-Event-Time"], rawTime);
      }
      await runtime.waitFor(async () =>
        (await admin.eventsQuery({ search: afterId }).orThrow()).items.length >
          0
      );
    } finally {
      await nats.close();
    }
  });
});

Deno.test("Rust durable events match registrations and retain unhandled messages", async () => {
  for (const reverse of [false, true]) {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: "events",
        contract: participants.EventService.participant,
      });
      const process = new Deno.Command("cargo", {
        args: [
          "run",
          "--config",
          `patch.crates-io.trellis-rs.path=${
            JSON.stringify(
              fromFileUrl(
                new URL("../../crates/trellis", import.meta.url),
              ),
            )
          }`,
          "--bin",
          "events",
          "--manifest-path",
          fromFileUrl(
            new URL(
              "../../integration/fixtures/runtime/Cargo.toml",
              import.meta.url,
            ),
          ),
        ],
        env: {
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_IDENTITY_SEED: identity.seed,
          REVERSE: String(reverse),
          CARGO_TARGET_DIR: fromFileUrl(
            new URL("../../target", import.meta.url),
          ),
        },
        stdout: "inherit",
        stderr: "inherit",
      }).spawn();
      let exited = false;
      const status = process.status.then((status) => {
        exited = true;
        return status;
      });
      const nats = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await Deno.readFile(
            join(runtime.workdir, "nats/creds/trellis-auth.creds"),
          ),
        ),
      });
      try {
        const alpha = await runtime.connectClient({
          name: "alpha",
          contract: participants.Alpha.participant,
        });
        const beta = await runtime.connectClient({
          name: "beta",
          contract: participants.Beta.participant,
        });
        await runtime.waitFor(async () => {
          if (exited) {
            throw new Error(
              `event service exited: ${JSON.stringify(await status)}`,
            );
          }
          return (await alpha.observed({}, { timeout: 1000 })).isOk();
        }, { timeoutMs: 120_000 });
        await alpha.publishAlpha({ site: "one", value: "alpha" }).orThrow();
        await beta.publishBeta({ site: "one", value: "beta-one" }).orThrow();
        await beta.publishBeta({ site: "two", value: "beta-two" }).orThrow();
        await runtime.waitFor(async () => {
          const observed = await alpha.observed({}).orThrow();
          return observed.values.length === 3 && observed;
        }).then((observed) =>
          assertEquals(observed.values, ["alpha", "beta-one", "beta-two"])
        );

        const manager = await jetstreamManager(nats);
        const streams = await manager.streams.list().next();
        const eventStream = streams.find((stream) =>
          stream.config.subjects?.some((subject) =>
            subject.startsWith("events.")
          )
        );
        if (!eventStream) throw new Error("event stream missing");
        const consumers = (await manager.consumers.list(eventStream.config.name)
          .next()).filter((consumer) =>
            consumer.config.filter_subjects?.includes(
              "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha",
            )
          );
        assertEquals(consumers.length, 1);
        const admin = await runtime.connectClient({
          name: "events-admin",
          contract: webParticipants.Console.participant,
        });
        const existingSessions = new Set(
          (await admin.sessionsList({
            participantId: participants.Alpha.participant.id,
            state: "active",
          }).orThrow()).items.map((entry) => entry.sessionId),
        );
        const revokedAlpha = await runtime.connectClient({
          name: "cold-revoked-alpha",
          contract: participants.Alpha.participant,
        });
        const revokedSession = (await admin.sessionsList({
          participantId: participants.Alpha.participant.id,
          state: "active",
        }).orThrow()).items.find((entry) =>
          !existingSessions.has(entry.sessionId)
        );
        if (!revokedSession) throw new Error("revoked alpha session missing");
        await manager.consumers.pause(
          eventStream.config.name,
          consumers[0].name,
          new Date(Date.now() + 60_000),
        );
        await revokedAlpha.publishAlpha({
          site: "revoked",
          value: "cold-revoked",
        }).orThrow();
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
        await admin.sessionsRevoke({
          sessionId: revokedSession.sessionId,
          expectedVersion: revokedSession.version,
          idempotencyKey: crypto.randomUUID(),
          reason: "cold Rust provider acceptance",
        }).orThrow();
        let contextStream: string | undefined;
        let contextSubjectPrefix: string | undefined;
        for (const stream of streams) {
          for (const subject of stream.config.subjects ?? []) {
            if (!subject.startsWith("$KV.") || !subject.endsWith(">")) continue;
            const prefix = subject.slice(0, -1);
            try {
              const stored = await manager.streams.getMessage(
                stream.config.name,
                { last_by_subj: `${prefix}${revokedDigest}` },
              );
              if (!stored) continue;
              contextStream = stream.config.name;
              contextSubjectPrefix = prefix;
              break;
            } catch {
              // This KV stream does not own authorization contexts.
            }
          }
          if (contextStream) break;
        }
        if (!contextStream || !contextSubjectPrefix) {
          throw new Error("authorization context stream missing");
        }
        await runtime.waitFor(async () =>
          Boolean(
            await manager.streams.getMessage(contextStream, {
              last_by_subj:
                `${contextSubjectPrefix}revocation.${revokedDigest}`,
            }),
          )
        );
        await manager.consumers.resume(
          eventStream.config.name,
          consumers[0].name,
        );
        await runtime.waitFor(async () =>
          (await manager.consumers.info(
            eventStream.config.name,
            consumers[0].name,
          )).ack_floor.stream_seq >= revokedEvent.seq
        );
        assertEquals(
          (await alpha.observed({}).orThrow()).values.includes("cold-revoked"),
          false,
        );
        await alpha.dropAlpha({}).orThrow();
        await alpha.publishAlpha({ site: "one", value: "unhandled" }).orThrow();
        await beta.publishBeta({ site: "three", value: "beta-after-drop" })
          .orThrow();
        await runtime.waitFor(async () =>
          (await alpha.observed({}).orThrow()).values.includes(
            "beta-after-drop",
          )
        );
        await runtime.waitFor(async () => {
          const info = await manager.consumers.info(
            eventStream.config.name,
            consumers[0].name,
          );
          return info.num_ack_pending === 1 && info.num_redelivered > 0;
        });
        assertEquals((await alpha.observed({}).orThrow()).values, [
          "alpha",
          "beta-after-drop",
          "beta-one",
          "beta-two",
        ]);

        // A real consumer failure must terminate the owning service, not a dummy registration task.
        await manager.consumers.delete(
          eventStream.config.name,
          consumers[0].name,
        );
        await runtime.waitFor(
          () => exited,
          { timeoutMs: 15_000 },
        );
        assertEquals((await status).success, false);
        const disabled = await runtime.services.disableInstance({
          instanceId: identity.instanceId,
          expectedVersion: 1n,
          idempotencyKey: crypto.randomUUID(),
          reason: "event stream regression complete",
        });
        assertEquals(disabled.instance.state, "disabled");
      } finally {
        if (!exited) process.kill("SIGTERM");
        await status;
        await nats.close();
      }
    });
  }
});

for (const sdk of ["rust", "typescript"] as const) {
  Deno.test(`${sdk} durable delivery leases, DLQ replay, and crash recovery`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: `delivery-${sdk}`,
        contract: participants.EventService.participant,
      });
      const start = (crash: boolean) => {
        if (sdk === "rust") {
          return new Deno.Command("cargo", {
            args: [
              "run",
              "--config",
              `patch.crates-io.trellis-rs.path=${
                JSON.stringify(
                  fromFileUrl(
                    new URL("../../crates/trellis", import.meta.url),
                  ),
                )
              }`,
              "--bin",
              "events",
              "--manifest-path",
              fromFileUrl(
                new URL(
                  "../../integration/fixtures/runtime/Cargo.toml",
                  import.meta.url,
                ),
              ),
            ],
            env: {
              TRELLIS_URL: runtime.trellisUrl,
              TRELLIS_IDENTITY_SEED: identity.seed,
              REVERSE: "false",
              CRASH: String(crash),
              CARGO_TARGET_DIR: fromFileUrl(
                new URL("../../target", import.meta.url),
              ),
            },
            stdout: "inherit",
            stderr: "inherit",
          }).spawn();
        }
        return new Deno.Command(Deno.execPath(), {
          args: [
            "run",
            "--allow-env",
            "--allow-net",
            "--allow-read",
            "--allow-sys",
            "--config",
            fromFileUrl(
              new URL(
                "../../integration/fixtures/runtime/deno.json",
                import.meta.url,
              ),
            ),
            fromFileUrl(
              new URL(
                "../../integration/fixtures/runtime/events.ts",
                import.meta.url,
              ),
            ),
            runtime.trellisUrl,
            identity.seed,
          ],
          env: { CRASH: String(crash) },
          stdout: "inherit",
          stderr: "inherit",
        }).spawn();
      };

      const alpha = await runtime.connectClient({
        name: `delivery-alpha-${sdk}`,
        contract: participants.Alpha.participant,
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

        await alpha.publishAlpha({ site: sdk, value: "slow" }).orThrow();
        const slow = await runtime.waitFor(async () => {
          const stats = await alpha.deliveryStats({}).orThrow();
          return stats.successes.includes("slow") && stats;
        });
        assertEquals(slow.maxActive, 1n);
        assertEquals(slow.attempts.length, 1);

        await alpha.publishAlpha({ site: sdk, value: "fail" }).orThrow();
        const retried = await runtime.waitFor(async () => {
          const result = await alpha.deliveryStats({}, { timeout: 1_000 });
          if (!result.isOk()) return false;
          const stats = result.orThrow();
          return stats.attempts.length >= 4 ? stats : false;
        });
        assertEquals(retried.attempts.length, 4);
        const failed = await runtime.waitFor(async () => {
          const rows = (await runtime.events.deadLettersQuery({
            resourceId: consumer.resourceId,
            state: ["dead"],
          })).items;
          return rows.find((row) =>
            row.lastError?.includes("fixture handler failure")
          ) || false;
        });
        assertStringIncludes(failed.lastError ?? "", "fixture handler failure");
        const afterFailure = await alpha.deliveryStats({}).orThrow();
        const retryTimes = afterFailure.attempts.slice(-3).map(Number);
        if (
          retryTimes.length !== 3 || retryTimes[1]! - retryTimes[0]! < 100 ||
          retryTimes[2]! - retryTimes[1]! < 220
        ) {
          throw new Error(
            `declared backoff not observed: ${retryTimes.join(",")}`,
          );
        }
        assertEquals(
          (await runtime.events.deadLettersQuery({
            resourceId: consumer.resourceId,
          }))
            .items.filter((row) => row.deadLetterId === failed.deadLetterId)
            .length,
          1,
        );

        await alpha.publishAlpha({ site: sdk, value: "replay" }).orThrow();
        const replayable = await runtime.waitFor(async () => {
          const rows = (await runtime.events.deadLettersQuery({
            resourceId: consumer.resourceId,
            state: ["dead"],
          })).items;
          return rows.find((row) => row.deadLetterId !== failed.deadLetterId) ||
            false;
        });
        let flooding = true;
        const flood = (async () => {
          let sequence = 0;
          while (flooding) {
            const published = `original-${sequence++}`;
            await alpha.publishAlpha({
              site: sdk,
              value: published,
            }).orThrow();
            await runtime.waitFor(async () =>
              (await alpha.deliveryStats({}).orThrow()).successes.includes(
                published,
              )
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
            const row = (await runtime.events.deadLettersQuery({
              resourceId: consumer.resourceId,
            })).items.find((row) =>
              row.deadLetterId === replayable.deadLetterId
            );
            return row && row.generation === replayable.generation + 1 &&
                ["replaying", "resolved"].includes(row.state)
              ? row
              : false;
          });
          assertEquals(activeReplay.generation, replayable.generation + 1);
          await runtime.waitFor(async () => {
            const result = await alpha.deliveryStats({}, { timeout: 1_000 });
            return result.isOk() &&
              result.orThrow().successes.includes("replay");
          }, { timeoutMs: 5_000 });
        } finally {
          flooding = false;
          await flood;
        }
        if (Date.now() - replayStartedAt > 5_000) {
          throw new Error(`${sdk} replay was starved by original traffic`);
        }
        await runtime.waitFor(async () =>
          (await runtime.events.deadLettersQuery({
            resourceId: consumer.resourceId,
            state: ["resolved"],
          })).items.some((row) => row.deadLetterId === replayable.deadLetterId)
        );
        assertEquals(
          (await runtime.events.deadLettersQuery({
            resourceId: consumer.resourceId,
            state: ["dead"],
          })).items.find((row) => row.deadLetterId === failed.deadLetterId)
            ?.state,
          "dead",
        );
        assertEquals(
          (await alpha.deliveryStats({}).orThrow()).successes.filter((value) =>
            value === "replay"
          ).length,
          1,
        );

        await alpha.publishAlpha({ site: sdk, value: "crash" }).orThrow();
        await runtime.waitFor(async () =>
          (await alpha.deliveryStats({}).orThrow()).active === 1n
        );
        process.kill("SIGKILL");
        await process.status;
        process = start(false);
        await runtime.waitFor(
          async () =>
            (await alpha.deliveryStats({}, { timeout: 1_000 })).isOk(),
          { timeoutMs: 120_000 },
        );
        await runtime.waitFor(async () =>
          (await alpha.deliveryStats({}).orThrow()).successes.includes("crash")
        );
      } finally {
        try {
          process.kill("SIGTERM");
        } catch { /* already exited */ }
        await process.status;
      }
    });
  });
}

Deno.test("explicit ephemeral delivery works alongside a declared durable consumer", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `ephemeral-both-${crypto.randomUUID()}`,
      contract: participants.EventServiceBoth.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.EventServiceBoth.participant,
      seed: identity.seed,
    }).orThrow();
    try {
      const received: string[] = [];
      await service.onAlpha(
        ({ event }) => {
          received.push(event.value);
        },
        {},
        { mode: "ephemeral" },
      ).orThrow();
      await service.onBeta(() => {}, {}, { mode: "ephemeral" }).orThrow();
      await service.handleObserved(() => Result.ok({ values: [...received] }));

      const alpha = await runtime.connectClient({
        name: `ephemeral-both-alpha-${crypto.randomUUID()}`,
        contract: participants.Alpha.participant,
      });
      await alpha.publishAlpha({ site: "both", value: "ephemeral" })
        .orThrow();
      const observed = await runtime.waitFor(async () => {
        const value = await alpha.observed({}).orThrow();
        return value.values.includes("ephemeral") ? value : false;
      });
      assertEquals(observed.values, ["ephemeral"]);
      // The declared durable consumer is provisioned by bootstrap and stays
      // independent of the explicit ephemeral listener.
      const consumers = await runtime.events.consumersQuery({});
      assertEquals(
        consumers.items.some((item) =>
          item.filterSubjects.includes(
            "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha",
          )
        ),
        true,
        "the declared durable consumer remains provisioned",
      );
    } finally {
      await service.stop();
    }
  });
});

Deno.test("a declared durable consumer without Event Subscribe rejects explicit ephemeral", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `consumer-only-${crypto.randomUUID()}`,
      contract: participants.EventServiceConsumerOnly.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
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
      assertEquals(
        failure instanceof AuthError,
        true,
        "explicit ephemeral without Event Subscribe must fail with a typed AuthError",
      );
      if (failure instanceof AuthError) {
        assertStringIncludes(
          String(failure.message),
          "Event Subscribe",
          "the failure names the missing authority",
        );
      }
      // The default durable listener still works for the declared consumer.
      await service.onAlpha(() => {}, {}, { mode: "durable", group: "events" })
        .orThrow();
    } finally {
      await service.stop();
    }
  });
});

Deno.test("Rust consumer-only service rejects explicit ephemeral at the runtime boundary", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `consumer-only-rust-${crypto.randomUUID()}`,
      contract: participants.EventServiceConsumerOnly.participant,
    });
    const process = new Deno.Command("cargo", {
      args: [
        "run",
        "--config",
        `patch.crates-io.trellis-rs.path=${
          JSON.stringify(
            fromFileUrl(new URL("../../rust/crates/trellis", import.meta.url)),
          )
        }`,
        "--bin",
        "events",
        "--manifest-path",
        fromFileUrl(
          new URL(
            "../../integration/fixtures/runtime/Cargo.toml",
            import.meta.url,
          ),
        ),
      ],
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: identity.seed,
        CONSUMER_ONLY: "true",
        EPHEMERAL: "true",
        CARGO_TARGET_DIR: fromFileUrl(
          new URL("../../rust/target", import.meta.url),
        ),
      },
      stdout: "piped",
      stderr: "inherit",
    }).spawn();
    try {
      const reader = process.stdout.pipeThrough(new TextDecoderStream())
        .getReader();
      let output = "";
      while (!output.includes("EPHEMERAL_REJECTED")) {
        const chunk = await reader.read();
        if (chunk.done) break;
        output += chunk.value;
      }
      assertStringIncludes(
        output,
        "EPHEMERAL_REJECTED",
        "Rust explicit ephemeral must fail fast",
      );
      assertStringIncludes(
        output,
        "Event Subscribe",
        "Rust failure names the missing authority",
      );
      assertEquals(output.includes("EPHEMERAL_ACCEPTED"), false);
    } finally {
      try {
        process.kill("SIGTERM");
      } catch { /* already exited */ }
      await process.status;
    }
  });
});

Deno.test("a subscriber-only service receives explicit ephemeral delivery with no durable consumer", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: `subscriber-only-${crypto.randomUUID()}`,
      contract: participants.EventSubscriberService.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.EventSubscriberService.participant,
      seed: identity.seed,
    }).orThrow();
    try {
      const received: string[] = [];
      await service.onAlpha(
        ({ event }) => {
          received.push(event.value);
        },
        {},
        { mode: "ephemeral" },
      ).orThrow();
      const alpha = await runtime.connectClient({
        name: `subscriber-only-publisher-${crypto.randomUUID()}`,
        contract: participants.Alpha.participant,
      });
      const value = `ephemeral-${crypto.randomUUID()}`;
      await alpha.publishAlpha({ site: "test", value }).orThrow();
      await runtime.waitFor(() => received.includes(value), {
        timeoutMs: 15_000,
      });
      const consumers = await runtime.events.consumersQuery({});
      assertEquals(
        consumers.items.some((item) =>
          item.filterSubjects.includes(
            "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha",
          )
        ),
        false,
        "explicit ephemeral must not create a durable consumer",
      );
    } finally {
      await service.stop();
    }
  });
});
