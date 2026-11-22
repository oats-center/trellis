/**
 * The reference a caller is shown for a failure finds the failure's one log
 * line in the process that failed, and that line names the request, the trace
 * and the cause. Each case reads the real output of a TypeScript service, a
 * Rust service or trellis-server. RPCs, Operations, jobs and event deliveries
 * are covered in both languages.
 */
import assert from "node:assert/strict";
import { execPath } from "node:process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { context, trace, propagation } from "@opentelemetry/api";
import { AsyncLocalStorageContextManager } from "@opentelemetry/context-async-hooks";
import { W3CTraceContextPropagator } from "@opentelemetry/core";
import {
  BasicTracerProvider,
  InMemorySpanExporter,
  SimpleSpanProcessor,
} from "@opentelemetry/sdk-trace-base";
import { codecs } from "@oatscenter/trellis/generated";
import { connectNativeClient } from "@oatscenter/trellis/native";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { ulid } from "ulid";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

// Callers in this process trace their requests, so each request and event
// carries a W3C trace context the way a traced application's does.
const spans = new InMemorySpanExporter();
trace.setGlobalTracerProvider(
  new BasicTracerProvider({
    spanProcessors: [new SimpleSpanProcessor(spans)],
  }),
);
propagation.setGlobalPropagator(new W3CTraceContextPropagator());
context.setGlobalContextManager(new AsyncLocalStorageContextManager().enable());

const CAUSE = "the fixture's store is unreachable";

/** The IDs a caller can quote from a failed call. */
function references(error: unknown): {
  id: string;
  requestId: string;
  traceId: string;
} {
  assert(
    typeof error === "object" &&
      error !== null &&
      "toSerializable" in error &&
      typeof error.toSerializable === "function",
    String(error),
  );
  const data = error.toSerializable() as {
    id?: unknown;
    traceId?: unknown;
    context?: { requestId?: unknown };
    remoteError?: {
      id?: unknown;
      traceId?: unknown;
      context?: { requestId?: unknown };
    };
  };
  const wire = data.remoteError ?? data;
  const { id, traceId } = wire;
  const requestId = wire.context?.requestId;
  assert.equal(typeof id, "string", JSON.stringify(data));
  assert.equal(typeof requestId, "string", JSON.stringify(data));
  assert.equal(typeof traceId, "string", JSON.stringify(data));
  // The trace is the caller's own, not one the server made up.
  assert(
    spans
      .getFinishedSpans()
      .some((span) => span.spanContext().traceId === traceId),
    `trace ${String(traceId)} was not started by this caller`,
  );
  return {
    id: id as string,
    requestId: requestId as string,
    traceId: traceId as string,
  };
}

/** The output lines that carry `id`, once there are `count` of them. */
async function linesWith(
  runtime: TrellisTestRuntime,
  output: () => string,
  id: string,
  count = 1,
): Promise<string[]> {
  const matching = () =>
    output()
      .split("\n")
      .filter((line) => line.includes(id));
  await runtime.waitFor(() => matching().length >= count, {
    timeoutMs: 30_000,
  });
  const lines = matching();
  assert.equal(lines.length, count, lines.join("\n"));
  return lines;
}

/** The one output line that carries `id`, waiting for it to be written. */
async function lineWith(
  runtime: TrellisTestRuntime,
  output: () => string,
  id: string,
): Promise<string> {
  return (await linesWith(runtime, output, id))[0]!;
}

/** Whether a trace was started by a caller in this process. */
function tracedHere(traceId: unknown): boolean {
  return spans
    .getFinishedSpans()
    .some((span) => span.spanContext().traceId === traceId);
}

/** The value of `field` in a Rust `fmt` log line. */
function rustField(line: string, field: string): string | undefined {
  return new RegExp(` ${field}="?([^" ]+)"?`).exec(line)?.[1];
}

/** Reads a child's stdout and stderr into one string. */
function captured(child: {
  stdout: NodeJS.ReadableStream;
  stderr: NodeJS.ReadableStream;
}): () => string {
  let output = "";
  child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
  child.stderr.on("data", (chunk: Buffer) => (output += chunk.toString()));
  return () => output;
}

