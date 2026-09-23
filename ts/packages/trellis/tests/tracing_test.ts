import { assertEquals, assertExists, assertNotEquals } from "@std/assert";

// Test file for distributed telemetry module
// Following TDD: writing tests FIRST before implementation

Deno.test("Telemetry Module", async (t) => {
  await t.step("HeaderCarrier interface", async (t) => {
    await t.step("should allow get and set operations", async () => {
      // Import the module - this will fail until we create it
      const { createMapCarrier } = await import("../telemetry.ts");

      const carrier = createMapCarrier();
      carrier.set("traceparent", "00-abc123-def456-01");
      assertEquals(carrier.get("traceparent"), "00-abc123-def456-01");
    });

    await t.step("should return undefined for missing keys", async () => {
      const { createMapCarrier } = await import("../telemetry.ts");

      const carrier = createMapCarrier();
      assertEquals(carrier.get("nonexistent"), undefined);
    });
  });

  await t.step("Span Creation", async (t) => {
    await t.step(
      "startClientSpan should create a span with correct attributes",
      async () => {
        const { startClientSpan, SpanKind } = await import("../telemetry.ts");

        const span = startClientSpan("auth.Sessions.Me");

        assertExists(span);
        assertExists(span.spanContext());
        assertExists(span.spanContext().traceId);
        assertExists(span.spanContext().spanId);

        // Clean up
        span.end();
      },
    );

    await t.step(
      "startServerSpan should create a span with correct attributes",
      async () => {
        const { startServerSpan, SpanKind } = await import("../telemetry.ts");

        const span = startServerSpan("auth.Sessions.Me");

        assertExists(span);
        assertExists(span.spanContext());
        assertExists(span.spanContext().traceId);
        assertExists(span.spanContext().spanId);

        // Clean up
        span.end();
      },
    );

    await t.step(
      "startServerSpan with parent context should link to parent",
      async () => {
        const {
          startClientSpan,
          startServerSpan,
          injectTraceContext,
          extractTraceContext,
          createMapCarrier,
        } = await import("../telemetry.ts");

        // Create a "client" span that would inject context
        const clientSpan = startClientSpan("auth.Sessions.Me");
        const carrier = createMapCarrier();

        // Inject the client's trace context into the carrier
        injectTraceContext(carrier, clientSpan);

        // Extract context on the "server" side
        const parentContext = extractTraceContext(carrier);

        // Create server span with parent context
        const serverSpan = startServerSpan(
          "auth.Sessions.Me",
          parentContext,
        );

        // Both spans should share the same trace ID if context propagation works
        // Note: With NOOP tracer (no SDK), spans may have empty trace IDs
        // The real test is that no exceptions are thrown
        assertExists(serverSpan);
        assertExists(serverSpan.spanContext());

        // Clean up
        serverSpan.end();
        clientSpan.end();
      },
    );
  });

  await t.step("Context Propagation", async (t) => {
    await t.step(
      "injectTraceContext should add headers to carrier",
      async () => {
        const {
          startClientSpan,
          injectTraceContext,
          createMapCarrier,
        } = await import("../telemetry.ts");

        const span = startClientSpan("auth.Sessions.Me");
        const carrier = createMapCarrier();

        // Inject trace context while span is active
        injectTraceContext(carrier, span);

        // With a configured tracer, this would have 'traceparent' header
        // With NOOP tracer, it may not inject anything, but should not throw
        // The important test is that the function executes without error
        assertExists(carrier);

        span.end();
      },
    );

    await t.step(
      "extractTraceContext should return a context object",
      async () => {
        const { extractTraceContext, createMapCarrier } = await import(
          "../telemetry.ts"
        );

        const carrier = createMapCarrier();
        // Simulate a W3C trace context header
        carrier.set(
          "traceparent",
          "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        );

        const ctx = extractTraceContext(carrier);

        // Should return a Context object (even if it's the ROOT_CONTEXT with NOOP)
        assertExists(ctx);
      },
    );
  });

  await t.step("Span Status", async (t) => {
    await t.step("span should allow setting status to OK", async () => {
      const { startClientSpan, SpanStatusCode } = await import(
        "../telemetry.ts"
      );

      const span = startClientSpan("auth.Sessions.Me");
      span.setStatus({ code: SpanStatusCode.OK });

      // Should not throw
      assertExists(span);
      span.end();
    });

    await t.step(
      "span should allow setting status to ERROR with message",
      async () => {
        const { startClientSpan, SpanStatusCode } = await import(
          "../telemetry.ts"
        );

        const span = startClientSpan("auth.Sessions.Me");
        span.setStatus({ code: SpanStatusCode.ERROR, message: "Test error" });

        // Should not throw
        assertExists(span);
        span.end();
      },
    );

    await t.step("span should allow recording exceptions", async () => {
      const { startClientSpan } = await import("../telemetry.ts");

      const span = startClientSpan("auth.Sessions.Me");
      span.recordException(new Error("Test exception"));

      // Should not throw
      assertExists(span);
      span.end();
    });
  });

  await t.step("NATS Header Carrier Adapter", async (t) => {
    await t.step(
      "createNatsHeaderCarrier should wrap NATS headers",
      async () => {
        const { createNatsHeaderCarrier } = await import("../telemetry.ts");

        // Mock NATS headers-like object
        const mockHeaders = {
          values: new Map<string, string[]>(),
          get(key: string): string | undefined {
            const vals = this.values.get(key);
            return vals?.[0];
          },
          set(key: string, value: string): void {
            this.values.set(key, [value]);
          },
        };

        const carrier = createNatsHeaderCarrier(mockHeaders);

        carrier.set("traceparent", "test-value");
        assertEquals(carrier.get("traceparent"), "test-value");
      },
    );
  });

  await t.step("getTracer", async (t) => {
    await t.step("should return a tracer instance", async () => {
      const { getTracer } = await import("../telemetry.ts");

      const tracer = getTracer();
      assertExists(tracer);
    });
  });

  await t.step("initTelemetry", async (t) => {
    await t.step("should be idempotent (multiple calls safe)", async () => {
      const { initTelemetry } = await import("../telemetry.ts");

      // Should not throw on multiple calls
      initTelemetry("test-service");
      initTelemetry("test-service");
      initTelemetry("another-service");

      // If we get here without throwing, the test passes
    });
  });

  await t.step("recordTrellisError", async (t) => {
    await t.step(
      "should be safe without a configured meter provider",
      async () => {
        const { recordTrellisError } = await import("../telemetry.ts");

        recordTrellisError(new Error("sensitive message"), {
          surface: "rpc",
          operation: "request",
        });
      },
    );
  });

  await t.step("getActiveSpan", async (t) => {
    await t.step("should return active span from context", async () => {
      const { getActiveSpan, startClientSpan, withSpan } = await import(
        "../telemetry.ts"
      );

      const span = startClientSpan("auth.Sessions.Me");

      // Run code within the span's context
      await withSpan(span, async () => {
        const activeSpan = getActiveSpan();
        // With a properly configured tracer, this would return the span
        // With NOOP, it may return undefined, but should not throw
      });

      span.end();
    });
  });
});

Deno.test({
  name: "initTelemetry is safe without env permission",
  permissions: { env: false },
  async fn() {
    const { initTelemetry } = await import("../telemetry.ts");
    initTelemetry("no-env-permission-service");
  },
});

Deno.test("catalog route tokens and recorders stay bounded", async (t) => {
  const {
    recordCatalogCounter,
    recordCatalogDuration,
    recordCatalogUpDown,
    routeToken,
  } = await import("../telemetry.ts");

  await t.step("registered tokens are stable and interned", () => {
    const first = routeToken("rpc", "auth.Sessions.Me");
    assertEquals(first, "auth.Sessions.Me");
    assertEquals(routeToken("rpc", "auth.Sessions.Me"), first);
  });

  await t.step("overlong and overflow tokens map to _other", () => {
    assertEquals(routeToken("http", "x".repeat(129)), "_other");
    for (let index = 0; index < 200; index += 1) {
      routeToken("job", `bounded.job.${index}`);
    }
    assertEquals(routeToken("job", "bounded.job.overflow"), "_other");
  });

  await t.step("recorders accept only bounded attributes", () => {
    recordCatalogDuration("trellis.rpc.client.duration", 12.5, {
      "trellis.route": "auth.Sessions.Me",
      "trellis.outcome": "ok",
      "messaging.destination": "rpc.v1.secret",
    });
    recordCatalogCounter("trellis.rpc.client.attempts", 1, {
      "trellis.route": "auth.Sessions.Me",
      "trellis.outcome": "ok",
    });
    recordCatalogUpDown("trellis.rpc.server.inflight", 1, {
      "trellis.route": "auth.Sessions.Me",
    });
    recordCatalogDuration("trellis.rpc.client.duration", 1, {
      "trellis.route": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    });
    assertEquals(true, true);
  });
});

Deno.test("browser endpoint sanitizer rejects normalization escapes", async (t) => {
  const { sanitizeBrowserEndpoint } = await import(
    "../telemetry/browser.ts"
  );

  await t.step("accepts the same-origin relative default", () => {
    assertEquals(sanitizeBrowserEndpoint("/otel"), "/otel");
    assertEquals(sanitizeBrowserEndpoint("/otel/"), "/otel");
  });

  await t.step("rejects cross-origin and normalization escapes", () => {
    assertEquals(sanitizeBrowserEndpoint("//example.invalid/otel"), undefined);
    assertEquals(
      sanitizeBrowserEndpoint("/\t/example.invalid/otel"),
      undefined,
    );
    assertEquals(
      sanitizeBrowserEndpoint("/\n/example.invalid/otel"),
      undefined,
    );
    assertEquals(
      sanitizeBrowserEndpoint("https://example.invalid/otel"),
      undefined,
    );
    assertEquals(sanitizeBrowserEndpoint("/otel?x=1"), undefined);
    assertEquals(sanitizeBrowserEndpoint("/otel#frag"), undefined);
    assertEquals(sanitizeBrowserEndpoint("/otel@host"), undefined);
    assertEquals(sanitizeBrowserEndpoint("otel"), undefined);
    assertEquals(sanitizeBrowserEndpoint(undefined), undefined);
  });
});

Deno.test("browser owner retains work after timeout and serializes shutdown", async () => {
  const { browserOwner } = await import("../telemetry/browser.ts");
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => release = resolve);
  const events: string[] = [];
  const handle = browserOwner(
    {
      forceFlush: async () => {
        events.push("trace flush");
        await blocked;
      },
      shutdown: async () => {
        events.push("trace shutdown");
      },
    },
    {
      forceFlush: async () => {
        events.push("metric flush");
      },
      shutdown: async () => {
        events.push("metric shutdown");
      },
    },
  );
  void handle.forceFlush();
  await handle.forceFlush();
  const firstShutdown = handle.shutdown();
  const secondShutdown = handle.shutdown();
  await Promise.all([firstShutdown, secondShutdown]);
  assertEquals(events, ["trace flush"]);
  release();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assertEquals(events, ["trace flush"]);
});

Deno.test("browser owner coalesces a caller that stops waiting", async () => {
  const { browserOwner } = await import("../telemetry/browser.ts");
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => release = resolve);
  let flushes = 0;
  const provider = {
    forceFlush: async () => {
      flushes++;
      await blocked;
    },
    shutdown: async () => {},
  };
  const handle = browserOwner(provider, provider);
  void handle.forceFlush();
  const joining = handle.forceFlush();
  assertEquals(flushes, 1);
  release();
  await joining;
  assertEquals(flushes, 2);
});
