# TypeScript Demo

This workspace contains one consolidated Field Ops demo:

- `demos/ts/service`: an installable Field Ops service.
- `demos/ts/device`: an activated field-device TUI.
- `demos/app`: a separate shared browser Field Inspection Desk with its own Deno
  config.
- `demos/ts/shared`: sample data and helpers used by the demo participants.

Use `demos/README.md` as the cross-language entrypoint. This file documents the
TypeScript service and device path, which is the complete end-to-end runtime
path today.

The demo is product-oriented instead of split by Trellis primitive. The browser
app is the Field Inspection Desk demo client: a branded coordinator desk for
daily inspection work that is powered by a local Trellis server. It uses a
full-height left navigation shell and integrated workspace pages for reviewing
the queue, checking site status, running reports, attaching image evidence,
watching live activity, and saving operator notes. Each page still includes a
secondary Trellis callout for the platform concept it exercises.

## Before You Start

1. Make sure Trellis is running at `http://localhost:3000`.
2. Make sure the `trellis` CLI is logged in as an admin.
3. If the browser app should use a non-default Trellis URL, set
   `PUBLIC_TRELLIS_URL` in `demos/app/.env`. The default is
   `http://localhost:3000`.
4. Install registry APIs and generate each project:

```sh
trellis generate --root demos/ts/service
trellis update --root demos/ts/device
trellis update --root demos/app
```

Each project owns one ordinary ESM package under `trellis/`, with executable
JavaScript, declarations, and `apis` and `participants` namespaces. Registry
dependencies use the validated global API cache. Use `trellis generate --watch`
while editing `contract.trellis`; do not edit generated artifacts. Trellis
computes approval identity from the normalized participant interface: editing
display-only metadata such as `displayName` or `description` updates
portal/catalog copy but does not force a new browser, CLI, or device approval
digest.

The demo schemas and surfaces are ordinary Trellis IDL declarations. If you add
templated event subjects, declare `params` in subject-token order and point them
at properties that exist in the event payload schema.

When evolving the demo service during a rollout, keep duplicate RPC, operation,
event, and job payload schemas wire-compatible. The demo schemas intentionally
use normal open TypeBox objects, so adding optional fields is the safe additive
path. Adding required fields, closing objects with closed-object
additional-property rejection, or changing field types requires shrinking the
old deployment authority only after old runtimes are gone, or moving to a new
contract lineage.

## Create And Start The Service

Create one service deployment, review its generated participant authority, then
provision one service instance and start the service with the provisioned
instance seed. After deployment authority includes the service contract
boundary, the service instance can connect and serve traffic.

```sh
trellis deploy create svc/demo.field-ops
trellis --format json deploy provision svc/demo.field-ops
deno task -c demos/ts/deno.json service http://localhost:3000 <instance-seed>
```

Use Console **Admin → Services** as the primary review path for the service
authority change before starting the runtime. For automation, the
`trellis svc <id> authority ...` CLI flow can submit and decide the same
boundary.

Use the `instanceSeed` field from the provision JSON as `<instance-seed>`.

## Create And Start The Device

Create one device deployment, review its generated participant authority,
provision one device instance, then start the TUI with the provisioned root
secret.

```sh
trellis deploy create dev/demo.field-device
trellis --format json deploy provision dev/demo.field-device
deno task -c demos/ts/deno.json device http://localhost:3000 <root-secret>
```

Use Console **Admin → Devices** to review pending device authority before
starting the runtime. For automation, use the matching Trellis CLI authority
flow to submit and decide the same boundary.

Use the `rootSecret` field from the provision JSON as `<root-secret>`.

The first run for a new root secret prints an activation URL and QR code. Unless
the deployment routes device activation to a custom portal, the URL opens the
bundled `trellis-app.portal@v1` portal. Open the URL, approve the device, and
let the TUI continue. Later runs with the same root secret should reconnect
without another approval step.

## Start The Browser App

