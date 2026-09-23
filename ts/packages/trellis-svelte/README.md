# @oatscenter/trellis-svelte

Svelte integration for Trellis browser applications.

Provides `TrellisProvider` for app-level wiring and `createTrellisApp(...)` for
app-scoped typed Svelte context around the real connected Trellis client.

Use `TrellisProvider` as the primary integration surface:

- create an app context with `createTrellisApp({ contract, trellisUrl })`
- pass `trellisApp` to `TrellisProvider`
- read the live client synchronously with `app.getTrellis()` from child
  components

Uses the contract/runtime model from `@oatscenter/trellis/contracts` and
`@oatscenter/trellis`.
