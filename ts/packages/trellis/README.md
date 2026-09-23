# @oats-center/trellis

JavaScript Trellis client runtime. Provides generated-participant client helpers
and runtime error types.

The npm package contains ESM JavaScript and TypeScript declarations. Use
`import`; CommonJS output and `require()` support have intentionally been
removed. Node and Deno adapters remain available as ESM subpaths. JSR
publication remains supported.

For AI-agent context, start with the generated package `TRELLIS.md` files and
the raw docs index:

- https://raw.githubusercontent.com/oats-center/trellis/main/docs/static/llms.txt
- https://raw.githubusercontent.com/oats-center/trellis/main/docs/static/llms-full.txt

```typescript
import { TrellisClient } from "@oats-center/trellis";
import { participants } from "example-trellis";

const client = await TrellisClient.connect({
  trellisUrl: "https://trellis.example.com",
  participant: participants.exampleApp.participant,
}).orThrow();
```

The local package name comes from `trellis.toml`. Generation emits one ordinary
ESM package with `apis` and `participants` namespaces, executable JavaScript and
declarations, and the matching published runtime dependency. Commit that package
when ordinary builds should not require the Trellis CLI or API cache.

Connected API facades group actions by surface: `.rpc.<group>.<leaf>(input)`,
`.event.<group>.<leaf>.listen(handler)` or `.publish(event)`,
`.feed.<group>.<leaf>(input)`, and `.operation.<group>.<leaf>.start(input)`.
Inspect generated declarations for the exact selected APIs and action names. Do
not reconstruct transport subjects or use handwritten contract metadata.

Prepared events support durable publish flows. `prepare(...)` returns a
`PreparedTrellisEvent`; services can persist prepared events in SQL or NATS KV
outbox repositories and later publish them with `client.publishPrepared(...)`,
dispatch them with `dispatchOutbox`, or run an `OutboxDispatcher` and call
`notify()` after an outbox transaction commits.

Durable service event consumption is contract-declared. Add an event consumer
group to the service contract and call the generated listener with
`{ group: "groupName" }`. Do not pass `durableName`; Trellis provisions the
physical JetStream consumer and grants only the bound consumer subjects to the
service token. Use `{ mode: "ephemeral", replay: "new" }` for live-only
listeners.

Service connection helpers live in `@oats-center/trellis/service*` to keep the
root package browser-safe. Browser login and portal-flow helpers live on
`@oats-center/trellis/auth` and `@oats-center/trellis/auth/browser`.

Service authors should not use the core package to recreate service bootstrap or
fetch resource bindings. Connect with `TrellisService.connect(...)` from
`@oats-center/trellis/service` and use the returned resource handles instead of
calling `Trellis.Bindings.Get`, constructing `TrellisService` or `StoreHandle`,
or passing binding/resource data into `Trellis` constructors.
