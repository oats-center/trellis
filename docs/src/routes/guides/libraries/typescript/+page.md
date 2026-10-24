---
title: "TypeScript libraries"
description: "Use Trellis from TypeScript apps, services, devices, portals, and CLIs with generated SDKs and surface-first APIs."
---

# TypeScript libraries

Application code imports runtime classes from `@oatscenter/trellis` and the
`apis` and `participants` namespaces from its ordinary generated package. With
`[package]` and `[generate.typescript].output` in `trellis.toml`:

```ts
import { TrellisClient } from "@oatscenter/trellis";
import { participants } from "orders-trellis";

const client = await TrellisClient.connect({
  participant: participants.ordersApp.participant,
  trellisUrl: "https://trellis.example.com",
  auth: "browser",
}).orThrow();
```

Do not construct API or participant metadata in TypeScript. Author it in native
IDL and run `trellis generate`. Configure output with
`[generate.typescript].output`. Each language gets one package, not one per API
or participant.

The generated ESM package contains executable `.js`, `.d.ts`, and
`package.json`, at the source package version, with the published runtime
dependency matching the CLI. It needs no consumer transpiler or Trellis CLI.
Install it with ordinary package tooling, for example `npm install ./trellis`.
Generation never edits the consumer's manifest. Commit output when clones
should build without regeneration or the
Trellis source-package cache.

Use `@oatscenter/trellis/service` for service bootstrap in Node 24. Pass
`participant` and the immutable provisioned identity `seed` to
`TrellisService.connect(...)`; `name` is optional. Automatic telemetry can be
disabled with `telemetry: false`, while `runtime` configures service logging,
default request timeout, event stream selection, no-responder retry, and health
heartbeat interval. Use the connected service's generated handlers and `kv`,
`store`, and `jobs` handles. The browser-safe root does not expose native
filesystem helpers.

The SDK's shared protocol verifier is a WebAssembly module, fetched as a
separate asset next to the SDK module when a client connects; bundlers that
resolve `new URL(path, import.meta.url)` assets, such as Vite and webpack 5,
need no configuration. In browsers, synchronous protocol helpers such as
`decodeTransferGrant` need a connected client. Services and devices load a
larger provider build automatically through `@oatscenter/trellis/service` and
`@oatscenter/trellis/device`; import that entrypoint before using any other
Trellis API in the process.

Connected clients expose only the actions selected by their generated
participant. RPCs return `Result`-style values; `.orThrow()` is convenient at an
application boundary, while handlers should model expected failures explicitly.
Do not import provider implementation source to call a service: depend on its
source package, select exact actions in native IDL, and generate your local SDK.

Find exact signatures through [/api](/api). Generate after every IDL change
before checking extracted handler types.

Generated paginated RPCs retain their one-page method and add lazy `.pages(...)`
and `.items(...)` iterators. Missing `nextCursor` is the only end signal;
pagination is keyset-based and does not generally promise a snapshot.

State is one typed value per declared resource. KV is typed key/value history,
and Store remains raw object bytes. Optional resources are `undefined`; an
available but unwritten State resource returns no value. State/KV representation
migrations happen in SDK memory and do not rewrite stored bytes.

App State declarations generate schema registration and explicit requests for
State read, write, and delete capabilities. Retain `const state =
client.state.savedResource`. The first operation registers its native-IDL
projected schemas at expected revision `"0"`. You can instead call
`(await state.register("0")).orThrow()` explicitly to retain the registration
revision. Identical registration returns the current revision. To change a
representation, explicitly pass the current registration revision to
`state.register(revision, idempotencyKey)` and handle `RegistrationConflict`.
Conflicts are not retried; existing representation versions cannot change schema.
Registration revisions and value revisions are separate CAS tokens.

`const observation = (await state.watch()).orThrow()` opens certified Live State
observation. Iterate `for await (const snapshot of observation)` for the initial
snapshot and later updates; `undefined` means unwritten or deleted. Values use
the same in-memory migrations as reads and CAS conflicts. Leaving iteration or
calling `observation.close()` releases the observation; `observation.closed`
resolves to its Live terminal outcome. Watch does not silently reopen after
transport or authorization loss.

Generated service participants expose declared jobs through `service.jobs`.
Keyed capacity waits happen before handler execution and do not consume retry
attempts; retryable handler errors do. See
[Jobs: TypeScript](/guides/using-jobs-ts).

## Native OAuth credentials

Node applications acting as a logged-in user import `discoverNativeOAuth`,
`NativeOAuthClient`, and `connectNativeClient` from
`@oatscenter/trellis/native`. Discover the explicitly selected authorization
origin, open a client with its exact credential `scope` and initial `trust`, then
complete Device Code authorization using the generated native request. Ordinary
provisioned services use their identity seed instead; they need no local user
credential store.

`NativeOAuthClient.open({ scope, trust })` uses a fresh process-local credential
family. It does not open files or require SQLite. To reconnect as the same user
within a process, retain a `MemoryNativeOAuthCustody` object and pass it as
`custody` on each opening for that exact scope. Close the previous connection
before acquiring it again, then call `resume()` rather than prompting for another
login. The connection owns the lease after admission; closing an old session
reference does not release the connection's ownership. Await
`client.connection.close()` before reacquiring: it settles active exchanges and
awaits the application's asynchronous lease release. Repeated closes join the
same disposal result, including a release failure.

Remembering login across process restarts requires an explicitly supplied
application-owned `NativeOAuthCustody` provider. Trellis supplies no persistent
Node backend. Its acquired `NativeOAuthCustodyLease` must exclusively own the
family, keep private signing material private, durably mark a refresh token spent
before submission, atomically retain successors and verified identity, and retain
authenticated issuer trust before spending credentials. The SDK awaits those
commits. An ambiguous refresh is never replayed, and reopening cannot silently
switch accounts or lower retained issuer trust. Rust CLI file custody and browser
IndexedDB custody are unchanged. Existing Node SQLite credential files are left
untouched; applications must choose a provider or authorize a fresh session.

## Automatic transport lifecycle

The connected client is one logical connection, even when its physical NATS
connections change. Renewing unchanged transport authority promotes the new
session authority without replacing a healthy socket. Newly approved authority is adopted automatically: the SDK prepares a
successor, makes its intake ready, and selects it for fresh work. There is no
manual transport-refresh API or upgrade notification to handle.

Work already accepted on a safe connection continues without being restarted by
the handoff. The SDK closes an old connection after its retained work and
subscriptions release it. Revocation, reduced authority, physical failure, or a
hard authorization deadline can instead interrupt affected work; automatic
adoption does not bypass those checks.

Retain the connected client and its typed resource handles rather than
rebuilding them after transport changes. Still-bound KV and Store handles, job
references, and new calls select an eligible connection for each exchange;
removed bindings remain invalid. Jobs and durable event intake keep bounded
receiving capacity, and each delivery retains its receiving connection through
disposition.

A connected socket is not proof of usable application authorization. Fresh work
cannot proceed while own revocation coverage is unavailable. Bounded
acquisitions wait within their budget; immediate acquisitions report
unavailability. Verified coverage recovery wakes waiting calls without requiring
socket churn. Successful make-before-break watch migration preserves cached
authority and existing authorization leases. Actual revocation still fails
closed.
