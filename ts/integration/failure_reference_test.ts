/**
 * The reference a caller is shown for a failure finds the failure's one log
 * line in the process that failed, and that line names the request, the trace
 * and the cause. Each case reads the real output of a TypeScript service, a
 * Rust service or trellis-server.
 */
import assert from "node:assert/strict";
import { execPath } from "node:process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { trace, propagation } from "@opentelemetry/api";
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

// Callers in this process trace their requests, so each request carries a W3C
// trace context the way a traced application's does.
const spans = new InMemorySpanExporter();
trace.setGlobalTracerProvider(
  new BasicTracerProvider({
    spanProcessors: [new SimpleSpanProcessor(spans)],
  }),
);
propagation.setGlobalPropagator(new W3CTraceContextPropagator());

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

/** The one output line that carries `id`, waiting for it to be written. */
async function lineWith(
  runtime: TrellisTestRuntime,
  output: () => string,
  id: string,
): Promise<string> {
  await runtime.waitFor(() => output().includes(id));
  const lines = output()
    .split("\n")
    .filter((line) => line.includes(id));
  assert.equal(lines.length, 1, lines.join("\n"));
  return lines[0]!;
}

test("a TypeScript handler failure is logged once under the reference its caller gets", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "failing-echo",
      contract: participants.Provider.participant,
    });
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
      { env: { ...process.env, NODE_EXTRA_CA_CERTS: runtime.caCertificatePath } },
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
    } finally {
      child.kill("SIGTERM");
      await child.exited;
    }
  });
});

test("a Rust handler failure is logged once under the reference its caller gets", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    let output = "";
    const child = spawnTracked(
      rustFixtureArgv("trellis-runtime-acceptance")[0]!,
      [],
      {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificatePath,
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
    } finally {
      child.kill("SIGTERM");
      await child.exited;
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