/** Publishes inside a span, as a traced application does. */
async function publishTraced(publish: () => Promise<unknown>): Promise<void> {
  await trace
    .getTracer("failure-reference-test")
    .startActiveSpan("publish", async (span) => {
      try {
        await publish();
      } finally {
        span.end();
      }
    });
}

/** A valid W3C trace ID. */
const TRACE_ID = /^(?!0{32})[0-9a-f]{32}$/;

// The durable consumer of the event fixtures delivers an event three times.
const EVENT_DELIVERIES = 3;

test("TypeScript RPC, Operation, job and event failures are each logged once under their reference", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "failing-echo",
      contract: participants.Provider.participant,
    });
    const eventIdentity = await runtime.registerService({
      name: "events-ts",
      contract: participants.EventService.participant,
    });
    const events = spawnTracked(
      execPath,
      [
        fileURLToPath(
          new URL(
            "../../integration/fixtures/runtime/events.ts",
            import.meta.url,
          ),
        ),
        runtime.trellisUrl,
        eventIdentity.seed,
      ],
      { env: { ...process.env, NODE_EXTRA_CA_CERTS: runtime.caCertificate } },
    );
    const eventOutput = captured(events);
    let output = "";
    const child = spawnTracked(
      execPath,
      [
        fileURLToPath(
          new URL(
            "../../integration/fixtures/runtime/failing_echo.ts",
            import.meta.url,
          ),
        ),
        runtime.trellisUrl,
        identity.seed,
      ],
      { env: { ...process.env, NODE_EXTRA_CA_CERTS: runtime.caCertificate } },
    );
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => (output += chunk.toString()));
    try {
      const client = await runtime.connectClient({
        name: "failure-caller",
        contract: apps.Caller.request,
      });
      await runtime.waitFor(
        async () => (await client.echo({ value: "ready" })).isOk(),
        { timeoutMs: 60_000 },
      );
      const failed = await client.echo({ value: "fail" });
      assert(failed.isErr());
      const reference = references(failed.error);

      const line = JSON.parse(
        await lineWith(runtime, () => output, reference.id),
      ) as Record<string, unknown>;
      assert.equal(line.error_id, reference.id);
      assert.equal(line.request_id, reference.requestId);
      assert.equal(line.trace_id, reference.traceId);
      assert(String(line.route).endsWith("Echo"), String(line.route));
      assert.equal(typeof line.caller_principal, "string");
      assert.equal(typeof line.caller_session, "string");
      assert.deepEqual(line.cause, [
        { type: "Error", message: `echo failed: ${CAUSE}` },
      ]);
      // The cause stays in the log: the caller's error never carries it.
      assert(!JSON.stringify(failed.error).includes(CAUSE));

      // A failed Operation's snapshot carries the error ID and trace; the line
      // adds the Operation ID, the caller that started it and the cause.
      const work = await client.work({ value: "fail" }).start().orThrow();
      const failedWork = await work.wait().orThrow();
      assert.equal(failedWork.state, "failed");
      const workError = failedWork.error as
        { id?: unknown; traceId?: unknown } | undefined;
      assert.equal(typeof workError?.id, "string", JSON.stringify(failedWork));
      assert(tracedHere(workError?.traceId), JSON.stringify(failedWork));
      const workLine = JSON.parse(
        await lineWith(runtime, () => output, String(workError?.id)),
      ) as Record<string, unknown>;
      assert.equal(workLine.operation_id, failedWork.id);
      assert.equal(workLine.trace_id, workError?.traceId);
      assert(String(workLine.route).endsWith("Work"), String(workLine.route));
      assert.equal(workLine.caller_principal, line.caller_principal);
      assert.equal(workLine.caller_session, line.caller_session);
      assert.deepEqual(workLine.cause, [
        { type: "Error", message: `work failed: ${CAUSE}` },
      ]);
      assert(!JSON.stringify(failedWork).includes(CAUSE));

      // A failed job's reference is its job ID. The job runs in a trace of its
      // own: this service has no tracer, so the creating request's trace is not
      // carried into it.
      const jobId = (await client.echo({ value: "job" }).orThrow()).value;
      const jobLine = JSON.parse(
        await lineWith(runtime, () => output, `"job_id":"${jobId}"`),
      ) as Record<string, unknown>;
      assert.match(String(jobLine.trace_id), TRACE_ID, JSON.stringify(jobLine));
      assert.equal(typeof jobLine.request_id, "string");
      assert.deepEqual(jobLine.cause, [
        { type: "Error", message: `job failed: ${CAUSE}` },
      ]);

      // Each delivery of a failing event logs one line under the event's ID,
      // with the publisher's trace.
      const alpha = await runtime.connectClient({
        name: "failing-alpha",
        contract: apps.Alpha.request,
      });
      await runtime.waitFor(
        async () => (await alpha.observed({}, { timeout: 1000 })).isOk(),
        { timeoutMs: 60_000 },
      );
      await publishTraced(() =>
        alpha.publishAlpha({ site: "one", value: "fail" }).orThrow(),
      );
      await runtime.waitFor(() => eventOutput().includes('"event_id":"'));
      const eventId = /"event_id":"([^"]+)"/.exec(eventOutput())?.[1];
      assert(eventId, eventOutput());
      const deliveries = (
        await linesWith(
          runtime,
          eventOutput,
          `"event_id":"${eventId}"`,
          EVENT_DELIVERIES,
        )
      ).map((text) => JSON.parse(text) as Record<string, unknown>);
      assert.deepEqual(
        deliveries.map((delivery) => delivery.delivery),
        [1, 2, 3],
      );
      for (const delivery of deliveries) {
        assert(
          String(delivery.route).endsWith("Alpha"),
          String(delivery.route),
        );
        assert(tracedHere(delivery.trace_id), JSON.stringify(delivery));
        assert.deepEqual(delivery.cause, [
          { type: "Error", message: "fixture handler failure" },
        ]);
      }
    } finally {
      child.kill("SIGTERM");
      events.kill("SIGTERM");
      await child.exited;
      await events.exited;
    }
  });
});

