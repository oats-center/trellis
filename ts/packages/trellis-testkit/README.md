# @oatscenter/trellis-testkit

Deno helpers that own a real Trellis/NATS runtime for service-repository tests.
Use ordinary `deno test -A`; there is no runner, matrix, or case registration.

## Generated Participants

List the fixture source and dependencies in `trellis.toml`, author participants
in `contract.trellis`, run `trellis install`, and import their generated
participant exports. Never construct test contracts, subjects, or authorization
evidence by hand.

```ts
import { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "test-trellis";

await using runtime = await TrellisTestRuntime.start();
const identity = await runtime.registerService({
  name: "provider",
  contract: participants.provider.participant,
});
const service = await TrellisService.connect({
  trellisUrl: runtime.trellisUrl,
  participant: participants.provider.participant,
  name: "provider",
  seed: identity.seed,
}).orThrow();
const client = await runtime.connectClient({
  name: "caller",
  contract: participants.caller.participant,
});
```

Register generated handlers before starting the service. Exercise generated
caller methods and inspect their `Result` values with ordinary assertions. Stop
the service in `finally`, await its run task, and let `await using` stop the
test runtime. Ephemeral authorization storage is appropriate only for isolated
tests; production clients need persistent trust state.

For events, subscribe through a generated surface before publishing, then await
the actual typed payload. For operations and jobs, await their public terminal
result. No helper should replace a real boundary with a fabricated result.

## Ownership and Waiting

The released package acquires the exact-version production CLI and server from
release archives pinned by SHA-256. It supports Linux and macOS x86-64/arm64.
Production `trellis init config` generates configuration and accounts;
`trellis-server --local-nats` owns the real broker. There is no `nsc` dependency
or TypeScript configuration renderer. `TRELLIS_TEST_CACHE_DIR` shares only
verified native binaries; each runtime has private configuration, credentials,
SQLite data, broker state, process files, and logs.

`start()` seeds an administrator by default. Use `firstAdmin: "browser-flow"`
only when exercising first-admin browser setup. To use producer-built binaries
without downloading, pass `trellis: { source: { kind: "path", cli, server } }`;
both files must match the package version. There is no fallback for invalid
paths.

Startup accepts `/readyz` only when its `processId` identifies the spawned
native host, so another runtime at the selected port cannot satisfy readiness.

`restart()` restarts the entire production host and its managed broker while
retaining data and identity. `stop()` waits for child cleanup before removing
the sandbox. The host receives SIGTERM first; if it exceeds the shutdown bound,
the testkit force-closes its isolated Unix process group, including the managed
broker. Descendants are also reclaimed if the host crashes. `await using` shares
that cleanup path. The package does not attach tests to a shared NATS URL or
assign infrastructure by test name.

Real broker adapter tests can use `connectNats()` for a privileged isolated
connection. It does not own the broker and closes on stop or restart; acquire a
new connection after restart. Ordinary service tests use generated clients and
`registerService`/`connectClient` instead.

With `interruptibleNativeProxy`, `nativeProxyUrl()` returns the advertised
endpoint for raw clients exercising `nativeTransportGate()`. Runtime internals
and `connectNats()` stay on the direct broker endpoint. The advertised override
is a server launch option, not a change to the generated config file.

`runtime.waitFor` provides bounded polling for public transitions. Prefer an
observable completion signal when available. `tempSqlitePath` and
`sqliteMemoryUrl` support deterministic real SQLite tests without a control
plane.

## Repository Live Suite

See [integration setup](../../../integration/README.md) for generation, binary
builds, and `TRELLIS_TEST_SERVER_BIN` / `TRELLIS_TEST_CLI_BIN` environment
variables. Run all or a native filter:

```sh
deno test -A -c ts/integration/deno.json ts/integration
deno test -A -c ts/integration/deno.json --filter 'generated runtime workflows' ts/integration
```
