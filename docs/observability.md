# Observability

The Trellis server always writes its normal tracing output to stdout, or to
stderr for `trellis-server check`. Attached terminals keep the human-readable
ANSI format with targets shown only when verbose; detached output stays
JSON-formatted without ANSI. `RUST_LOG` and the `--verbose`/`--dev` flags keep
their existing behavior; the OpenTelemetry export described here is layered on
top of that output and never replaces it.

## OpenTelemetry export

Trace and metric export over OTLP is opt-in and disabled unless standard
endpoint environment variables are explicitly configured with non-empty
values. When no endpoint variable is set, the server does not create an OTLP
exporter and does not attempt to connect anywhere.

| Environment | Traces | Metrics |
| --- | --- | --- |
| No endpoint variable | off | off |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | on | on |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` only | on | off |
| `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` only | off | on |
| `trellis-server check ...` | off | off |

The supported transport is OTLP over HTTP/protobuf. The exporter honors the
standard OpenTelemetry environment variables, including:

- `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`,
  `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`
- `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_TRACES_HEADERS`,
  `OTEL_EXPORTER_OTLP_METRICS_HEADERS`
- `OTEL_EXPORTER_OTLP_TIMEOUT`, `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT`,
  `OTEL_EXPORTER_OTLP_METRICS_TIMEOUT`
- `OTEL_METRIC_EXPORT_INTERVAL`

If an explicitly requested exporter cannot be built, the server prints one
warning to stderr, disables only that signal, and continues starting up.

Trace/log correlation and inbound or outbound trace-context propagation across
HTTP, NATS, RPC, Events, Jobs, or generated SDK protocols are not implemented
by this release; only server-side spans and metrics are exported.

## Metrics

All duration histograms use seconds with explicit sub-second boundaries
(1 ms to 10 s), and their sum and count preserve the exact average. Trellis
does not emit a separate average-time gauge; dashboards derive averages and
percentiles from the histogram.

| Metric | Kind | Dimensions |
| --- | --- | --- |
| `trellis.auth.flow.duration` | Histogram, s | `trellis.surface=auth`, `trellis.operation`, `trellis.phase`, `trellis.outcome` |
| `trellis.auth.callout.duration` | Histogram, s | `trellis.surface=auth`, `trellis.operation`, `trellis.phase`, `trellis.outcome` |
| `trellis.contract.analysis.duration` | Histogram, s | `trellis.surface=contract`, `trellis.operation`, `trellis.phase`, `trellis.outcome` |
| `trellis.contract.cache.requests` | Counter, `{request}` | `trellis.cache.kind=evidence\|compatibility`, `trellis.cache.result=hit\|miss\|wait` |

Operations and phases are fixed low-cardinality values: authorization context
refresh, bootstrap issuance, current and resolved API bindings, NATS auth
callout request and authorization, compiled-evidence analysis, and
selected-surface comparison. Identifiers such as principal IDs, participant
IDs, deployment IDs, connection IDs, request IDs, context or evidence digests,
subjects, and URLs are never metric attributes or span fields added by this
instrumentation.

## Manual performance audit

`tools/perf/connect.ts` is a manual same-machine developer audit of console
connect latency. It is not part of CI, the `Check` workflow, or release
validation, and it never fails a build because of timing; performance timing is
not a CI gate. See `tools/perf/README.md`.