test("Rust RPC, Operation, job and event failures are each logged once under their reference", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    const eventIdentity = await runtime.registerService({
      name: "events",
      contract: participants.EventService.participant,
    });
    const events = spawnTracked(rustFixtureArgv("events")[0]!, [], {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: eventIdentity.seed,
        REVERSE: "false",
        RUST_LOG: "info",
        NO_COLOR: "1",
      },
    });
    const eventOutput = captured(events);
    let output = "";
    const child = spawnTracked(
      rustFixtureArgv("trellis-runtime-acceptance")[0]!,
      [],
      {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
          TRELLIS_IDENTITY_SEED: identity.seed,
          RUST_LOG: "info",
          NO_COLOR: "1",
        },
      },
    );
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => (output += chunk.toString()));
    try {
      const client = await runtime.connectClient({
        name: "failure-caller",
        contract: apps.Caller.request,
      });
      await runtime.waitFor(
        async () => (await client.echo({ value: "from TypeScript" })).isOk(),
        { timeoutMs: 120_000 },
      );
      const failed = await client.echo({ value: "fail" });
      assert(failed.isErr());
      const reference = references(failed.error);

      const line = await lineWith(runtime, () => output, reference.id);
      assert.match(line, / ERROR /);
      assert(line.includes(`request_id="${reference.requestId}"`), line);
      assert(line.includes(`trace_id="${reference.traceId}"`), line);
      assert(line.includes(`error=echo failed: ${CAUSE}`), line);
      assert(line.includes("caller_principal="), line);
      assert(!JSON.stringify(failed.error).includes(CAUSE));

      const work = await client.work({ value: "failure" }).start().orThrow();
      const failedWork = await work.wait().orThrow();
      assert.equal(failedWork.state, "failed");
      const workError = failedWork.error as
        { id?: unknown; traceId?: unknown } | undefined;
      assert.equal(typeof workError?.id, "string", JSON.stringify(failedWork));
      assert(tracedHere(workError?.traceId), JSON.stringify(failedWork));
      const workLine = await lineWith(
        runtime,
        () => output,
        String(workError?.id),
      );
      assert.match(workLine, / ERROR /);
      assert.equal(rustField(workLine, "operation_id"), failedWork.id);
      assert.equal(rustField(workLine, "trace_id"), workError?.traceId);
      assert.equal(
        rustField(workLine, "caller_principal"),
        rustField(line, "caller_principal"),
      );
      assert(workLine.includes("domain failure"), workLine);
      assert(!JSON.stringify(failedWork).includes("domain failure"));

      const jobId = (await client.echo({ value: "job" }).orThrow()).value;
      const jobLine = await lineWith(
        runtime,
        () => output,
        `job_id="${jobId}"`,
      );
      assert.match(jobLine, / ERROR /);
      assert.match(rustField(jobLine, "trace_id") ?? "", TRACE_ID, jobLine);
      assert(jobLine.includes("unexpected fail"), jobLine);

      const alpha = await runtime.connectClient({
        name: "failing-alpha",
        contract: apps.Alpha.request,
      });
      await runtime.waitFor(
        async () => (await alpha.observed({}, { timeout: 1000 })).isOk(),
        { timeoutMs: 120_000 },
      );
      await publishTraced(() =>
        alpha.publishAlpha({ site: "one", value: "fail" }).orThrow(),
      );
      await runtime.waitFor(() => eventOutput().includes("event_id="));
      const eventId = rustField(
        eventOutput()
          .split("\n")
          .find((text) => text.includes("event_id="))!,
        "event_id",
      );
      assert(eventId, eventOutput());
      const deliveries = await linesWith(
        runtime,
        eventOutput,
        `event_id="${eventId}"`,
        EVENT_DELIVERIES,
      );
      assert.deepEqual(
        deliveries.map((text) => rustField(text, "delivery")),
        ["1", "2", "3"],
      );
      for (const delivery of deliveries) {
        assert(tracedHere(rustField(delivery, "trace_id")), delivery);
        assert(delivery.includes("fixture handler failure"), delivery);
      }
    } finally {
      child.kill("SIGTERM");
      events.kill("SIGTERM");
      await child.exited;
      await events.exited;
    }
  });
});

