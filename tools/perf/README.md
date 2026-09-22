# Manual connect performance audit

`connect.ts` is a developer-facing, manual same-machine audit of console
connect latency. It is never invoked by `cargo test`, `deno test`,
`trellis check`, GitHub Actions, or release validation, and it makes no
pass/fail decision from timing; it only reports p50/p95 and raw samples.

- Compare a baseline and a candidate only on the same machine and under the
  same conditions.
- Use release builds of Trellis when comparing performance.
- Keep the server configuration, browser version and profile, asset-cache
  mode, and background load comparable between sides.
- Store outputs under `results/` (gitignored).

```
TRELLIS_URL=http://127.0.0.1:3000 \
TRELLIS_PROFILE_DIR=/path/to/already-signed-in/profile \
  deno run -A -c ts/deno.json tools/perf/connect.ts \
  --warmup 3 \
  --runs 20 \
  --asset-cache enabled \
  --output tools/perf/results/connect-current.json
```

Flags:

- `--warmup <non-negative integer>`: warmup navigations, excluded from
  percentiles. Default `3`.
- `--runs <positive integer>`: measured navigations. Default `20`.
- `--asset-cache enabled|disabled`: browser HTTP cache mode. Default
  `enabled`.
- `--output <path>`: required report path.

The process exits non-zero only when a measured navigation is functionally
invalid (a required refresh did not succeed, readiness was not reached, or a
page/network/HTTP error was observed). Latency never changes the exit code.
