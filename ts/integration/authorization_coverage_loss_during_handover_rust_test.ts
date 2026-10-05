/** Real peer-watch loss must interrupt stalled Rust coverage replacement. */
import { assert, assertEquals } from "@std/assert";
import { createClient } from "@libsql/client";
import { join } from "@std/path";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { createRequire } from "node:module";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestConnectedClient } from "@oatscenter/trellis-testkit";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

type Attachment = {
  participantId: string;
  contextDigest: string;
  runtimeConnectionId: string;
};
type ConsumerInfo = {
  name: string;
  stream_name: string;
  created: string;
  push_bound: boolean;
  config: { filter_subject: string; deliver_subject: string };
};

// Decode the ordinary Rust HTTP/protobuf export with the already-installed
// OpenTelemetry exporter's own generated OTLP decoder, not a second wire parser.
// These types describe only the decoded fields this proof reads.
type MetricAttributes = {
  key: string;
  value?: { stringValue?: string | null } | null;
}[];
type MetricExport = {
  resourceMetrics: {
    resource?: { attributes?: MetricAttributes } | null;
    scopeMetrics: {
      scope?: { name?: string | null } | null;
      metrics: {
        name: string;
        sum?: {
          aggregationTemporality: number;
          dataPoints: {
            attributes: MetricAttributes;
            asInt?: { toString(): string } | number | null;
            asDouble?: number | null;
          }[];
        } | null;
      }[];
    }[];
  }[];
};
const exporterRequire = createRequire(
  import.meta.resolve("@opentelemetry/exporter-metrics-otlp-proto"),
);
const transformerRequire = createRequire(
  exporterRequire.resolve("@opentelemetry/otlp-transformer"),
);
const metricDecoder: { decode(body: Uint8Array): MetricExport } =
  transformerRequire("./generated/root.js").opentelemetry.proto.collector
    .metrics
    .v1.ExportMetricsServiceRequest;