Start the Svelte Field Inspection Desk after `trellis update --root demos/app`
has generated its participant and API SDK. The app defaults to
`http://localhost:3000` for its Trellis server and can be pointed at another
server with `PUBLIC_TRELLIS_URL`. It keeps local Trellis package and generated
SDK aliases explicitly in `demos/app/svelte.config.js`; `vite.config.js` should
not duplicate those local package mappings.

```sh
deno task -c demos/app/deno.json dev
```

If you change the app participant's requested RPC, operation, event, or state
surface, the next browser sign-in may require approval when the requested
boundary exceeds the existing identity authority. If you only rename the demo
app or adjust its description, existing approval remains valid because that
metadata is not part of the runtime contract evidence.

For ad hoc runs against a non-default Trellis URL, set the env var from the
shell:

```sh
PUBLIC_TRELLIS_URL=http://localhost:3000 deno task -c demos/app/deno.json dev
```

## Field Desk Routes And Trellis Callouts

The browser app routes are stable, but the UI presents them as one integrated
inspection desk workflow. Field Inspection Desk is the client identity; Trellis
appears as the server/platform relationship through copy such as "Powered by
Trellis" and route callouts.

- `Dashboard`: today's field board with queue, status, evidence, and live feed
  context.
- `Assignments`: inspection queue backed by bounded `Assignments.List` pages and
  `Sites.Get` RPC requests.
- `Sites`: site status board backed by bounded `Sites.List` pages and
  `Sites.Get` RPC requests plus the `Sites.Refresh` operation.
- `Reports`: report run workflow with `Reports.Generate` operation progress,
  completion, and cancel.
- `Evidence`: evidence locker with `Evidence.Upload` send transfer and
  `Evidence.Download` receive transfer previews.
- `Activity`: live feed using event subscriptions.
- `Workspace`: operator notes backed by app/device state.

The device TUI exposes the same concepts as menu actions: page through
assignments, view the selected site, refresh a site, generate a report, upload
evidence, watch activity events briefly, and save or page through draft state.
The guided inspection wizard groups those actions into a task-oriented flow so
device runs can exercise the same Trellis surfaces without stepping through each
primitive manually.

The service declares explicit empty `observe` lists for operations that callers
watch and explicit empty `cancel` rights for `Reports.Generate`. This mirrors
runtime permission derivation: `call` starts an operation, `observe` controls
`get`/`wait`/`watch`, `cancel` controls cancellation, and `control` is reserved
for named post-start signals. An omitted `observe` list defaults to `call`,
while the demo's explicit empty `observe` lists make watching available to
authenticated callers without extra capabilities.

Because the demo service intentionally uses empty capability gates for its
public surfaces, its contract does not declare a top-level local `capabilities`
map. In services that define local capability keys, declare that metadata in the
returned contract body rather than in the schema registry argument.

## Jobs Are Private Implementation

Jobs are demonstrated behind the `Sites.Refresh` operation. The caller starts
and watches the public operation; the service uses its private
`refreshSiteSummary` job internally to do the work. There is intentionally no
public job polling API in this demo.

## Event Subscription Demo

The app `Activity` route subscribes to `Audit.Recorded` and `Reports.Published`
with ephemeral event handlers. The device TUI has a matching activity-watch menu
option for a short terminal subscription. Report generation, evidence upload,
and site refresh workflows publish service events that these subscribers can
display.

Prepared outbox dispatch and inbox duplicate suppression are covered by the
integration harness. The Field Ops service itself uses normal direct event
publish for the demo workflows; it is not claiming to persist a production
outbox.

## Cleanup

Revoke activated device access and disable the deployments you created when you
are done:

```sh
trellis deploy activation revoke <device-instance-id>
trellis deploy disable svc/demo.field-ops
trellis deploy disable dev/demo.field-device
```

Use each provision JSON's `instanceId` field for `<service-instance-id>` and
`<device-instance-id>`.
