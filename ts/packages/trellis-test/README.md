# @oats-center/trellis-test

Deno helpers that own a real Trellis/NATS runtime for service-repository tests.
Use ordinary `deno test -A`; there is no runner, matrix, or case registration.

## Generated Participants

List the fixture source and dependencies in `trellis.toml`, author participants
in `contract.trellis`, run `trellis install`, and import their generated
participant exports. Never construct test contracts, subjects, or authorization
evidence by hand.

```ts
import { TrellisTestRuntime } from "@oats-center/trellis-test";
import { TrellisService } from "@oats-center/trellis/service";
import { participants } from "test-trellis";

await using runtime = await TrellisTestRuntime.start({
  trellis: {
    command: {
      cmd: "/path/to/trellis-server",
      args: ["--config", "{config}", "all"],
    },
  },
});
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

Each runtime owns fresh accounts, a real NATS process, SQLite files, and a real
control-plane process. `trellis.command` is explicit. The package does not
attach tests to a shared NATS URL or assign infrastructure by test name.

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