Deno.test("Rust peer authorization loss remains observable while replacement setup is stalled", async () => {
  await withTrellisRuntime(async (runtime) => {
    const subjectContract = participants.TransportGrowthSubject.participant;
    const targetContract = participants.TransportGrowthTarget.participant;
    await runtime.contracts.apply({
      deployment: "coverage-target",
      contract: targetContract,
    });
    const targetInstance = await runtime.services.createInstance({
      deployment: "coverage-target",
      name: "coverage-target",
      contract: targetContract,
    });
    const target = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: targetContract,
      name: "coverage-target",
      seed: targetInstance.seed,
    }).orThrow();
    let advances = 0;
    let initialAdvanceDigest: string | undefined;
    await target.handleAdvance(({ context }) => {
      advances++;
      if (advances === 1) {
        assertEquals(context.caller.type, "verified");
        if (context.caller.type === "verified") {
          initialAdvanceDigest = context.caller.contextDigest;
        }
      }
      return Result.ok({});
    });
    await target.handleExtend(() => Result.ok({}));
    let progress = 0;
    await target.handleProgress(async ({ emit, signal }) => {
      while (!signal.aborted) {
        await emit({ value: String(++progress) });
        await new Promise((resolve) => setTimeout(resolve, 150));
      }
    });
    const targetExit = target.wait().catch(() => undefined);
    await runtime.contracts.install({ contract: subjectContract });
    const proposal = await runtime.contracts.requestApply({
      deployment: "coverage-subject",
      contract: subjectContract,
    });
    assert(proposal.status === "approval_required");
    await runtime.contracts.approveApply(proposal.pendingId, {
      excludeCapabilities: ["runtime-trellis.transport_growth@v1::extend"],
    });
    const instance = await runtime.services.createInstance({
      deployment: "coverage-subject",
      name: "coverage-subject",
      contract: subjectContract,
    });
    const privileged = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(
            runtime.workdir,
            "config",
            "trellis",
            "nats",
            "creds",
            "trellis-auth.creds",
          ),
        ),
      ),
    });
    const gate = runtime.nativeTransportGate();
    const attachments = async () =>
      (await runtime.callAdminRpc("authConnectionsList", {}) as {
        items: Attachment[];
      }).items;
    const telemetryOwner = crypto.randomUUID();
    type MetricSnapshot = {
      at: number;
      points: { name: string; attributes: MetricAttributes; value: number }[];
    };
    let metricSnapshot: MetricSnapshot | undefined;
    let telemetryError: unknown;
    let telemetryEndpoint = "";
    const collector = Deno.serve({
      hostname: "127.0.0.1",
      port: 0,
      onListen: ({ port }) => {
        telemetryEndpoint = `http://127.0.0.1:${port}/v1/metrics`;
      },
    }, async (request) => {
      try {
        assertEquals(new URL(request.url).pathname, "/v1/metrics");
        assertEquals(
          request.headers.get("content-type"),
          "application/x-protobuf",
        );
        const decoded = metricDecoder.decode(
          new Uint8Array(await request.arrayBuffer()),
        );
        const points = decoded.resourceMetrics.filter((resource) =>
          resource.resource?.attributes?.some((attribute) =>
            attribute.key === "service.instance.id" &&
            attribute.value?.stringValue === telemetryOwner
          )
        ).flatMap((resource) => resource.scopeMetrics).filter((scope) =>
          scope.scope?.name === "@oatscenter/trellis"
        ).flatMap((scope) => scope.metrics).flatMap((metric) => {
          if (!metric.name.startsWith("trellis.live.") || !metric.sum) {
            return [];
          }
          assertEquals(
            metric.sum.aggregationTemporality,
            2,
            "cumulative OTLP sum",
          );
          return metric.sum.dataPoints.map((point) => ({
            name: metric.name,
            attributes: point.attributes,
            value: Number(point.asInt ?? point.asDouble),
          }));
        });
        if (points.length) metricSnapshot = { at: Date.now(), points };
        return new Response(new Uint8Array(), {
          headers: { "content-type": "application/x-protobuf" },
        });
      } catch (cause) {
        telemetryError = cause;
        return new Response(null, { status: 400 });
      }
    });
    const total = (
      snapshot: MetricSnapshot,
      name: string,
      subset: Record<string, string> = {},
    ) =>
      snapshot.points.filter((point) =>
        point.name === name &&
        Object.entries({
          "trellis.kind": "live",
          "trellis.side": "provider",
          ...subset,
        }).every(([key, value]) =>
          point.attributes.some((attribute) =>
            attribute.key === key && attribute.value?.stringValue === value
          )
        )
      ).reduce((sum, point) => sum + point.value, 0);
    const child = new Deno.Command("setsid", {
      args: rustFixtureArgv("transport_growth_subject"),
      clearEnv: true,
      env: {
        ...Object.fromEntries(
          Object.entries(Deno.env.toObject()).filter(([key]) =>
            !key.startsWith("OTEL_")
          ),
        ),
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: instance.seed,
        OTEL_SDK_DISABLED: "false",
        OTEL_TRACES_EXPORTER: "none",
        OTEL_METRICS_EXPORTER: "otlp",
        OTEL_EXPORTER_OTLP_METRICS_ENDPOINT: telemetryEndpoint,
        OTEL_EXPORTER_OTLP_METRICS_PROTOCOL: "http/protobuf",
        OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE: "cumulative",
        OTEL_RESOURCE_ATTRIBUTES: `service.instance.id=${telemetryOwner}`,
        OTEL_METRIC_EXPORT_INTERVAL: "500",
      },
      stdin: "piped",
      stdout: "piped",
      stderr: "inherit",
    }).spawn();
    const stdin = child.stdin.getWriter();
    let exited = false;
    const status = child.status.then((result) => {
      exited = true;
      return result;
    });
    const lines: string[] = [];
    const output = (async () => {
      let buffer = "";
      for await (
        const chunk of child.stdout.pipeThrough(new TextDecoderStream())
      ) {
        buffer += chunk;
        const parts = buffer.split("\n");
        buffer = parts.pop() ?? "";
        lines.push(...parts.map((line) => line.trim()));
      }
    })();
    const send = (command: string) =>
      stdin.write(new TextEncoder().encode(`${command}\n`));
    const marker = (value: string) =>
      runtime.waitFor(() => {
        const error = lines.find((line) => line.includes("_ERROR "));
        if (error) throw new Error(error);
        return lines.some((line) => line.startsWith(value)) ? true : undefined;
      }, { timeoutMs: 30_000, intervalMs: 25 });
    type Observer = TrellisTestConnectedClient<
      typeof participants.LiveProbeCaller.participant
    >;
    let observer: Observer | undefined;
    let hold: ReturnType<typeof gate.armResponseHold> | undefined;
    let creates: ReturnType<typeof privileged.subscribe> | undefined;
    let controls: ReturnType<typeof privileged.subscribe> | undefined;
    let feed:
      | Awaited<ReturnType<ReturnType<Observer["watch"]>["orThrow"]>>
      | undefined;
    const tasks: Promise<unknown>[] = [];
    const events: { event: string; at: number; detail?: unknown }[] = [];
    const record = (event: string, detail?: unknown) => {
      events.push({ event, at: Date.now(), detail });
      console.log(JSON.stringify(events.at(-1)));
    };
    try {
      record("fixture-child", { pid: child.pid });
      await marker("TRANSPORT_GROWTH_ADVANCE_OK");
      // Open the peer shortly before the subject's persisted refresh deadline.
      // Preparation can outlast the peer's own refresh window, so select the
      // currently retained peer guard when the loss experiment actually starts.
      const database = createClient({
        url: `file:${
          join(runtime.workdir, "data", "trellis", "platform.sqlite")
        }`,
      });
      let subjectRefreshAt: number;
      try {
        const contexts = await database.execute({
          sql:
            "SELECT refresh_at FROM auth_authorization_contexts WHERE participant_id = ? ORDER BY issued_at DESC LIMIT 1",
          args: [subjectContract.identity],
        });
        assert(contexts.rows[0]);
        subjectRefreshAt = Number(contexts.rows[0].refresh_at) * 1000;
      } finally {
        database.close();
      }
      await runtime.waitFor(
        () => Date.now() >= subjectRefreshAt - 5_000 ? true : undefined,
        { timeoutMs: 35_000, intervalMs: 25 },
      );
      observer = await runtime.connectClient({
        name: "coverage-observer",
        contract: participants.LiveProbeCaller.participant,
      });
      const peer = (await attachments()).find((item) =>
        item.participantId === participants.LiveProbeCaller.participant.identity
      );
      assert(peer, "the ordinary observer must be admitted");
      const peerDatabase = createClient({
        url: `file:${
          join(runtime.workdir, "data", "trellis", "platform.sqlite")
        }`,
      });
      try {
        const timing = await peerDatabase.execute({
          sql:
            "SELECT issued_at, refresh_at, expires_at FROM auth_authorization_contexts WHERE context_digest = ?",
          args: [peer.contextDigest],
        });
        assert(
          timing.rows[0],
          "the admitted peer has persisted context timing",
        );
        record("peer-context-timing", {
          contextDigest: peer.contextDigest,
          issuedAt: Number(timing.rows[0]?.issued_at) * 1000,
          refreshAt: Number(timing.rows[0]?.refresh_at) * 1000,
          expiresAt: Number(timing.rows[0]?.expires_at) * 1000,
          subjectRefreshAt,
        });
      } finally {
        peerDatabase.close();
      }
      const controlContexts = new Set<string>();
      const controlRequests: string[] = [];
      let currentPeerDigest = peer.contextDigest;
      controls = privileged.subscribe("live.v1.route.>", {
        callback: (_error, message) => {
          if (!message.subject.includes(".Watch.observe.")) return;
          const context = message.headers?.get("authorization-context");
          if (context) {
            currentPeerDigest = context;
            controlRequests.push(context);
          }
          if (context && !controlContexts.has(context)) {
            controlContexts.add(context);
            record("accepted-watch-control-context", { context });
          }
        },
      });
      await privileged.flush();
      await marker("TRANSPORT_GROWTH_ADVANCE_OK");
      const baseline = (await attachments()).find((item) =>
        item.participantId === subjectContract.identity
      );
      assert(baseline);
      // Identify the fixture through its actual accepted Advance, never through
      // a connection ordinal or a socket that merely happens to own Watch.
      const advanceRoutes = gate.connections().flatMap((connection) =>
        connection.subs.filter((sub) => sub.subject.endsWith(".Advance"))
      );
      assertEquals(advanceRoutes.length, 1);
      assert(initialAdvanceDigest);
      const advanceOwners = gate.connections().filter((connection) =>
        connection.outboundContexts.some((outbound) =>
          outbound.subject === advanceRoutes[0].subject &&
          outbound.context === initialAdvanceDigest
        )
      );
      record("initial-advance-owner-selection", {
        advanceSubject: advanceRoutes[0].subject,
        signedContextDigest: initialAdvanceDigest,
        baselineDigest: baseline.contextDigest,
        runtimeConnectionId: baseline.runtimeConnectionId,
        candidates: advanceOwners,
      });
      assertEquals(advanceOwners.length, 1);
      const rustOwner = advanceOwners[0];
      assert(!rustOwner.closed, "the actual Advance owner must still be live");
      assertEquals(
        initialAdvanceDigest,
        baseline.contextDigest,
        "the initial signed Advance must still identify current authority, not a stale generation",
      );
      const initialWatchRoutes = rustOwner.subs.filter((sub) =>
        sub.subject.startsWith("live.v1.route.") &&
        sub.subject.endsWith(".Watch")
      );
      assertEquals(initialWatchRoutes.length, 1);
      const watchSubject = initialWatchRoutes[0].subject;
      const sockets = admittedConnections(
        await readRuntimeBrokerInventory(runtime),
        new Set([baseline.contextDigest]),
      );
      assertEquals(sockets.length, 1);
      const owner = sockets[0];
      let oldInfo: ConsumerInfo | undefined;
      hold = gate.armResponseHold(
        "$JS.API.CONSUMER.INFO.",
        rustOwner.id,
        (body) => {
          const info = JSON.parse(
            new TextDecoder().decode(body),
          ) as ConsumerInfo;
          if (
            !info.push_bound ||
            !info.config?.filter_subject.endsWith(
              `.revocation.${peer.contextDigest}`,
            )
          ) return false;
          oldInfo = info;
          return true;
        },
      );
      let watchDelivery:
        | Awaited<ReturnType<typeof gate.waitForDelivery>>
        | undefined;
      gate.waitForDelivery(watchSubject).then((delivery) => {
        watchDelivery = delivery;
      });
      const opening = observer.watch({
        runId: crypto.randomUUID(),
        streamId: "coverage-loss",
      }).orThrow();
      let openingError: unknown;
      tasks.push(opening.catch((cause) => {
        openingError = cause;
      }));
      let initialHeld: Awaited<typeof hold.held> | undefined;
      hold.held.then((value) => {
        initialHeld = value;
      });
      await runtime.waitFor(() => {
        if (openingError) throw openingError;
        return initialHeld;
      }, {
        timeoutMs: 10_000,
        intervalMs: 10,
      });
      record("initial-peer-info-held", {
        heldInfo: initialHeld,
        consumer: oldInfo,
        advanceOwner: rustOwner.id,
        signedAdvanceDigest: initialAdvanceDigest,
        baselineDigest: baseline.contextDigest,
        runtimeConnectionId: baseline.runtimeConnectionId,
        brokerOwner: owner,
        watchSubject,
        watchDelivery,
        sockets: gate.connections(),
      });
      assert(oldInfo);
      let old = oldInfo;
      const oldConnection = initialHeld!.connectionId;
      assertEquals(oldConnection, rustOwner.id);
      assert(watchDelivery, "the opening Watch must actually reach a provider");
      assertEquals(
        watchDelivery.connectionId,
        oldConnection,
        "the actual Watch delivery and held Rust peer consumer must share the old physical owner",
      );
      const watchRoutes = gate.connection(oldConnection)!.subs.filter((sub) =>
        sub.subject.startsWith("live.v1.route.") &&
        sub.subject.endsWith(".Watch")
      );
      assertEquals(watchRoutes.length, 1);
      assertEquals(watchRoutes[0].subject, watchSubject);
      record("native-owner", {
        brokerKey: brokerConnectionKey(owner),
        proxyConnection: oldConnection,
        runtimeConnectionId: baseline.runtimeConnectionId,
      });
      assert(
        gate.connections().find((connection) => connection.id === oldConnection)
          ?.subs.some((sub) => sub.subject === old.config.deliver_subject),
        "old coverage consumer must deliver on the exact Rust owner socket",
      );
      record("old-peer-watch", old);
      await hold.release();
      feed = await opening;
      let frames = 0;
      let lastFrameAt = 0;
      let terminal: Awaited<typeof feed.closed> | undefined;
      let heldAt = 0;
      feed.closed.then((end) => {
        terminal = end;
        // This is the consumer's own peer/liveness outcome. Caller coverage loss
        // does not permit the provider to sign an unsolicited remote END.
        record("remote-consumer-terminal", {
          reason: end.reason,
          code: end.error?.code,
        });
      });
      tasks.push(
        (async () => {
          for await (const _frame of feed!) {
            frames++;
            lastFrameAt = Date.now();
          }
        })().catch(() => undefined),
      );
      await runtime.waitFor(() => frames > 0 ? true : undefined);
      await send("OBSERVE");
      await marker("TRANSPORT_GROWTH_OBSERVING");
      await runtime.waitFor(() =>
        lines.some((line) => line.startsWith("TRANSPORT_GROWTH_OBSERVED "))
          ? true
          : undefined
      );

      // Each Consumer.info request has async-nats' ordinary ten-second client
      // budget. Release only expired replies, then rearm before the paced retry.
      // A held replacement is never released while it could still initialize.
      let replacement: ConsumerInfo | undefined;
      let retries = 0;
      let retryFailure: unknown;
      const infoRequests = new Map<string, number>();
      let replacementConnection: number | undefined;
      let replacementHeld: Awaited<typeof hold.held> | undefined;
      let candidatesLogged = 0;
      const arm = () => {
        replacementHeld = undefined;
        hold = gate.armResponseHold(
          "$JS.API.CONSUMER.INFO.",
          replacementConnection,
          (body) => {
            const info = JSON.parse(
              new TextDecoder().decode(body),
            ) as ConsumerInfo;
            if (candidatesLogged++ < 8) {
              record("replacement-info-candidate", {
                pid: child.pid,
                consumer: info,
                samePeer:
                  info.config?.filter_subject === old.config.filter_subject,
                sockets: gate.connections().filter((connection) =>
                  connection.subs.some((sub) =>
                    sub.subject === info.config?.deliver_subject
                  )
                ).map((connection) => ({
                  id: connection.id,
                  closed: connection.closed,
                  watchIngress: connection.subs.some((sub) =>
                    sub.subject === watchSubject
                  ),
                  requestedInfo: connection.outboundContexts.some((outbound) =>
                    outbound.subject ===
                      `$JS.API.CONSUMER.INFO.${info.stream_name}.${info.name}`
                  ),
                  recentRequests: connection.outboundContexts.slice(-8),
                })),
                childLines: lines.slice(-8),
              });
            }
            if (
              !info.push_bound || info.name === old.name ||
              !info.config?.filter_subject.endsWith(
                `.revocation.${currentPeerDigest}`,
              )
            ) return false;
            const owners = gate.connections().filter((connection) =>
              !connection.closed && connection.id !== oldConnection &&
              connection.subs.some((sub) => sub.subject === watchSubject) &&
              connection.subs.some((sub) =>
                sub.subject === info.config.deliver_subject
              )
            );
            if (owners.length !== 1) return false;
            replacementConnection = owners[0].id;
            replacement = info;
            return true;
          },
        );
        hold.held.then((value) => {
          replacementHeld = value;
          heldAt = Date.now();
          record("replacement-info-held", replacement);
        });
      };
      // Observe the broker's real retry CREATE, after the previous INFO timed
      // out. Disarm/rearm synchronously before that consumer's later INFO;
      // waiting ten seconds from reply arrival can miss the 100ms retry gap.
      creates = privileged.subscribe(
        "$JS.API.CONSUMER.>",
        {
          callback: (_error, message) => {
            if (
              message.subject.startsWith(
                `$JS.API.CONSUMER.INFO.${old.stream_name}.`,
              )
            ) {
              infoRequests.set(message.subject, Date.now());
              return;
            }
            if (
              !message.subject.startsWith(
                `$JS.API.CONSUMER.CREATE.${old.stream_name}.`,
              )
            ) return;
            const request = JSON.parse(
              new TextDecoder().decode(message.data),
            ) as { config?: { filter_subject?: string } };
            if (
              !replacementHeld ||
              request.config?.filter_subject !==
                replacement?.config.filter_subject
            ) return;
            if (
              !gate.connection(replacementHeld.connectionId)!.outboundContexts
                .some((outbound) => outbound.subject === message.subject)
            ) return;
            try {
              const requestedAt = infoRequests.get(
                replacementHeld.requestSubject,
              );
              assert(
                requestedAt !== undefined,
                "observe the exact held INFO request",
              );
              assert(
                Date.now() >= requestedAt + 10_000,
                "the exact Rust INFO must expire before releasing its reply",
              );
              assert(retries < 2, "bounded replacement retry chain exhausted");
              retries++;
              record("replacement-setup-retry", {
                previousHeldMs: Date.now() - heldAt,
              });
              const released = hold!.release();
              arm();
              tasks.push(released.catch((cause) => {
                retryFailure = cause;
              }));
            } catch (cause) {
              retryFailure = cause;
            }
          },
        },
      );
      await privileged.flush();
      arm();
      record("replacement-growth-requested", { pid: child.pid });
      await runtime.contracts.apply({
        deployment: "coverage-subject",
        contract: subjectContract,
      });
      await runtime.waitFor(() => replacementHeld, {
        timeoutMs: 45_000,
        intervalMs: 10,
      });
      assert(
        replacementHeld!.connectionId !== oldConnection,
        "replacement must warm on the new physical attachment",
      );
      const firstHeldAt = heldAt;
      const beforeTimeout = frames;
      await runtime.waitFor(
        () => {
          if (retryFailure) throw retryFailure;
          return retries === 1 && replacementHeld ? true : undefined;
        },
        { timeoutMs: 13_000, intervalMs: 10 },
      );
      assertEquals(
        terminal,
        undefined,
        "replacement setup failure must not invalidate healthy old coverage",
      );
      assert(
        frames > beforeTimeout,
        "accepted old Watch must keep delivering through setup failure",
      );
      record("setup-failure-retry-held", { elapsedMs: heldAt - firstHeldAt });
      const lossPeerDigest = currentPeerDigest;
      const lossControlOffset = controlRequests.length - 1;
      assert(
        lossControlOffset >= 0,
        "observe the live caller's signed control",
      );
      assertEquals(
        replacement?.config.filter_subject,
        `$KV.trellis_authorization_contexts.revocation.${lossPeerDigest}`,
        "withhold replacement coverage for the currently retained peer",
      );
      const listed = JSON.parse(
        new TextDecoder().decode(
          (await privileged.request(
            `$JS.API.CONSUMER.LIST.${old.stream_name}`,
            "{}",
            { timeout: 2_000 },
          )).data,
        ),
      ) as { consumers: ConsumerInfo[] };
      const activeOld = listed.consumers.filter((info) =>
        info.push_bound &&
        info.config.filter_subject === replacement!.config.filter_subject &&
        gate.connection(oldConnection)!.subs.some((sub) =>
          sub.subject === info.config.deliver_subject
        )
      );
      assertEquals(
        activeOld.length,
        1,
        "the current peer must have one bound guard on the original Watch owner",
      );
      old = activeOld[0];
      const identityDatabase = createClient({
        url: `file:${
          join(runtime.workdir, "data", "trellis", "platform.sqlite")
        }`,
      });
      try {
        const identity = await identityDatabase.execute({
          sql: `SELECT
            original.session_public_key = current.session_public_key
            AND original.principal_id = current.principal_id
            AND original.principal_kind = current.principal_kind
            AND original.participant_id = current.participant_id
            AND original.inbox_prefix = current.inbox_prefix AS same_identity
            FROM auth_authorization_contexts AS original
            JOIN auth_authorization_contexts AS current
              ON current.context_digest = ?
            WHERE original.context_digest = ?`,
          args: [lossPeerDigest, peer.contextDigest],
        });
        assertEquals(
          Number(identity.rows[0]?.same_identity),
          1,
          "renewal must preserve the accepted caller identity and reply boundary",
        );
      } finally {
        identityDatabase.close();
      }
      record("loss-peer-guard-selected", {
        openingDigest: peer.contextDigest,
        currentDigest: lossPeerDigest,
        consumer: old,
        oldConnection,
      });
      assert(
        listed.consumers.some((info) =>
          info.name === old.name && info.created === old.created &&
          info.push_bound &&
          info.config.filter_subject === old.config.filter_subject &&
          info.config.deliver_subject === old.config.deliver_subject
        ),
        "delete only the exact still-bound current peer consumer",
      );
      const flushAt = Date.now();
      await send("FLUSH_TELEMETRY");
      await marker("TRANSPORT_GROWTH_TELEMETRY_FLUSHED");
      const metricBaseline = await runtime.waitFor(() => {
        if (telemetryError) throw telemetryError;
        const snapshot = metricSnapshot;
        return snapshot && snapshot.at >= flushAt ? snapshot : undefined;
      }, { timeoutMs: 2_000, intervalMs: 10 });
      assertEquals(total(metricBaseline, "trellis.live.sessions"), 1);
      assertEquals(
        total(metricBaseline, "trellis.live.sessions", {
          "trellis.phase": "active",
        }),
        1,
        "the isolated Rust provider owns the accepted Watch before deletion",
      );
      const beforeLoss = total(metricBaseline, "trellis.live.ends", {
        "trellis.reason": "authorization_lost",
      });
      record("provider-metric-baseline", {
        at: metricBaseline.at,
        authorizationLostEnds: beforeLoss,
        providerSessions: total(metricBaseline, "trellis.live.sessions"),
      });
      assertEquals(
        currentPeerDigest,
        lossPeerDigest,
        "the selected peer guard must still serve the accepted Watch at deletion",
      );
      const deletion = await privileged.request(
        `$JS.API.CONSUMER.DELETE.${old.stream_name}.${old.name}`,
        "",
        { timeout: 2_000 },
      );
      assertEquals(
        JSON.parse(new TextDecoder().decode(deletion.data)).success,
        true,
      );
      const deletedAt = Date.now();
      record("old-peer-consumer-deleted");
      // Missing heartbeat is detected by the real old watch. If that deadline
      // straddles a request timeout, chain one further withheld retry, not a
      // larger request/heartbeat budget or a synthetic deletion notification.
      const deadline = deletedAt + 12_000;
      let localLoss:
        | {
          at: number;
          requestDeadline: number;
          retries: number;
          authorizationLostEndsDelta: number;
          providerSessions: number;
        }
        | undefined;
      while (!localLoss && Date.now() < deadline) {
        if (retryFailure) throw retryFailure;
        if (telemetryError) throw telemetryError;
        const snapshot = metricSnapshot;
        if (
          snapshot && snapshot.at > deletedAt &&
          total(snapshot, "trellis.live.ends", {
                  "trellis.reason": "authorization_lost",
                }) - beforeLoss === 1 &&
          total(snapshot, "trellis.live.sessions") === 0
        ) {
          assert(
            replacementHeld,
            "replacement INFO remains withheld at local cleanup",
          );
          assert(
            snapshot.at >= heldAt,
            "local loss evidence belongs to the currently withheld retry, not an expired request",
          );
          const requestAt = infoRequests.get(replacementHeld.requestSubject);
          assert(
            requestAt !== undefined,
            "observe the exact real held INFO request",
          );
          localLoss = {
            at: snapshot.at,
            requestDeadline: requestAt + 10_000,
            retries,
            authorizationLostEndsDelta: total(snapshot, "trellis.live.ends", {
              "trellis.reason": "authorization_lost",
            }) - beforeLoss,
            providerSessions: total(snapshot, "trellis.live.sessions"),
          };
        }
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      record("loss-deadline-state", {
        frames,
        retries,
        terminal,
        localLoss,
        lastFrameAt,
        lastFrameAfterDeleteMs: lastFrameAt - deletedAt,
        heldAgeMs: Date.now() - heldAt,
      });
      assertEquals(
        [...new Set(controlRequests.slice(lossControlOffset))],
        [lossPeerDigest],
        "the accepted Watch must retain the selected peer digest throughout the loss experiment",
      );
      assertEquals(
        gate.connections().find((connection) => connection.id === oldConnection)
          ?.closed,
        false,
        "peer coverage loss must not disconnect its owner socket",
      );
      const inventory = await readRuntimeBrokerInventory(runtime, {
        requiredServerIds: [owner.server],
      });
      assert(
        inventory.some((item) =>
          brokerConnectionKey(item) === brokerConnectionKey(owner)
        ),
        "the exact native owner remains broker-admitted after peer loss",
      );
      const previous = advances;
      const observed = lines.filter((line) =>
        line.startsWith("TRANSPORT_GROWTH_OBSERVED ")
      ).length;
      await send("ADVANCE");
      await runtime.waitFor(() => advances > previous ? true : undefined, {
        timeoutMs: 5_000,
        intervalMs: 10,
      });
      await runtime.waitFor(
        () =>
          lines.filter((line) => line.startsWith("TRANSPORT_GROWTH_OBSERVED "))
              .length > observed
            ? true
            : undefined,
        { timeoutMs: 2_000, intervalMs: 10 },
      );
      const current = (await attachments()).filter((item) =>
        item.participantId === subjectContract.identity
      );
      assert(
        current.every((item) =>
          item.runtimeConnectionId === baseline.runtimeConnectionId
        ),
      );
      record("unrelated-own-rpc-succeeded", {
        lossAfterDeleteMs: Date.now() - deletedAt,
        retries,
        pinnedProgressFramesBefore: observed,
        pinnedProgressFramesAfter: lines.filter((line) =>
          line.startsWith("TRANSPORT_GROWTH_OBSERVED ")
        ).length,
      });
      assert(
        localLoss,
        "Rust provider must commit authorization_lost and finish local cleanup while replacement INFO is withheld",
      );
      assert(
        localLoss.at < localLoss.requestDeadline,
        "exported local terminal and cleanup must precede the held request's default timeout",
      );
      record("provider-local-authorization-loss-and-cleanup", localLoss);
      creates.unsubscribe();
      await hold!.release();
      hold = undefined;
      // Only now release the independently pinned outgoing session, allowing
      // ordinary G1 retirement before opening the recovery session on G2.
      await send("OBSERVE_CLOSE");
      await marker("TRANSPORT_GROWTH_OBSERVED_CLOSED");
      await runtime.waitFor(async () =>
        (await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [owner.server],
          })).some((item) =>
            brokerConnectionKey(item) === brokerConnectionKey(owner)
          )
          ? undefined
          : true
      );
      // The ended session retains its failed caller guard; restoring authority
      // must not resurrect that guard. Recover with a fresh ordinary caller
      // context, after the provider's local outcome has already been proved.
      await observer.connection.close();
      observer = await runtime.connectClient({
        name: "coverage-recovery-observer",
        contract: participants.LiveProbeCaller.participant,
      });
      const fresh = await observer.watch({
        runId: crypto.randomUUID(),
        streamId: "coverage-recovered",
      }).orThrow();
      let freshFrames = 0;
      const freshTask = (async () => {
        for await (const _frame of fresh) freshFrames++;
      })().catch(() => undefined);
      tasks.push(freshTask);
      try {
        await runtime.waitFor(() => freshFrames > 0 ? true : undefined);
        const receipt = await fresh.close().orThrow();
        assertEquals(receipt.remote, "confirmed");
        assertEquals(receipt.cleanup, "complete");
        // Local provider terminalization is established above by its production
        // owner metrics; no signed remote authorization-loss END is claimed.
        record("new-watch-recovered-and-closed");
      } finally {
        await fresh.close().orThrow().catch(() => undefined);
      }
      await send("CLOSE");
      await marker("TRANSPORT_GROWTH_CLOSED");
    } catch (cause) {
      record("fixture-failure", {
        pid: child.pid,
        cause: String(cause),
        childLines: lines.slice(-12),
        sockets: gate.connections().map((connection) => ({
          id: connection.id,
          closed: connection.closed,
          watchIngress: connection.subs.filter((sub) =>
            sub.subject.endsWith(".Watch")
          ),
          recentRequests: connection.outboundContexts.slice(-12),
          recentDeliveries: connection.deliveries.slice(-8),
        })),
      });
      throw cause;
    } finally {
      controls?.unsubscribe();
      creates?.unsubscribe();
      await hold?.release().catch(() => undefined);
      await feed?.close().orThrow().catch(() => undefined);
      await observer?.connection.close().catch(() => undefined);
      await privileged.close();
      await target.connection.close().catch(() => undefined);
      await targetExit;
      await stdin.close().catch(() => undefined);
      if (!exited) {
        try {
          Deno.kill(-child.pid, "SIGKILL");
        } catch { /* already exited */ }
      }
      await Promise.allSettled([status, output, ...tasks]);
      await collector.shutdown();
    }
  }, {
    interruptibleNativeProxy: true,
    authorization: {
      contextLifetimeSeconds: 120,
      refreshLeadSeconds: 60,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 65,
    },
  });
});
