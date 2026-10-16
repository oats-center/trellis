import assert from "node:assert/strict";
import { after, test } from "node:test";
import { NodeTracerProvider } from "@opentelemetry/sdk-trace-node";
import {
  configureErrorTraceId,
  getActiveSpan,
  getTracer,
  withSpan,
} from "../telemetry.ts";
import { AuthError } from "./AuthError.ts";
import { RemoteError } from "./RemoteError.ts";

const provider = new NodeTracerProvider();
provider.register();
configureErrorTraceId();
after(() => provider.shutdown());

test("error JSON retains the originating trace across an asynchronous continuation", async () => {
  const span = getTracer().startSpan("originating-request");
  try {
    await withSpan(span, async () => {
      await Promise.resolve();
      const decoded = RemoteError.parseJSON(
        new AuthError({ reason: "forbidden" }).toJSON(),
      ).orThrow();
      assert.equal(decoded.traceId, span.spanContext().traceId);
    });
  } finally {
    span.end();
  }
});

test("remote error JSON preserves remote and local trace correlation independently", () => {
  const remoteSpan = getTracer().startSpan("remote-request");
  const localSpan = getTracer().startSpan("local-request");
  try {
    const remote = withSpan(remoteSpan, () =>
      RemoteError.parseJSON(
        new AuthError({ reason: "forbidden" }).toJSON(),
      ).orThrow(),
    );
    const decoded = withSpan(localSpan, () =>
      RemoteError.parseJSON(
        new RemoteError({ error: remote }).toJSON(),
      ).orThrow(),
    );
    assert.equal(decoded.traceId, localSpan.spanContext().traceId);
    const nested: unknown = Reflect.get(decoded, "remoteError");
    assert(nested !== null && typeof nested === "object");
    assert.equal(
      Reflect.get(nested, "traceId"),
      remoteSpan.spanContext().traceId,
    );
  } finally {
    remoteSpan.end();
    localSpan.end();
  }
});

test("error JSON outside a span does not inherit a completed request's trace", () => {
  assert.equal(getActiveSpan(), undefined);
  const decoded = RemoteError.parseJSON(
    new AuthError({ reason: "forbidden" }).toJSON(),
  ).orThrow();
  assert.equal(decoded.traceId, undefined);
});
