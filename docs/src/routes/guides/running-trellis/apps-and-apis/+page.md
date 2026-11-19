---
title: "Register apps and review APIs"
description: "Register an OAuth client for a browser or native app, and review or accept changes to the API catalog, from the CLI or the Console."
---

Both of these are administrator tasks. The Console has them under
**OAuth clients** and **API catalog**; the CLI commands below do the same, once
you've run `trellis login`.

## Register a development client

An administrator with `clients.manage` can register a browser app running on
`localhost` without installing an app participant for it:

```sh
trellis clients add
```

The command asks for each setting in turn: the app's client ID, whether it's a
browser or native app, its title, whether it's a development app, its redirect
URIs, the capabilities it gets without asking, and whether users can sign up
through it. It checks each redirect URI as you type it, shows a summary, and
saves once you confirm. Every answer is also a flag, so a script can register
the same app without a terminal:

```sh
trellis clients add my-local-app --kind browser --title "My local app" \
  --description "Local browser development" --development \
  --redirect-uri http://localhost:5173/login
```

Without a terminal, or with `--no-input`, a missing required flag fails at once
and names every flag still needed; nothing is registered. To change a client
later, `trellis clients edit my-local-app` asks the same questions starting from
its current values, and `clients edit my-local-app --title "New title"` changes
only what you pass.

A script can also send the generated request as JSON. Save it as
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
distinct change. Then (`-` reads the request from standard input):

```sh
trellis clients put client.json
trellis clients get my-local-app
trellis clients list
```

Redirect ports and origins have to match exactly. Trellis never calls back to
`localhost` itself; only the browser does. To update a client with `put`, send
the revision `get` returned. `disable`, `enable` and `delete` take
`--expected-revision`, so a stale revision fails instead of overwriting another
administrator's change.

A disabled app stays disabled when you change its settings, whether with
`edit`, `put` or the Console. To turn it back on, run
`trellis clients enable my-local-app --expected-revision <n>` or use **Turn on**
in the Console.
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

An incompatible replacement needs `apis.forceReplace`:

```sh
trellis apis force-replace --source <project-root> --participant <package>.<Name>
```

It reviews each accepted API the participant changes, shows the review, and
asks you to accept that callers may break and that what users agreed to may now
mean something different. Leave out `--source` or `--participant` and it asks
for them too. A script passes `--acknowledge-breakage` and
`--acknowledge-consent-impact` instead; without a terminal, leaving either out
fails before anything is replaced.

When the review asks for role or mapping edits, send an
`AuthApiForceReplaceRequest` file instead:
`trellis apis force-replace replacement.json`. The request sets both
`acknowledgeBreakage` and `acknowledgeConsentImpact` along with the edits.
`trellis apis list` and `trellis apis get <api-id>` show what's currently
accepted.
