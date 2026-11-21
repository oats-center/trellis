---
title: "Author contracts with Trellis IDL"
description: "Declare APIs and participants in .trellis files, lock dependencies, and generate TypeScript or Rust packages."
---

# Author contracts with Trellis IDL

Trellis contracts are `.trellis` source files. They define the messages an API
exchanges, the actions it exposes, and which services or apps provide or use
those actions. The CLI compiles the same source into Rust and TypeScript
packages; you do not write separate wire schemas for each language.

## Create a package

Put `trellis.toml` alongside the source file:

```toml
[package]
name = "acme-orders"
workspace = "ghcr.io/acme"
version = "1.0.0"

[sources]
orders = "contract.trellis"

[generate.typescript]
output = "trellis"
```

`workspace` is where the package is published, and with the name it makes the
package's identity, `ghcr.io/acme/acme-orders`, which starts every ID it
defines: the service below is `ghcr.io/acme/acme-orders.OrdersService`. Leave
it out for a package you will never publish; it then gets `localhost`. See
[Workspaces](/design/contracts/workspaces) for how IDs are formed. Choose
`[generate.rust]` with an `output` path instead for a Rust service, or
configure both targets. Source aliases such as `orders` are local names used by
imports in other `.trellis` files; list every source explicitly.

Then author `contract.trellis`:

```trellis
model CreateOrderRequest { customerId: string; }
model CreateOrderResponse { orderId: string; customerId: string; }

api orders@v1 {
  title "Orders";
  description "Create an order.";
  rpc Create { input CreateOrderRequest; output CreateOrderResponse; }
  capabilities {
    capability orders.create {
      title "Create orders";
      description "Create an order for a customer.";
      grants "The caller may create customer orders.";
      allows { rpc Create; }
    }
  }
}

service OrdersService { implements orders; }
agent OrdersCaller { use orders { required capability orders.create; } }
```

The service provides `orders`; the agent, a native app such as a script or
test, requires the `orders.create` capability, which allows `Create`. Merely
importing an API does not grant permission to call it. See the
[service walkthrough](/guides/write-a-service) for handlers and a working
[Orders example](https://github.com/OATS-Center/trellis/tree/main/docs/examples/orders).
A participant's settings and secrets are `config` members; see
[Settings and secrets](/guides/settings-and-secrets).

## Browser apps and native apps

An app that people sign in to is declared one of two ways:

```trellis
app OrdersWeb { use orders { required capability orders.read; } }
agent OrdersTool { use orders { required capability orders.create; } }
```

- `app` declares a browser app. It signs in by sending the user to the Trellis
  sign-in page and back to its web address, and connects with the browser
  client.
- `agent` declares a native app: a desktop, mobile, or command-line program.
  It signs the user in with a Device Code shown on another screen, through
  `NativeOAuthClient`, and connects with `connectNativeClient` (Rust:
  `NativeOAuthClient`, then the generated app `Client`). Writing
  `native app OrdersTool` is an error that points here.

Nothing else differs: `use` blocks, required and optional capabilities,
consent, State, and `config` work the same for both. The generated request
carries the kind, `kind: "browser"` or `kind: "native"`, and the sign-in
entry points refuse the other kind with an `AppKindMismatch` error that names
both.

Register the app's OAuth client with the same kind. `trellis clients add
--source <project> --app <package.App>` takes the kind from the declaration
and refuses a `--kind` that contradicts it. A native client needs no redirect
URIs when it signs in only with a code. Device Code sign-in against a browser
client fails with an error that names both kinds.

## Operations with progress and live updates

An Operation declares a durable `progress` schema and, optionally, a distinct
transient `update` schema:

```trellis
model ReindexInput { scope: string; }
model ReindexSummary { reindexed: uint64; }
model Status { stage: string; }
model Preview { text: string; }

api orders@v1 {
  title "Orders";
  description "Reindex orders.";
  operation Reindex { input ReindexInput; output ReindexSummary; progress Status; update Preview; }
  capabilities { public { allows { operation Reindex; } } }
}
```

`progress` is written to the durable snapshot; `update` is delivered only to
live watchers and is never persisted. The generated `Progress` and `Update`
types stay distinct. When an Operation declares only `progress`, the live
channel reuses that schema, so existing progress-only contracts are unchanged.
A handler writes durable status with `progress(...)` and transient previews with
`emit_update(...)`.

## Install and generate

```sh
trellis install
trellis generate
trellis generate --check
```

Commit `trellis.toml` and `trellis.lock`. `install` resolves exact locked
dependencies without upgrading; `trellis update` deliberately refreshes them.
`generate` writes the configured language package, and `--check` detects stale
output. If consumers must build without the CLI or package cache, commit the
generated package too. Do not hand-edit generated files.

For a dependency, add a package alias under `[dependencies]` and explicitly
import its types or APIs in the source file. Published dependencies name their
workspace and a version range, `{ package = "acme-common", workspace =
"ghcr.io/acme", version = "^1.4" }`; local development dependencies use a
`path`.
The [contract artifacts guide](/guides/write-a-service/contract-artifacts)
covers the lockfile and build lifecycle. Consult the
[IDL design reference](/design/contracts/trellis-idl) for the full grammar and
compatibility rules.