test("a refused Auth call is logged once in trellis-server under the reference its caller gets", async () => {
  await withTrellisRuntime(async (runtime) => {
    const user = await runtime.createUser({ name: "Reference Reader" });
    await runtime.grantUserCapabilities({
      user,
      capabilityIds: apps.DirectoryCaller.request.requiredCapabilities,
    });
    const login = await runtime.registerClient({
      name: "Reference reader",
      contract: apps.DirectoryCaller.request,
      user,
    });
    const client = await connectNativeClient({
      ...login,
      request: apps.DirectoryCaller.request,
    }).orThrow();
    try {
      const missing = codecs.ulid.decode(ulid());
      const failed = await client.usersGet({ userId: missing });
      assert(failed.isErr());
      const reference = references(failed.error);

      const line = JSON.parse(
        await lineWith(
          runtime,
          () => runtime.controlPlaneOutput(),
          reference.id,
        ),
      ) as { level: string; fields: Record<string, unknown> };
      assert.equal(line.level, "INFO");
      assert.equal(line.fields.error_id, reference.id);
      assert.equal(line.fields.request_id, reference.requestId);
      assert.equal(line.fields.trace_id, reference.traceId);
      assert(
        String(line.fields.route).endsWith("Users.Get"),
        String(line.fields.route),
      );
      assert.equal(line.fields.code, "not_found");
      assert.equal(
        line.fields.caller_principal,
        codecs.ulid.decode(user.principalId),
      );
      assert(
        String(line.fields.error).includes(missing),
        String(line.fields.error),
      );
    } finally {
      await client.connection.close();
    }
  });
});
