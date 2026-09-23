# Trellis observability

This directory documents the production observability contract and the
operator-side reference assets.

- `metric-catalog.json` — the normative metric catalog: instrument names, units,
  dimensions, and observation ownership.
- `runbooks.md` — the runbook for every reference alert. Alert annotations link
  to its headings.
- `disposition.md` — every catalog instrument's emitting owner and its evidence
  category.
- `evidence.md` — the exact commands, pinned exporter versions, captured
  families, and trace parent chains behind the collected evidence.
- `../deploy/observability/` — the reference Collector, Prometheus,
  Alertmanager, Blackbox, browser-relay, recording/alert rule, expected
  component, and dashboard assets.

Telemetry is diagnostic only. It never authorizes, gates readiness, or changes a
business result, and an unavailable Collector never affects Trellis work. Native
and built-in browser telemetry use separate Collector pipelines and separate
Prometheus jobs; browser reports are untrusted and never page as proof that a
server is healthy.

Validate the supplied assets with the actual pinned binaries
(`otelcol-contrib validate`, `promtool check config|rules|test rules`,
`amtool check-config`) using case-owned environment and secret values. See
`../deploy/observability/README.md` for the reference topology and activation
sequence.
