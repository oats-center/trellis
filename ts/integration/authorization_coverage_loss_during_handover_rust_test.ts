/** Shared retirement-coverage loss fences accepted work while replacement replay is stalled. */
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { AckPolicy, jetstreamManager } from "@nats-io/jetstream";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import { type Attributes, decodeMetrics } from "./_support/otlp.ts";

test("Rust terminates accepted work on shared coverage loss before replacement replay finishes", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const owner = ulid();
      let points: { name: string; attributes: Attributes; value: number }[] =
        [];
      let telemetryError: unknown;
      const collector = createServer(async (request, response) => {
        try {
          const chunks: Buffer[] = [];
          for await (const chunk of request) chunks.push(Buffer.from(chunk));
          const decoded = decodeMetrics(Buffer.concat(chunks));
          const next = decoded.resourceMetrics
            .filter((resource) =>
              resource.resource?.attributes?.some(
                (attribute) =>
                  attribute.key === "service.instance.id" &&
                  attribute.value?.stringValue === owner,
              ),
            )
            .flatMap((resource) => resource.scopeMetrics)
            .filter((scope) => scope.scope?.name === "@oatscenter/trellis")
            .flatMap((scope) => scope.metrics)
            .flatMap(
              (metric) =>
                metric.sum?.dataPoints.map((point) => ({
                  name: metric.name,
                  attributes: point.attributes,
                  value: Number(point.asInt ?? point.asDouble),
                })) ?? [],
            );
          if (next.length) points = next;
          response
            .writeHead(200, { "content-type": "application/x-protobuf" })
            .end();
        } catch (error) {
          telemetryError = error;
          response.writeHead(400).end();
        }
      });
      await new Promise<void>((resolve) =>
        collector.listen(0, "127.0.0.1", resolve),
      );
      const address = collector.address();
      assert(address && typeof address !== "string");
      const total = (name: string, reason?: string) => {
        if (telemetryError) throw telemetryError;
        return points
          .filter(
            (point) =>
              point.name === name &&
              point.attributes.some(
                (attribute) =>
                  attribute.key === "trellis.side" &&
                  attribute.value?.stringValue === "provider",
              ) &&
              (!reason ||
                point.attributes.some(
                  (attribute) =>
                    attribute.key === "trellis.reason" &&
                    attribute.value?.stringValue === reason,
                )),
          )
          .reduce((sum, point) => sum + point.value, 0);
      };
      try {
        const targetIdentity = await runtime.registerService({
          name: "coverage-target",
          contract: participants.TransportGrowthTarget.participant,
        });
        const target = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.TransportGrowthTarget.participant,
          seed: targetIdentity.seed,
        }).orThrow();
        const targetExit = target.wait().catch(() => undefined);
        try {
          await target.handleAdvance(() => Result.ok({}));
          let extensions = 0;
          await target.handleExtend(() => {
            extensions++;
            return Result.ok({});
          });
          const identity = await runtime.registerService({
            deployment: "coverage-subject",
            name: "coverage-subject",
            contract: participants.TransportGrowthSubject.participant,
          });
          const capabilityId =
            "runtime-trellis.transport_growth.v1:growth.extend";
          const grants = await runtime.callAdminRpc(
            "authCapabilityGrantsList",
            {
              capabilityId,
            },
          );
          assert.equal(grants.items.length, 1);
          const grant = grants.items[0];
          await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
            grantId: grant.grantId,
            expectedRevision: grant.revision,
            idempotencyKey: ulid(),
          });
          const argv = rustFixtureArgv("transport_growth_subject");
          const child = spawnTracked(argv[0], argv.slice(1), {
            env: {
              ...Object.fromEntries(
                Object.entries(process.env).filter(
                  ([key]) => !key.startsWith("OTEL_"),
                ),
              ),
              TRELLIS_URL: runtime.oauthIssuer,
              TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
              TRELLIS_IDENTITY_SEED: identity.seed,
              OTEL_SDK_DISABLED: "false",
              OTEL_TRACES_EXPORTER: "none",
              OTEL_METRICS_EXPORTER: "otlp",
              OTEL_EXPORTER_OTLP_METRICS_ENDPOINT: `http://127.0.0.1:${address.port}/v1/metrics`,
              OTEL_EXPORTER_OTLP_METRICS_PROTOCOL: "http/protobuf",
              OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE: "cumulative",
              OTEL_RESOURCE_ATTRIBUTES: `service.instance.id=${owner}`,
              OTEL_METRIC_EXPORT_INTERVAL: "500",
            },
          });
          let output = "";
          child.stdout.setEncoding("utf8");
          child.stdout.on("data", (chunk: string) => {
            output += chunk;
          });
          child.stderr.pipe(process.stderr);
          try {
            await runtime.waitFor(() =>
              output.includes("TRANSPORT_GROWTH_ADVANCE_OK"),
            );
            const caller = await runtime.connectClient({
              name: "coverage-observer",
              contract: apps.LiveProbeCaller.request,
            });
            const abort = new AbortController();
            let framesDone: Promise<void> | undefined;
            try {
              const observation = await caller
                .watch(
                  { runId: ulid(), streamId: "coverage-loss" },
                  { signal: abort.signal },
                )
                .orThrow();
              const first = (await deadline(observation.next(), 10_000)).value;
              assert(first);
              let latest = first;
              framesDone = (async () => {
                for await (const frame of observation) latest = frame;
              })().catch(() => undefined);
              await runtime.waitFor(() => total("trellis.live.sessions") === 1);
              const beforeLoss = total(
                "trellis.live.ends",
                "authorization_lost",
              );
              const gate = runtime.nativeTransportGate();
              const route = apis.liveprobe.API.actions["live:Watch"].subject;
              const original = gate
                .connections()
                .find((connection) =>
                  connection.deliveries.some(
                    (message) => message.subject === route,
                  ),
                );
              assert(original);
              const privileged = await connect({
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
              const js = await jetstreamManager(privileged);
              const stream = "KV_TRELLIS_AUTH_REVOCATIONS";
              const settings = (await js.streams.info(stream)).config;
              const hold = gate.armResponseHold(
                "$JS.API.CONSUMER.INFO.KV_TRELLIS_AUTH_REVOCATIONS.",
                undefined,
                (body) =>
                  JSON.parse(new TextDecoder().decode(body)).config
                    ?.description ===
                  "Trellis logical-session retirement mirror",
              );
              try {
                await runtime.callAdminRpc("authCapabilityGrantsGrant", {
                  principalId: grant.principalId,
                  capabilityId,
                  expectedRevision: 0n,
                  idempotencyKey: ulid(),
                });
                const candidate = await deadline(hold.held, 90_000);
                assert.notEqual(candidate.connectionId, original.id);
                const accepting = gate.connection(original.id);
                assert(accepting);
                const sent = accepting.outboundContexts.filter((message) =>
                  message.subject.startsWith("live.v1.data."),
                ).length;
                await runtime.waitFor(() => latest.index > BigInt(sent));
                const prefix = `$JS.API.CONSUMER.MSG.NEXT.${stream}.`;
                const pull = original.outboundContexts.findLast((message) =>
                  message.subject.startsWith(prefix),
                );
                assert(
                  pull,
                  "identify the mirror from its actual owner socket",
                );
                const count = (await js.streams.info(stream)).state
                  .consumer_count;
                const constrained = {
                  ...settings,
                  max_consumers: count,
                };
                await js.streams.update(stream, constrained);
                await js.consumers.delete(
                  stream,
                  pull.subject.slice(prefix.length),
                );
                const blocker = await js.consumers.add(stream, {
                  name: ulid(),
                  ack_policy: AckPolicy.None,
                });
                await runtime.waitFor(
                  () =>
                    total("trellis.live.ends", "authorization_lost") ===
                      beforeLoss + 1 && total("trellis.live.sessions") === 0,
                  { timeoutMs: 8_000 },
                );
                assert.equal(
                  gate.connection(candidate.connectionId)?.closed,
                  false,
                );
                await js.consumers.delete(stream, blocker.name);
                await js.streams.update(stream, settings);
                await hold.release();
                child.stdin.write("WAIT_EXTEND\n");
                await runtime.waitFor(() =>
                  output.includes("TRANSPORT_GROWTH_EXTEND_OK"),
                );
                assert.equal(extensions, 1);
                assert(
                  gate
                    .connection(candidate.connectionId)
                    ?.outboundContexts.some(
                      (message) =>
                        message.subject ===
                        apis.transport_growth.API.actions["rpc:Extend"].subject,
                    ),
                );
                const fresh = await caller
                  .watch({ runId: ulid(), streamId: "coverage-recovered" })
                  .orThrow();
                try {
                  assert((await deadline(fresh.next(), 10_000)).value);
                  const receipt = await fresh.close().orThrow();
                  assert.equal(receipt.remote, "confirmed");
                  assert.equal(receipt.cleanup, "complete");
                } finally {
                  await fresh
                    .close()
                    .orThrow()
                    .catch(() => undefined);
                }
              } finally {
                await js.streams.update(stream, settings);
                await hold.release();
                await privileged.close();
              }
            } finally {
              abort.abort();
              await framesDone;
              await caller.connection.close();
            }
          } finally {
            if (!child.stdin.destroyed) child.stdin.end("EXIT\n");
            child.kill("SIGTERM");
            await child.exited;
          }
        } finally {
          await target.stop();
          await targetExit;
        }
      } finally {
        await new Promise<void>((resolve, reject) =>
          collector.close((error) => (error ? reject(error) : resolve())),
        );
      }
    },
    {
      interruptibleNativeProxy: true,
      authorization: {
        contextLifetimeSeconds: 53,
      },
    },
  );
});
