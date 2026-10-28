# Auth0 upstream OIDC smoke

Auth0 authenticates the user upstream. Trellis remains the OAuth authorization
server for applications and owns capability consent, refresh families, and NATS
authority. Automated Dex checks do not establish Auth0 interoperability. This
procedure requires a test Auth0 tenant and has not been executed against Auth0.

## Configuration

1. Start Trellis with a public HTTPS OAuth portal origin separate from Console
   and applications. Keep an independently usable local administrator account.
2. Create an Auth0 **Regular Web Application** for the Trellis upstream adapter.
   Enable Authorization Code and configure the exact allowed callback URL
   `https://<trellis-portal>/portal/oidc/callback`. Use the tenant/custom-domain
   issuer from its OIDC discovery document, including the trailing slash. Auth0
   needs this portal callback, not the browser application's localhost callback.
3. Through the generated Auth `OIDCProviders.Put` NATS RPC, configure that issuer,
   Auth0 client ID/secret, title, scopes `openid`, `profile`, `email`, and a
   3600-second claim freshness. Use expected revision zero for creation and a
   fresh idempotency key. Keep the secret in server configuration custody, never
   the application bundle. No upstream refresh-token storage is needed.
4. If mapping Auth0 roles, use an Auth0 post-login Action to include a namespaced
   array claim in the **ID token**, for example `https://example.com/roles`.
   Configure Trellis `roleClaim` as its JSON pointer
   `/https:~1~1example.com~1roles`, then map exact claim values to existing Trellis
   roles through `OIDCRoleMappings.Put`. An email/name is not identity evidence.
5. Register the downstream browser development client with its exact origin and
   callback port through `OAuthClients.Put`. Its client ID is distinct from the
   Auth0 upstream client ID. Grant only the capabilities needed for this smoke.

## Browser and lifecycle checks

1. In a fresh Chromium profile, open the ordinary application sign-in flow and
   click the portal's company-login link. Confirm top-level navigation reaches
   Auth0 and returns to Trellis consent under the existing restrictive CSP.
2. Approve one capability and decline an optional capability. Complete the
   callback, connect through the public SDK, and call the allowed generated API.
   Confirm the declined API is denied. Repeat using a localhost development app
   against the remote issuer; the issuer must not make a request to localhost.
3. Reload, open a second tab, and refresh concurrently. Both tabs retain the same
   account and approved subset without a second durable runtime/NKey credential.
   Restart the browser and resume. Switch accounts explicitly and confirm no
   previous user's grant or State appears under the newly selected account.
4. Remove the mapped Auth0 role, perform fresh upstream authentication, and
   confirm affected old Trellis sessions lose that mapped authority. A separately
   granted direct capability remains usable. Restoring the role on a later login
   must not silently add capabilities to an older grant that never approved them.
5. Revoke the Trellis delegation/login through the generated management RPCs;
   confirm reconnect/refresh fails for that family. Disable the OIDC provider and
   confirm new upstream sign-in fails. Do not equate Trellis logout with clearing
   Auth0's own SSO cookie; test explicit account switching at Auth0 as well.

Record tenant issuer, browser version, exact callbacks, capability outcomes, and
which steps ran. Exclude tokens, cookies, client secrets, and private keys from
the report. Restore test role/client/provider settings when finished.
