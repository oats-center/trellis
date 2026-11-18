# Monitoring reference assets

These files are reference configuration for monitoring a Trellis deployment.
Copy and adapt them; nothing here deploys itself.
[docs/observability](../../docs/observability/README.md) holds the metric
catalog and the runbooks the alerts link to.

## Collectors

`collector.native.yaml` receives native OTLP over HTTP on `127.0.0.1:4318` and
exposes Prometheus metrics on `127.0.0.1:9464`. `collector.browser.yaml`
receives browser OTLP on `127.0.0.1:4319` and exposes it on `127.0.0.1:9465`.
Run them as separate processes so browser traffic, which is untrusted, cannot
use up the native Collector's memory. `collector.yaml` combines both pipelines
in one process for validation.

Each Collector reads `TRELLIS_CLUSTER`, `TRELLIS_ENVIRONMENT` and
`TRELLIS_TRACE_BACKEND` from its environment and spools traces under
`/var/lib/otelcol/`. Every listener binds to loopback. To reach one from
another machine, put TLS and authentication in front of it instead of opening
a cleartext listener. `browser-proxy.nginx.conf` is a same-origin relay for
browser telemetry to place in the Console's HTTPS virtual host.

Both Collectors' Prometheus exporters use
`translation_strategy: UnderscoreEscapingWithoutSuffixes`, so names carry no
unit or type suffix (`trellis_auth_flow_duration_bucket`); the recording and
alert rules expect those names. Browser metrics keep `service.instance.id`, a random ID per page load, so separate
pages' cumulative counters don't merge. Aggregate it away in queries rather
than dropping it before ingestion. It does not identify a user.

## Prometheus

`prometheus.yml` scrapes the two Collector exporters, the Collectors' own
metrics (`8888`, `8889`), the Node exporter (`9100`) and the NATS Prometheus
exporter (`7777`), and probes two endpoints through the Blackbox exporter
(`9115`, module `http_2xx` from `blackbox.yml`):

- `http://127.0.0.1:3000/readyz` on Trellis. It returns `503` until startup
  has finished and `200` after, and `503` again while the server stops or
  while another server runs Auth. A failed probe means this server isn't
  serving, not only that it is unreachable. A `200` does not show that
  authorization, jobs or delivery work; component metrics and request signals
  cover those.
- `http://127.0.0.1:8222/healthz` on the NATS monitoring port.

Install the Node, NATS and Blackbox exporters yourself. Replace `example` in
the `trellis_cluster` labels of both `prometheus.yml` and
`expected-components.yml`, keep only the components the deployment runs, and
point the targets at the real hosts. Keep the `telemetry_pipeline` labels.

`recording-rules.yml`, `expected-components.yml` and `alert-rules.yml` are the
rule files, `rule-tests.yml` tests them, `alertmanager.example.yml` routes
alerts to a webhook whose URL it reads from
`/run/secrets/trellis-alert-webhook-url`, and `trellis-dashboard.json` is a
Grafana dashboard.

## Validate

Check the files with the real tools before using them, with your own
environment values and secrets:

```sh
otelcol-contrib validate --config collector.native.yaml
promtool check config prometheus.yml
promtool check rules recording-rules.yml expected-components.yml alert-rules.yml
promtool test rules rule-tests.yml
amtool check-config alertmanager.example.yml
```

The RPC 99.9% error budget and the queue and latency thresholds in the alert
rules are starting policy, not a measured SLA. Review them and the
notification routing before turning on paging.
