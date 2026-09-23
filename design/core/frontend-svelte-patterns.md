---
title: Frontend Svelte Patterns
description: Trellis frontend guidance for Svelte applications and state-management conventions.
order: 80
---

# Design: Frontend Svelte Patterns

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md) - Trellis architecture and
  service boundaries

## Scope

This document defines contributor conventions for Trellis-owned Svelte apps and
components. Its state, layout, styling, and file-organization preferences are
not requirements for applications that consume Trellis. Those applications
choose their own frontend architecture; the optional
`@oatscenter/trellis-svelte` package's connection/context APIs are documented in
the Svelte integration guide.

## Svelte 5 State Pattern

Conductor-style Svelte apps use Svelte 5 runes for reactive state.

```ts
class Auth {
  #state: AuthState = $state({ handle: null, nonce: null });

  get handle() {
    return this.#state.handle;
  }

  async signIn(options?: {
    redirectTo?: string;
    landingPath?: string;
    context?: unknown;
  }): Promise<void> {
    // mutate #state, reactivity propagates automatically
  }
}

export const authState = new Auth();
```

Patterns:

- private `#state` field with `$state()`
- public getters and no public setters
- methods own mutations
- static factory methods handle async initialization when needed

## Console Read and Mutation Pattern

Trellis-owned console pages follow four stable conventions. They apply to
operator surfaces under the console app; consuming applications choose their
own.

### Page reads use a request scope

Every asynchronous page read belongs to a request scope keyed by the page's
semantic target and filter values. Capture those values before awaiting, and
commit data, errors, and pagination cursors only while the token is current. A
target change clears the previous target's data; a same-target refresh may keep
a labelled snapshot. Component disposal invalidates outstanding reads so no late
response mutates a page that has gone away. Do not make the initiating `$effect`
itself async.

### Lists use cursor pagination

Tables advance one server page at a time with an opaque cursor history and reset
that history when a server filter changes. Selectors and catalogs that need
completeness traverse pages with a bounded limit and a cursor-cycle guard, and a
failed later page is reported as an incomplete catalog rather than a complete
one. Exact-target pages resolve through the resource's `Get` where the contract
offers one, and otherwise through correctly filtered complete traversal; a
missing target renders an unavailable state and never falls back to the first
loaded record.

### Mutations use an immutable intent

Capture the exact endpoint, target identifiers, displayed label, expected
version, editable values, and one idempotency key when the operator confirms.
Confirmation and the request both use that frozen intent, never a mutable
selection reread after an `await`. Update the local record from the returned
result or a documented exact refresh. Preserve the draft on a conflict; treat an
uncertain transport outcome as unknown rather than successful or failed.

### Live views schedule coalesced refreshes

A watch is established once per semantic scope and tied to component disposal,
not restarted on every data refresh. Change events schedule a non-resetting
coalesced refresh with at most one read in flight and one trailing refresh, so
continuous events cannot starve the view. A live indicator reflects the actual
feed lifecycle; the general connection badge does not.

## Browser App Runtime Pattern

Svelte browser apps should split responsibilities between one app-local module
and Svelte context.

```ts
// src/lib/trellis.ts
import { env } from "$env/dynamic/public";
import {
  createTrellisApp,
  type TrellisClientFor,
} from "@oatscenter/trellis-svelte";
import contract from "$lib/contract";

type MyAppClient = TrellisClientFor<typeof contract>;

function publicTrellisUrl(): string {
  return new URL(env.PUBLIC_TRELLIS_URL ?? "http://localhost:3000")
    .toString()
    .replace(/\/$/, "");
}

export const trellisUrl = publicTrellisUrl();

export const trellisApp = createTrellisApp({ contract, trellisUrl });

export function getTrellis(): MyAppClient {
  return trellisApp.getTrellis();
}

export function getConnection() {
  return trellisApp.getConnection();
}
```

Rules:

- the app-local module owns static app metadata and typed helpers
- browser apps should let `createTrellisApp` derive the connected client type
  from the app contract, using `TrellisClientFor<typeof contract>` for local
  helper annotations when an explicit name is useful
- in the common fixed-instance case, the app-local module should resolve the
  fixed `trellisUrl` once and pass it to `createTrellisApp`
- `TrellisProvider` should receive an app-owned `trellisApp` created with
  `createTrellisApp({ contract, trellisUrl })`
- `trellis-svelte` should keep the connected Trellis client and reactive
  connection adapter scoped to that app context rather than exposing a synthetic
  runtime bag
- normal pages should import app-local helpers such as `getTrellis` and
  `getConnection`; they should not rebuild auth config just to make an RPC call
- `getTrellis()` and `getConnection()` are Svelte context getters; call them
  during component initialization and store the result in a top-level `const`,
  never inside `onMount`, event handlers, async helper functions, or later
  callbacks
- Svelte context is the runtime transport for the live Trellis instance and
  related browser state; the app-local module is the static typing boundary that
  keeps contract knowledge out of arbitrary page files
- generated client facades do not exist; the app contract is the typing source
  for `createTrellisApp` and `TrellisProvider`
- do not generate or import an app SDK just to type `getTrellis()`; the app
  contract passed to `createTrellisApp` infers its flat caller surface, while
  generated service SDK imports provide the descriptors selected by that app
- SvelteKit apps should usually source that fixed instance URL from public env
  such as `PUBLIC_TRELLIS_URL`; use `$env/dynamic/public` when local demos need
  a safe default and `$env/static/public` when the value must be fixed at build
  time
- apps that let the user choose an auth instance at runtime should pass a
  resolver to `createTrellisApp`, for example `trellisUrl: () => selectedUrl`,
  and update that selected value before rendering `TrellisProvider`; this should
  remain an explicit advanced pattern rather than the default guide story

## Browser Auth Session Pattern

Browser apps should make session-key persistence an explicit UX choice:

- temporary sessions use a memory-only non-extractable WebCrypto key and end
  when the tab/app session is discarded
- remembered sessions use an IndexedDB-stored non-extractable WebCrypto key plus
  expiry metadata
- both modes still rely on Trellis session TTL, revocation, and fresh
  per-request proofs; IndexedDB persistence is not a bypass for auth policy
- `session_not_found` should be treated as an auth-required state that sends the
  user through the configured login flow with the current return URL

## Local Workspace Alias Pattern

SvelteKit apps that consume local workspace packages must keep Deno, Vite, and
the Svelte/TypeScript editor on the same package graph.

Rules:

- installed registry packages do not need aliases; let the package manager and
  normal resolver handle them
- local generated service SDK packages need SvelteKit aliases unless they are
  installed packages
- if Trellis itself is local-linked, alias the package root
  `@oatscenter/trellis` and every Trellis subpath the app or generated SDKs
  import
- keep local frontend aliases in the app's `svelte.config.js` `kit.alias`
  object; SvelteKit generates the `.svelte-kit/tsconfig.json` path mappings used
  by editor tooling and `svelte-check`, and passes those aliases to Vite for
  SvelteKit builds
- do not duplicate the same local package mappings in `vite.config.js`
- order explicit alias entries from most specific to least specific, with
  Trellis subpaths before the `@oatscenter/trellis` package root, because Vite
  resolves aliases by prefix

The Trellis repo's local frontend apps keep explicit `kit.alias` objects in each
SvelteKit config. App workspaces should resolve their single generated ESM
package through an ordinary local package dependency, or an app-local alias to
its `index.js`. The package exposes `apis` and `participants`; do not add
per-API aliases or mappings into an installation tree.
