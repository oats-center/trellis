---
title: "Register apps and review APIs"
description: "Register an OAuth client for a browser or native app, and review or accept changes to the API catalog, from the CLI or the Console."
---

Both of these are administrator tasks. The Console has them under
**OAuth clients** and **API catalog**; the CLI commands below do the same, once
you've run `trellis login`.

## Register a development client

An administrator with `clients.manage` can register a browser app running on
`localhost` without installing an app participant for it. Save the request as
`client.json`:

```json
{
  "clientId": "my-local-app",
  "expectedRevision": "0",
  "title": "My local app",
  "description": "Local browser development",
  "kind": "browser",
  "development": true,
  "redirectUris": ["http://localhost:5173/login"],
  "impliedConsent": [],
  "idempotencyKey": "01ARZ3NDEKTSV4RRFFQ69G5FAV"
}
```

That's an `AuthOAuthClientPutRequest`. Use a new idempotency key for each
distinct change. Then:

```sh
trellis clients put client.json
trellis clients get my-local-app
trellis clients list
```

Redirect ports and origins have to match exactly. Trellis never calls back to
`localhost` itself; only the browser does. To update a client, send the
revision `get` returned. `disable` and `delete` take `--expected-revision`, so
a stale revision fails instead of overwriting another administrator's change.
The commands print the generated JSON, including the cursor for
`list --cursor`.

Which sign-in flows a client gets follows from its kind and callbacks, on
every update:

| Client                   | Flows                                           |
| ------------------------ | ----------------------------------------------- |
| browser                  | Authorization Code and refresh; needs callbacks |
| native without callbacks | Device Code and refresh                         |
| native with callbacks    | both, and refresh                               |

Adding callbacks to an existing native client turns on Authorization Code, and
removing them turns it off. A native client's plain-HTTP `localhost` callbacks
don't need the development flag; a browser client's do. A client's kind can't
change, so register a new client ID to switch between native and browser.

## Review an API catalog change

```sh
trellis apis review --source <project-root> --participant <package>.<Name>
```

This compiles the package and reviews each API the participant implements:
its digest, whether it's compatible, which roles and deployments it affects,
its generation, and the revision currently accepted. `--participant` can be
left out when the package has only one.

`trellis apis accept` with the same flags reviews again and accepts every
compatible API that isn't already accepted at that definition. It needs
`apis.accept`, and it only succeeds against the revision it reviewed.

An incompatible replacement goes through
`trellis apis force-replace replacement.json` with an
`AuthApiForceReplaceRequest`. That needs `apis.forceReplace`, and the request
has to set both `acknowledgeBreakage` and `acknowledgeConsentImpact`, plus any
role or mapping edits the review asked for. `trellis apis list` and
`trellis apis get <api-id>` show what's currently accepted.
