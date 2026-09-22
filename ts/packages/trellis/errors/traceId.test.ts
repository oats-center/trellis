/**
 * Tests for traceId inclusion in Trellis error serialization.
 *
 * When a Trellis RPC returns an error, the serialized error should include
 * `traceId` so clients can report it for debugging.
 */

import {
  InMemorySpanExporter,
  SimpleSpanProcessor,
} from "@opentelemetry/sdk-trace-base";
import { NodeTracerProvider } from "@opentelemetry/sdk-trace-node";
import { UnexpectedError } from "@qlever-llc/result";
import { assertEquals, assertExists } from "@std/assert";
import {
  configureErrorTraceId,
  getActiveSpan,
  getTracer,
  withSpan,
} from "../telemetry.ts";
import { AuthError } from "./AuthError.ts";
import { KVError } from "./KVError.ts";
import { RemoteError } from "./RemoteError.ts";
import { ValidationError } from "./ValidationError.ts";

// Set up a real tracer provider for tests (required for spans to have valid trace IDs)
const testExporter = new InMemorySpanExporter();
const testProvider = new NodeTracerProvider({
  spanProcessors: [new SimpleSpanProcessor(testExporter)],
});
testProvider.register();

// Configure error traceId getter before running tests
configureErrorTraceId();

Deno.test({
  name: "traceId in error serialization",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn(t) {
    await t.step("AuthError includes traceId when span is active", () => {
      const tracer = getTracer();
      const span = tracer.startSpan("test-span");

      withSpan(span, () => {
        const error = new AuthError({ reason: "invalid_request" });
        const serialized = error.toSerializable();

        assertExists(
          serialized.traceId,
          "traceId should be present when span is active",
        );
        assertEquals(typeof serialized.traceId, "string");
        // TraceId should be a 32-character hex string
        assertEquals(serialized.traceId?.length, 32);
      });

      span.end();
    });

    await t.step("ValidationError includes traceId when span is active", () => {
      const tracer = getTracer();
      const span = tracer.startSpan("test-span");

      withSpan(span, () => {
        const error = new ValidationError({
          errors: [{ path: "/field", message: "required" }],
        });
        const serialized = error.toSerializable();

        assertExists(
          serialized.traceId,
          "traceId should be present when span is active",
        );
        assertEquals(typeof serialized.traceId, "string");
      });

      span.end();
    });

    await t.step("KVError includes traceId when span is active", () => {
      const tracer = getTracer();
      const span = tracer.startSpan("test-span");

      withSpan(span, () => {
        const error = new KVError({ operation: "get" });
        const serialized = error.toSerializable();

        assertExists(
          serialized.traceId,
          "traceId should be present when span is active",
        );
        assertEquals(typeof serialized.traceId, "string");
      });

      span.end();
    });

    await t.step("UnexpectedError includes traceId when span is active", () => {
      const tracer = getTracer();
      const span = tracer.startSpan("test-span");

      withSpan(span, () => {
        const error = new UnexpectedError();
        const serialized = error.toSerializable();

        assertExists(
          serialized.traceId,
          "traceId should be present when span is active",
        );
        assertEquals(typeof serialized.traceId, "string");
      });

      span.end();
    });

    await t.step("RemoteError includes traceId when span is active", () => {
      const tracer = getTracer();
      const span = tracer.startSpan("test-span");

      withSpan(span, () => {
        const error = new RemoteError({
          error: {
            id: "remote-123",
            type: "AuthError",
            message: "Auth failed: forbidden",
            reason: "forbidden",
          },
        });
        const serialized = error.toSerializable();

        assertExists(
          serialized.traceId,
          "traceId should be present when span is active",
        );
        assertEquals(typeof serialized.traceId, "string");
      });

      span.end();
    });

    await t.step("AuthError omits traceId when no span is active", () => {
      // Ensure no span is active by running outside any span context
      const activeSpan = getActiveSpan();
      assertEquals(
        activeSpan,
        undefined,
        "No span should be active for this test",
      );

      const error = new AuthError({ reason: "session_expired" });
      const serialized = error.toSerializable();

      assertEquals(
        serialized.traceId,
        undefined,
        "traceId should be undefined when no span active",
      );
    });

    await t.step("AuthError preserves explicit human messages", () => {
      const error = new AuthError({
        reason: "username_taken",
        message: "That username is already in use.",
        context: { username: "ada" },
      });
      const serialized = error.toSerializable();

      assertEquals(serialized.message, "That username is already in use.");
      assertEquals(serialized.reason, "username_taken");
      assertEquals(serialized.context?.username, "ada");
    });

    await t.step("ValidationError omits traceId when no span is active", () => {
      const error = new ValidationError({
        errors: [{ path: "/name", message: "too short" }],
      });
      const serialized = error.toSerializable();

      assertEquals(
        serialized.traceId,
        undefined,
        "traceId should be undefined when no span active",
      );
    });

    await t.step("traceId is included in JSON serialization", () => {
      const tracer = getTracer();
      const span = tracer.startSpan("test-span");

      withSpan(span, () => {
        const error = new AuthError({ reason: "forbidden" });
        const json = error.toJSON();
        const parsed = JSON.parse(json);

        assertExists(parsed.traceId, "traceId should be in JSON output");
        assertEquals(typeof parsed.traceId, "string");
      });

      span.end();
    });

    await t.step("existing serialization fields are preserved", () => {
      const tracer = getTracer();
      const span = tracer.startSpan("test-span");

      withSpan(span, () => {
        const error = new AuthError({
          reason: "insufficient_permissions",
          context: { resource: "documents" },
          id: "test-error-id",
        });
        const serialized = error.toSerializable();

        // Verify all existing fields still work
        assertEquals(serialized.id, "test-error-id");
        assertEquals(serialized.type, "AuthError");
        assertEquals(
          serialized.message,
          "Auth failed: insufficient_permissions",
        );
        assertEquals(serialized.reason, "insufficient_permissions");
        assertEquals(serialized.context?.resource, "documents");
        // And traceId is added
        assertExists(serialized.traceId);
      });

      span.end();
    });

    await testProvider.shutdown();
  },
});
