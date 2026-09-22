# Trellis Web

The single SvelteKit application for Trellis-owned web surfaces. It currently
hosts the login Portal under `/login` and the Console under `/console`.

Built with SvelteKit, Tailwind CSS v4 + DaisyUI, and
`@qlever-llc/trellis-svelte` for auth and NATS wiring.

## Local dev

1. Start NATS and the Trellis runtime service with this app configured as its
   default web proxy.
2. Copy `.env.example` to `.env` if needed.
3. `deno task install`
4. `deno task dev`

The Vite server listens on `http://127.0.0.1:5173`. Browse the surfaces through
Trellis at `http://localhost:3000/login` and `http://localhost:3000/console`.
The app expects NATS WebSocket at `ws://localhost:8080`.

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
