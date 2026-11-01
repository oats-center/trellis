# Trellis Web

The required SvelteKit Console application under `/console`. OAuth login and
consent belong to the separately built [OAuth portal](../portal/README.md),
served by Auth on its dedicated origin and listener, not by this app.

Built with SvelteKit, Tailwind CSS v4 + DaisyUI, and
`@oatscenter/trellis-svelte` for auth and NATS wiring.

## Local dev

The Console is compiled into `trellis-server`; the runtime does not proxy a
Vite dev server. To try a change in the real runtime:

1. Run `npm ci` at the repository root (Node 24 LTS and npm 11).
2. Run `npm run browser:embedded` at the repository root. It builds this app and
   the OAuth portal into `crates/runtime/generated/`.
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

`trellis install` refreshes the locked contract artifacts and local SDK. The
local Trellis context binds `createTrellisApp` to the console contract and
derives `TrellisConsoleClient` with `TrellisClientFor<typeof contract>`, so
console pages call `getTrellis()` with explicit RPC, event, and state types
without importing a generated `client.ts` facade.

The console is an app contract, not a control-plane extension. Its contract
declares the exact Auth, Health, and Jobs surfaces it calls or subscribes to;
runtime permissions come from those `uses` declarations plus the authenticated
user's admin capabilities. Admin device revoke/disable flows now also clear the
device's durable session by public identity key so console state reflects the
runtime access decision.
