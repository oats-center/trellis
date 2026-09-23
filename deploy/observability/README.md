# Monitoring reference assets

These assets implement the production observability design; they are not a production deployment command.

Use Collector Contrib **0.160.0**. `collector.yaml` is a combined **case-owned validation** configuration. Production native and public browser ingestion use **`collector.native.yaml` and `collector.browser.yaml` in separate processes** so untrusted browser load cannot consume the native Collector's memory budget. Both default to loopback. Set `TRELLIS_CLUSTER`, `TRELLIS_ENVIRONMENT` and `TRELLIS_TRACE_BACKEND`, and create a protected trace spool directory. Remote access requires private discovery and TLS/authenticated ingress, not public cleartext listeners.

The reference Prometheus config targets colocated listeners and auxiliary exporters. Replace `example` in **both** target labels and `expected-components.yml`, choose only components the deployment actually requires, and point private probe/exporter addresses at the deployed hosts. Keep `telemetry_pipeline=native|browser` and the selected metric-name translation. The browser's document-lifetime instance identity prevents incorrect merging of independent cumulative metric streams; aggregate it away in queries rather than deleting it before ingestion. It is not a login/user identity.

Install the standard Node exporter, NATS Prometheus exporter (aggregate mode only), and Blackbox exporter as operator-managed monitoring utilities. `/readyz` in this Trellis source returns version metadata; its probe is HTTP reachability, not proof of authenticated readiness. Component metrics and normal request/delivery signals provide deeper coverage.

Validate with actual binaries: `otelcol-contrib validate`, `promtool check config`, `promtool check rules`, `promtool test rules`, and `amtool check-config`. Supply case-only environment/secret values during validation. Do not send live alerts as part of implementation. The webhook template's secret file is an operator activation input.

RPC 99.9% error-budget and queue/latency thresholds are explicit starting operational policy. They are not machine-speed tests, a release benchmark, or an asserted production SLA. Review warning-only observations and notification routing before turning on paging. No dashboard should show absent/stale data as green zero.

Copy these files into `deploy/observability/` and the runbooks into `docs/observability/runbooks.md`. Raw captured OTLP samples belong to case-owned ignored output, with only sanitized evidence summarized for review.
