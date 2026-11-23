# Trellis Web

The required SvelteKit Console application under `/console`. OAuth login and
consent belong to the separately built [OAuth portal](../portal/README.md),
served by Auth on its dedicated origin and listener, not by this app.

Built with SvelteKit, Tailwind CSS v4 + DaisyUI, and
`@oatscenter/trellis-svelte` for auth and NATS wiring.

## Local dev

The Console is compiled into `trellis-server`; the runtime does not proxy a
Vite dev server. To try a change in the real runtime:

1. Run `npm ci` at the repository root (Node 24 or 26, tested on 26, and npm 11).
2. Run `npm run browser:embedded` at the repository root. It builds this app and
   the OAuth portal into `crates/runtime/generated/`, with a fingerprint of
   their sources. A development build of `trellis-server` fails while that
   fingerprint no longer matches the sources.
3. Rebuild and restart the server: `cargo build -p trellis-server`.
4. Open `http://localhost:3000/console`.

Use `npm run check` in this directory for type and Svelte checks between builds.

## The standalone image

`trellis-server` already serves the Console. `web/Containerfile` builds an
nginx image of the same app for an installation that wants the Console's
hostname answered by its own containers. Create the installation with that
hostname, `trellis-server init --console-origin https://console.example.com`,
so Trellis registers the Console's sign-in there, and point the hostname at the
image instead of Trellis. The image needs both origins:

```sh
docker run -e TRELLIS_ORIGIN=https://trellis.example.com \
  -e CONSOLE_ORIGIN=https://console.example.com -p 8080:80 trellis-console
```

## Contract

The Console has no contract source of its own. It is the built-in `app console`
declared in `crates/builtin-contracts/src/builtin_source/participants.trellis`,
and that declaration is the only description of its capability request. Change
what the Console asks for there.

`cargo xtask install` generates the built-in package's TypeScript into
`ts/packages/trellis/internal_sdk/generated`. `src/lib/trellis-context.svelte.ts`
binds `createTrellisApp` to that package's `console` request and derives
`TrellisConsoleClient` with `TrellisClientFor<typeof consoleRequest>`, so pages
call `getTrellis()` with explicit RPC, event, and state types.
`src/lib/trellis-apis.ts` re-exports the same package's `apis` for the
descriptors and types pages import.

The Console is an app participant, not a control-plane extension. Its
declaration lists the exact Auth, Core, State, Health, Jobs, and Events
capabilities it may call or subscribe to; runtime permissions come from those
`use` declarations plus the authenticated user's admin capabilities. Admin device revoke/disable flows now also clear the
device's durable session by public identity key so console state reflects the
runtime access decision.
