# Trellis OAuth portal

This is the OAuth server's separately compiled login and consent application. It
does not import Console code, Trellis application clients, or ordinary app
scripts. Its public inputs are authorization transaction identifiers; passwords
are submitted directly to Auth using native forms and are not retained in
component state. The portal never receives an application's private DPoP key.

Install workspace dependencies with `npm ci` from the repository root using
Node 24 LTS and npm 11. From this directory:

```sh
npm run check
npm run build:embedded
```

The embedded build stages the complete Vite output before promoting it into
`crates/runtime/generated/portal`. Failed compilation preserves the previous
bundle. The Rust OAuth router serves these assets only beneath its reserved
origin and applies CSP, frame restrictions, and no-store responses to portal
pages. Ordinary application assets are not served by this router.

## Cookie isolation

Prefer a dedicated OAuth hostname, such as `auth.example.com`, distinct from
Console and application hostnames. Origin separation is mandatory, but a
different port does **not** isolate cookies: another service on the same
hostname can receive the OAuth portal's `__Host-` cookies. `HttpOnly` prevents
script access, not delivery to another server on that hostname.

Auth logs a prominent security warning for same-hostname/different-origin
configuration; it does not prohibit that configuration. Secure and HttpOnly
cookies, CSRF checks, and exact redirect validation remain mandatory.
