---
title: Receiving Webhooks
description: Proposal for HTTP webhook endpoints that services declare in IDL. Trellis serves them, checks that each request is genuine, and delivers accepted requests to the service as ordinary events.
order: 47
---

# Design: Receiving Webhooks

> **Status: proposal awaiting review.** Nothing in this document is
> implemented. The questions it raised are settled in
> [Decisions](#decisions) at the end.

A webhook is an HTTP request that another system sends when something happens
on its side. Stripe sends one when a payment succeeds, and GitHub sends one when
someone pushes a commit. Today a Trellis service that wants these requests has
to run its own HTTP listener, check signatures with hand-written code, and keep
the signing secret somewhere. If it crashes before writing a request down, the
request is lost.

This proposal moves that work into Trellis. A service declares a webhook
endpoint in its IDL. Trellis serves the endpoint, checks that each request
really comes from the sender, and hands accepted requests to the service as
ordinary Trellis events. Sending webhooks is out of scope.

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md): events and subjects
- [config-patterns.md](./config-patterns.md): service settings and secrets
- [../auth/service-updates.md](../auth/service-updates.md): how a new resource
  waits for approval

## How it works

In this document the sender is the outside system that sends the webhook, and
the service is the Trellis service that declared the endpoint. Every request to
an endpoint goes through four steps:

1. **Intake.** Trellis receives the request and checks its method, body size,
   request rate, and source address.
2. **Verification.** Trellis checks that the request is genuine, usually by
   checking a signature made with a secret the sender and the service share.
   An endpoint can also hand the request to the service and let the service
   decide.
3. **Recording.** Trellis records every request and its outcome. A request
   that passes verification becomes a `Received` event.
4. **Delivery.** The event reaches the service like any other event, with the
   usual retries. An event that keeps failing becomes a dead letter, which an
   administrator can replay later.

Each endpoint has a **mode** that decides when Trellis answers the sender:

- **`async`** stores the request, answers 2xx at once, and verifies afterwards.
  The sender gets 2xx even for a request that later fails verification. In
  return, nothing is lost while the service is down, and a request rejected
  because of a wrong secret can be verified again once the secret is fixed.
- **`sync`** verifies first and answers only after the request is recorded. A
  request that fails verification gets 401. Handshakes, and senders that read
  the response body, need this mode.

Trellis never parses the body. The service reads the bytes, decides what they
mean, and publishes its own domain events if other services need them.

## Declaring an endpoint

`webhook` is a new resource kind, next to `kv`, `store`, `job`, and `consumer`.
Only services can declare one. This declaration receives Stripe webhooks:

```trellis
service Billing {
  config stripeSigningSecret {
    title "Stripe signing secret";
    description "The whsec_ value shown for this endpoint in the Stripe dashboard.";
    type string;
    secret;
  }

  webhook stripe {
    title "Stripe";
    description "Payment and subscription events from Stripe.";
    mode async;
    verify hmac {
      key config stripeSigningSecret;
      algorithm sha256;
      signed "{timestamp}.{body}";
      signature { header "Stripe-Signature"; separator ","; prefix "v1="; encoding hex; }
      timestamp { header "Stripe-Signature"; separator ","; prefix "t="; format unix_seconds; tolerance 5m; }
    }
  }
}
```

| Member                                          | Meaning                                                                       | Default                  |
| ----------------------------------------------- | ----------------------------------------------------------------------------- | ------------------------ |
| `mode async \| sync`                            | When Trellis answers the sender                                               | `async`                  |
| `methods [POST, GET]`                           | The accepted HTTP methods. Others get 405.                                    | `[POST]`                 |
| `verify <kind> { ... }`                         | A [standard verifier](#standard-verifiers). All declared verifiers must pass. | required unless `decide` |
| `decide { deadline 2s; }`                       | Let the service [decide](#when-the-service-decides)                           | absent                   |
| `status 200;`                                   | The status for an accepted request                                            | `200`                    |
| `content_encoding [gzip];`                      | Encodings Trellis decodes before verifying. Others get 415.                   | none                     |
| `dedupe "<template>";`                          | Where the sender's delivery ID is, for [dropping duplicates](#duplicates)     | absent                   |
| `concurrency N;`, `retry { ... }`               | How events reach the service, as for a `consumer`                             | the consumer defaults    |
| `max_body`, `dedupe_window`, `rotation_overlap` | The service's own default for a [runtime setting](#runtime-settings)          | the Trellis default      |

The compiler enforces four rules:

- A key, token, or password must name a `secret` setting of the same service.
- An endpoint needs at least one `verify` or a `decide`. An endpoint that checks
  nothing must say `verify none;`, which cannot be combined with other
  verifiers.
- A template with an unknown placeholder is an error, and so is `{timestamp}`
  without a `timestamp` block.
- A `decide` deadline is at most 10 s in `sync` mode and 60 s in `async` mode.

Every member except `title` and `description` is part of the participant
digest, the hash that identifies a service build. Changing one therefore makes a
new build, which waits for [approval](#approval). The generated SDKs give each
endpoint a handle for registering its event handler and, with `decide`, its
decide handler.

## The URL

Each endpoint has one URL on the normal Trellis origin:

```text
https://<trellis origin>/webhooks/<service name>/<random id>
```

The service name is the name the administrator gave the service when adding it,
such as `billing`, so logs show which service a request was for. The random ID
is 128 random bits, written as 22 characters of base64url. It makes the URL
impossible to guess, which keeps scanners away from endpoints. That is not
authentication, but it matters for endpoints that use `verify none`.

The URL stays the same across new builds, service updates, and Trellis
upgrades. All instances of a service share it, and two services installed from
the same build get different URLs. It changes only when the endpoint is removed
or an administrator rotates it. `trellis svc <name> webhooks <endpoint>
rotate-url` assigns a new random ID, and the old one keeps working until
`retire-url`, so the administrator has time to change the URL at the sender.

Routing follows these rules:

- `/webhooks/` is a reserved path prefix. An unknown path gets 404. The query
  string passes through unchanged.
- When the Console has its own hostname, that hostname does not serve webhook
  paths.
- A proxy in front of Trellis must keep the `Host` header, the path, and the
  query unchanged. Senders that sign the URL, such as Twilio, fail otherwise.
- Webhook routes never send CORS headers. They have their own request budget,
  so a flood of webhooks cannot lock people out of sign-in.
- With a generated TLS certificate, senders on the internet will not trust
  Trellis, and the Console warns about that next to webhook URLs.

## Standard verifiers

A verifier is a check that a request is genuine. Trellis first runs the cheap
checks, the ones that need no secret: method, body size, content encoding, rate
limits, and `verify address`.
A request that fails one of them is answered at once in both modes, and its
body is not stored. The other verifiers then run in declared order. Every
comparison of a secret or signature takes constant time, so an attacker cannot
learn part of a secret by timing the comparison.

### `hmac`

An HMAC is a signature made by hashing the request together with a shared
secret. The sender computes it and sends it in a header. Trellis computes it the
same way and compares.

```trellis
verify hmac {
  key config <secret setting>;
  key_prefix "whsec_";            // optional; removed before decoding
  key_encoding text;              // text | hex | base64 | base64url
  algorithm sha256;               // sha1 | sha256 | sha512
  signed "<template>";
  signature { header "<name>"; separator "<sep>"; prefix "<prefix>"; encoding hex; }
  timestamp { header "<name>"; separator "<sep>"; prefix "<prefix>"; format unix_seconds; tolerance 5m; }
}
```

- **Key.** Trellis removes `key_prefix` from the secret and decodes the rest
  with `key_encoding`. With `text`, the secret's UTF-8 bytes are the key.
- **Signature.** Trellis splits each value of the named header on `separator` and
  keeps the items that start with `prefix`. Without `separator`, the whole
  value is one item. It tries up to 8 signatures, because some senders send
  several while they change secrets.
- **Signed content.** `signed` is a template for the exact bytes the HMAC
  covers. `{{` and `}}` are literal braces.

  | Placeholder         | Bytes                                                                     |
  | ------------------- | ------------------------------------------------------------------------- |
  | `{body}`            | The body as received, or after decoding `content_encoding`                |
  | `{timestamp}`       | The timestamp text exactly as sent                                        |
  | `{header:Name}`     | The first value of that header. A missing header fails verification.      |
  | `{method}`          | The request method                                                        |
  | `{url}`             | The configured Trellis origin plus the path and query exactly as received |
  | `{path}`, `{query}` | The path, and the query without its `?`                                   |
  | `{form_sorted}`     | The form-encoded body sorted by field name, each name then its value      |

- **Timestamp.** Trellis reads the timestamp the same way as a signature, in
  `unix_seconds`, `unix_millis`, or `rfc3339`. The request fails when the
  timestamp is more than `tolerance` from the time Trellis received the
  request. An `async` request verified again a day later still passes, because
  the receive time is what counts.
- **Match.** The request passes when any of its signatures matches under any
  [accepted key](#changing-a-secret). Trellis records which key matched.

For example, Twilio's scheme is declared as `algorithm sha1; signed "{url}{form_sorted}";
signature { header "X-Twilio-Signature"; encoding base64; }`.

### `token` and `basic`

```trellis
verify token { secret config gitlabToken; header "X-Gitlab-Token"; }
verify token { secret config hookToken; query "token"; }
verify token { secret config apiKey; header "Authorization"; prefix "Bearer "; }
verify basic { username config partnerUser; password config partnerPassword; }
```

`token` requires a header value (after `prefix`) or a query parameter to equal
a secret. `basic` requires matching HTTP Basic credentials. Trellis removes the
header or query parameter that carried the secret from everything it records
and delivers.

### `address`

```trellis
verify address { ranges config stripeAddresses; }
```

The request must come from one of the CIDR ranges in a plain `list<string>`
setting, so an administrator can follow the sender's published list without a
new build. Behind a proxy, Trellis knows the real client address only when the
`trusted_proxies` server setting lists that proxy. An address check is weak alone, so combine it with another
verifier when the sender allows it.

### `none`

The endpoint checks nothing. The Console marks it as unauthenticated, and
approving it needs an [explicit acknowledgement](#approval).

## Secrets

Webhook verification secrets are the service's ordinary settings secrets. The
service declares them with `config ... secret;`, and an administrator sets them
in the Console or with `trellis svc <name> config set-secret`. Trellis already
holds every settings secret, sealed with its storage key.

Webhook intake decrypts the secrets it needs inside the Trellis process and
caches them in memory until the settings change. It never writes the decrypted
values anywhere, and it never calls the service while it verifies a webhook.
This is a deliberate trust decision: Trellis already holds these secrets, and
verifying inside Trellis lets a request be checked and stored while the service
is down. The security review should list webhook intake as a new reader of
service secrets.

Verifiers use the service's own settings. Per-instance overrides do not apply,
because the endpoint belongs to the whole service. While a required secret is
not set, an `async` request is stored as `pending` and verified once the secret
is set, and a `sync` request gets 503.

### Changing a secret

When an administrator changes a signing secret, the sender may keep signing
with the old one for a while. The two sides cannot switch at the same moment,
and some senders sign with both secrets during their own change. So after a
change, Trellis accepts both the new secret and the previous one for an overlap
period, 24 hours by default and changeable at [runtime](#runtime-settings).

Deleting the earlier settings revision with `trellis svc <name> config
delete-revision <revision>` ends the overlap at once and destroys the old
secret. The Console shows when the old secret last matched a request, so the
administrator can see when the sender has switched.

## The two modes step by step

**`async`:**

1. Trellis runs the cheap checks.
2. Trellis writes the raw request to the intake stream and waits until NATS
   JetStream confirms the write. The intake stream is a JetStream stream, one per
   deployment, that holds each request's method, URL, headers,
   body, client address, and receive time.
3. Trellis answers the declared status, or 503 if the write failed.
4. A worker reads the request back, runs the verifiers and the decide handler,
   and checks for duplicates. A request that passes becomes a `Received` event.

**`sync`:**

1. Trellis runs the cheap checks, then the verifiers. A failure gets 401.
2. The decide handler runs, if declared, within its deadline.
3. Trellis checks for duplicates and publishes the `Received` event.
4. Trellis answers only after JetStream confirms the event. If the publish
   fails, it answers 503 even when the service accepted, so the sender retries
   and Trellis never confirms a request it has not stored.

A rejected `sync` request is written to the intake stream too, so an
administrator can see what failed and verify it again.

| Situation                                               | Status                                                          |
| ------------------------------------------------------- | --------------------------------------------------------------- |
| Unknown or retired URL                                  | 404                                                             |
| Endpoint or deployment disabled by an administrator     | 503, and nothing is stored                                      |
| Method not declared                                     | 405 with `Allow`                                                |
| Body over the size limit                                | 413                                                             |
| Content encoding not declared                           | 415                                                             |
| Rate limit reached                                      | 429 with `Retry-After`                                          |
| `verify address` fails                                  | 403                                                             |
| `sync`: a verifier fails, or the service rejects        | 401                                                             |
| `sync`: the decide handler fails or misses its deadline | 503                                                             |
| Storage unavailable or full                             | 503                                                             |
| Accepted, ignored, or duplicate                         | The service's response if it gave one, else the declared status |
| `async`, once stored                                    | The declared status, whatever verification finds later          |

Error responses have an empty body, so they tell a prober nothing.

## When the service decides

The standard verifiers cover most senders. The service's decide handler covers
the rest:

- handshakes, such as Slack's setup request that must get its `challenge`
  value back;
- signature schemes that no standard verifier describes;
- confirmation by calling the sender back, as PayPal requires;
- response bodies the sender reads, such as a Twilio TwiML document.

Trellis calls the decide handler with an ordinary signed Trellis RPC on the
subject `webhooks.v1.decide.<b64(service)>.<b64(endpoint)>`. Only instances
whose build declares the endpoint receive it. Standard verifiers declared on
the same endpoint run first, so the handler sees only requests that passed
them.

The handler returns a verdict:

- `accept`: the request is genuine and becomes a `Received` event.
- `ignore`: the request is genuine but does not become an event. Handshakes,
  pings, and requests the handler already fully processed return `ignore`, so
  the service does not handle them twice.
- `reject`: the request is not genuine. An unknown verdict counts as `reject`.

If the handler fails or misses its deadline, a `sync` request gets 503. An
`async` request is tried again after 5 s, 30 s, 2 min, and 10 min, then every
10 min until it leaves the intake stream, when it is recorded as `expired`.

```trellis
enum WebhookMode { "async"; "sync"; }
enum WebhookVerdict { "accept"; "ignore"; "reject"; }

model WebhookDecideRequest {
  delivery: WebhookDelivery;
  mode: WebhookMode;
  /// 1 for the first call. Async decide calls are retried.
  attempt: uint32;
  deadline: timestamp;
}

model WebhookResponse { status: uint32; headers: list<WebhookField>; body: bytes; }

model WebhookDecision {
  verdict: WebhookVerdict;
  /// Sent to the sender in sync mode. Ignored in async mode.
  response?: WebhookResponse;
  /// Recorded with the request. Never sent to the sender.
  reason?: string;
  /// Replaces the declared dedupe key, for senders that put their ID in the body.
  dedupeKey?: string;
}
```

A `sync` handler that does the work itself and returns a real answer, such as
TwiML, should return `ignore`, or return `accept` and make its event handler
safe to run twice.

The SDKs add three general helpers for decide handlers: compute an HMAC and
compare it with a signature in constant time, check a timestamp against a
window, and build a `WebhookResponse`. No helper is named for a particular
sender; one sender's handshake code belongs in the service.

### What a response may contain

A decide response is served from the Trellis origin, which also serves the
sign-in pages. A response that set a cookie there could change a person's
sign-in state, and a script served there would run with the sign-in pages'
access. So Trellis checks each response before sending it:

- The status is between 200 and 599, and the body is at most 64 KiB.
- There are at most 16 headers. `Set-Cookie`, hop-by-hop headers, and the
  security headers Trellis sets are refused.
- `Content-Type` is `text/plain`, `application/json`, `application/xml`,
  `text/xml`, `application/soap+xml`, or `application/x-www-form-urlencoded`.
  HTML and script types are refused.
- Trellis adds `Content-Security-Policy: sandbox; default-src 'none'`,
  `X-Content-Type-Options: nosniff`, and `Cache-Control: no-store`, which tell
  the browser to run nothing from the response and not to guess its type.

A response that breaks a rule is replaced with 502, and the request is recorded
as `invalid_decision`.

## Examples

**GitHub** signs only the body and sends no timestamp, so its delivery ID is
the only guard against a replayed request:

```trellis
webhook github {
  title "GitHub";
  description "Push, pull request, and issue events from GitHub.";
  verify hmac {
    key config githubWebhookSecret;
    algorithm sha256;
    signed "{body}";
    signature { header "X-Hub-Signature-256"; prefix "sha256="; encoding hex; }
  }
  dedupe "{header:X-GitHub-Delivery}";
}
```

**Slack** sends a `url_verification` request when the endpoint is set up and
expects its `challenge` value back, so the endpoint uses `sync` with `decide`:

```trellis
webhook slack {
  title "Slack events";
  description "Events API callbacks from the Slack app.";
  mode sync;
  verify hmac {
    key config slackSigningSecret;
    algorithm sha256;
    signed "v0:{timestamp}:{body}";
    signature { header "X-Slack-Signature"; prefix "v0="; encoding hex; }
    timestamp { header "X-Slack-Request-Timestamp"; format unix_seconds; tolerance 5m; }
  }
  decide { deadline 2s; }
}
```

For a `url_verification` body, the handler returns `ignore` with a
`200 text/plain` response that holds the challenge. For any other body it
returns `accept` and sets `dedupeKey` to the body's `event_id`, because Slack
puts that ID in the body.

**PayPal** notifications are confirmed by sending their headers to PayPal's
verification API, which no standard verifier can do. The endpoint declares only
`decide { deadline 30s; }` and stays `async`, so Trellis stores the request,
answers 2xx, and calls the handler until the service answers. A sender that
sends only an object ID works the same way: the handler fetches the object from
the sender's API and accepts the request if it exists.

## The delivery event

The built-in `trellis.webhooks@v1` API declares the event that carries an
accepted request:

```trellis
model WebhookField { name: string; value: string; }

enum WebhookCheckKind { "hmac"; "token"; "basic"; "address"; "none"; "decide"; }

model WebhookCheck {
  kind: WebhookCheckKind;
  /// "current" or "previous" for a check that uses a secret.
  key: string | null;
}

model WebhookDelivery {
  deliveryId: ulid;
  service: string;
  endpoint: string;
  receivedAt: timestamp;
  clientAddress: string;
  method: string;
  url: string;
  headers: list<WebhookField>;
  contentType: string | null;
  body: bytes;
  bodySha256: string;
  checks: list<WebhookCheck>;
  dedupeKey: string | null;
}

api webhooks@v1 {
  event Received { payload WebhookDelivery; params [service, endpoint]; }
}
```

A `Received` event is an ordinary Trellis event. It is stored, retained,
replayed, and dead-lettered by the same rules as every other event.

- **Body.** `body` holds the bytes exactly as received, or after decoding
  `content_encoding`. The SDK offers text, JSON, and form views of them.
- **Headers.** The event carries all request headers in order, with lowercase
  names. Trellis removes hop-by-hop headers, `Cookie`, `Proxy-Authorization`,
  any header that carried a `token` or `basic` secret, and the forwarding
  headers that `clientAddress` replaces. Signature headers stay, since they are not secret.
  A request may have at most 100 headers and 32 KiB of header data.
- **Batches.** One HTTP request is one event. When a sender batches many
  changes into one request, the service splits them.
- **Who receives it.** Each endpoint comes with its own durable consumer, a
  JetStream subscription that remembers what it has delivered. No capability
  lets another service subscribe; other services receive the owner's domain
  events instead.
- **Order.** Delivery is at least once, with no ordering guarantee.

Trellis publishes the event under its own identity, because the sender has no
Trellis identity. What the event says about the sender is only what `checks`
records.

## Duplicates

Senders retry, so a request can arrive twice. An attacker who captures a request
can also send it again later. Two mechanisms handle this.

For senders that sign a timestamp, the `hmac` tolerance rejects a request whose
timestamp is too old.

`dedupe` names where the sender's delivery ID is, usually a header. After a
request passes verification, Trellis keeps its ID for the dedupe window, 72
hours by default. A later verified request with the same ID is recorded as
`duplicate`, gets the original's answer, and publishes nothing. Trellis keeps
an ID only after verification. Otherwise anyone who learned a delivery ID could
send a forged request first and make Trellis drop the real one.

An endpoint without `dedupe` gets no deduplication. Trellis does not compare
bodies instead, because two identical pings are both legitimate. The event's
`deliveryId` doubles as its JetStream message ID, so a publish retried after a
lost confirmation is stored once.

## Runtime settings

Webhooks belong to a service deployment, so their runtime settings belong to
that deployment too. The service's IDL gives a default for some of them, and
the deployment's own value overrides the IDL at runtime, without a new build or
an approval. Trellis keeps the values in the Auth database. They are edited in
the Webhooks section of the service's Console page, or with `trellis svc <name>
webhooks <endpoint> set <setting> <value>`. None of them is server
configuration, and there is no installation-wide layer of defaults.

| Setting                                                               | Default                      | IDL member         |
| --------------------------------------------------------------------- | ---------------------------- | ------------------ |
| Requests per endpoint                                                 | 50 per second, bursts of 200 |                    |
| Failed requests per client address, across the deployment's endpoints | 20 per minute                |                    |
| Body size limit                                                       | 256 KiB                      | `max_body`         |
| Dedupe window                                                         | 72 hours                     | `dedupe_window`    |
| `sync` decide deadline                                                | 2 s, at most 10 s            | `decide`           |
| `async` decide deadline                                               | 30 s, at most 60 s           | `decide`           |
| Secret change overlap                                                 | 24 hours                     | `rotation_overlap` |
| Request record retention                                              | 30 days                      |                    |
| Intake retention                                                      | 7 days or 1 GiB              |                    |

A value set on the deployment wins over the IDL member, and the IDL member wins
over the Trellis default in the table. Two limits apply:

- The body travels inside the event, so it must fit in one NATS message. This
  is the one installation-wide ceiling, and it exists because no deployment
  setting can make NATS carry a larger message. With the default 1 MiB message
  size, the limit is about 700 KiB after base64 and the event envelope. A larger
  body gets 413, whatever the deployment's body size limit says.
- The intake retention must be longer than the longest `async` decide retry, or
  requests would expire while the service is merely slow to return.

Each deployment's raw requests live in its own intake stream, so one
deployment's retention or volume never affects another's. When a deployment
changes its intake retention, Trellis changes that stream's limits in place,
the same way startup already converges stream limits.

## Approval

An endpoint is a service resource, and it is approved like any other resource.
A build that adds an endpoint waits for an administrator. So does a build that
changes anything about an endpoint other than its title or description, since
a longer tolerance, a removed verifier, or a different mode each change what
reaches the service.

Approving takes `trellis.auth.v1:users.manage`, the capability that approves
every service update. The name reads oddly here. In Trellis Auth, services and
devices are principals alongside people, and `users.manage` manages principals
and what they hold: services, devices, resources, sign-in providers, and
sessions. People's accounts have their own capability, `accounts.manage`.

The approval screen shows each endpoint's mode, methods, verifiers in plain
words (such as "HMAC-SHA256 over the timestamp and body, 5 minute tolerance"),
and whether the service decides. An endpoint with `verify none` also needs an
explicit acknowledgement, as a shared API does: `ServiceUpdates.Decide`,
`Deployments.Create`, and `Deployments.Apply` take
`acknowledgeUnauthenticatedWebhooks`, the Console shows a warning and a
checkbox, and the CLI takes `--acknowledge-unauthenticated-webhooks`.

An approved endpoint is enabled. An administrator can disable and enable it at
any time.

## Failures and upgrades

| Situation                        | `async`                                                          | `sync`                                                     |
| -------------------------------- | ---------------------------------------------------------------- | ---------------------------------------------------------- |
| No instance of the service runs  | Accepted. Events wait, and decide calls retry.                   | Accepted with standard verifiers only. With `decide`, 503. |
| The event handler fails          | Event retries, then a dead letter                                | The same                                                   |
| The deployment is disabled       | 503, and nothing is stored                                       | 503, and nothing is stored                                 |
| The endpoint is disabled         | 503                                                              | 503                                                        |
| Trellis is restarting            | Senders retry. A request already answered 2xx is already stored. | The same                                                   |
| JetStream is unavailable or full | 503                                                              | 503                                                        |

Intake uses the endpoint as the service's installed build declares it, so a
pending update changes nothing until it is approved. During a rolling upgrade,
old and new builds both receive `Received` events, because the event type
belongs to Trellis. Decide calls go only to instances whose build declares the
endpoint.

A build that drops an endpoint detaches it once installed. Its URL answers 404,
and events already recorded stay with its consumer. Declaring it again and
approving that brings back the same URL and the waiting events. Random IDs, the
intake stream, request records, and dedupe IDs all survive Trellis upgrades.

## Watching and recovering

Trellis keeps a record of every request to a known endpoint. The API and the
CLI call these records attempts. Each one has an outcome: `accepted`,
`ignored`, `duplicate`, `pending`, `rejected`, `limited`, `unavailable`,
`invalid_decision`, or `expired`. A record also holds the receive time, client
address, body size and hash, reason, matched key, decide verdict, response
status, and the event's `deliveryId`. Requests to unknown URLs are only counted
in metrics, so they cannot fill storage.

An administrator can verify a stored request again. Trellis runs the current
verifiers over the original bytes and receive time, so a request rejected
because of a wrong secret is recovered once the secret is fixed. Once the
request has left the intake stream, its record can still be read but not
verified again.

The `trellis.webhooks@v1` API has three capabilities, shaped like the Events
API's:

| Capability                              | Allows                                             |
| --------------------------------------- | -------------------------------------------------- |
| `trellis.webhooks.v1:attempts.query`    | List endpoints and request records, without bodies |
| `trellis.webhooks.v1:attempts.inspect`  | See request bodies and header values               |
| `trellis.webhooks.v1:attempts.reverify` | Verify a stored request again                      |

Bodies have their own capability because they often hold personal or payment
data that someone watching delivery health does not need. Enabling or disabling
an endpoint, rotating its URL, and changing its runtime settings take
`users.manage`, like other changes to a service's resources.

The Console's service page gets a Webhooks section with each endpoint's URL and
a copy button, its verifiers in plain words, whether its secrets are set, when
an old secret last matched, its runtime settings, and recent requests by
outcome. The CLI has the same views:

```text
trellis svc <name> webhooks [<endpoint>]
trellis svc <name> webhooks <endpoint> attempts [--outcome <outcome>] [--since <duration>]
trellis svc <name> webhooks <endpoint> attempt <attempt-id> [--body]
trellis svc <name> webhooks <endpoint> reverify <attempt-id>
trellis svc <name> webhooks <endpoint> enable|disable|rotate-url|retire-url
trellis svc <name> webhooks <endpoint> set <setting> <value>
```

Logs get one line per request with its ID, service, endpoint, outcome, status,
size, duration, and client address. Logs, records, and error responses never
contain header values, bodies, secrets, or a computed signature. A rejection
reason names only the failed check, such as "timestamp 412 s outside
tolerance". The metrics are `trellis.webhooks.requests` (by endpoint and
outcome), `trellis.webhooks.request.duration`,
`trellis.webhooks.decide.duration`, and a `trellis.webhooks.pending` gauge.

## Retention

Following [data-retention.md](./data-retention.md), each new kind of data has a
policy:

| Data              | Where                               | Default                           | Needed for                                     |
| ----------------- | ----------------------------------- | --------------------------------- | ---------------------------------------------- |
| Raw requests      | the deployment's intake stream      | the deployment's intake retention | `async` verification, retries, verifying again |
| Request records   | `webhooks.sqlite`                   | 30 days after receipt             | Console and CLI request views                  |
| Dedupe IDs        | KV bucket `trellis_webhook_dedupe`  | the endpoint's dedupe window      | Dropping duplicates                            |
| `Received` events | the event stream and Events history | the normal event policies         | Delivery and replay                            |

When a deployment's intake stream is full, Trellis refuses new requests with 503 rather
than remove old ones, so a flood cannot push out requests still waiting for
verification.

## Tests

Tests send real HTTP requests to the URL of an endpoint on a service the
testkit started, with secrets set through the normal settings RPCs. The
testkits expose each endpoint's URL and add no shortcut around intake or
verification.

## Sender facts to check

These senders shaped the design. Facts marked _unverified_ come from memory and
must be checked against the vendor's current documentation before
implementation.

| Sender            | How it signs                                                                              | What it expects back                                         |
| ----------------- | ----------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Stripe            | `Stripe-Signature: t=<unix>,v1=<hex>`, HMAC-SHA256 over `<t>.<body>`                      | Any 2xx. Retries for up to 3 days (_unverified_).            |
| GitHub            | `X-Hub-Signature-256: sha256=<hex>`, HMAC-SHA256 over the body                            | Any 2xx within 10 s. No automatic redelivery (_unverified_). |
| Slack             | `X-Slack-Signature: v0=<hex>` over `v0:<timestamp>:<body>`                                | 2xx within 3 s (_unverified_), and the `challenge` on setup  |
| Twilio            | `X-Twilio-Signature`, base64 HMAC-SHA1 over the URL and sorted form fields (_unverified_) | A TwiML document                                             |
| Meta (WhatsApp)   | `X-Hub-Signature-256` on POST, and a GET handshake with `hub.challenge`                   | The `hub.challenge` value as the GET response                |
| Microsoft Graph   | A `clientState` value in the body, and a setup POST with `validationToken`                | The `validationToken` as `text/plain`                        |
| PayPal            | Headers confirmed by calling PayPal's verification API (_unverified_)                     | Any 2xx                                                      |
| GitLab and others | A fixed token in a header or the query string                                             | Any 2xx                                                      |
| Partner systems   | Basic authentication or a source address range                                            | Sometimes a SOAP acknowledgement                             |

## Not in this version

- **Mutual TLS** is left out because few senders use client certificates, and
  supporting them behind a proxy needs a trusted forwarded-certificate header.
- **Bodies larger than one event** are refused with 413, because keeping them
  elsewhere needs a second store with its own retention.
- **Endpoints created at runtime**, such as one URL per customer, are left out
  because one endpoint that routes by a query parameter and verifies with
  `decide` covers that until a real service needs more.

## Decisions

These questions came up during review and are settled.

1. **A disabled deployment's endpoints stop accepting requests.** They answer
   503 in both modes, and Trellis stores nothing from the request. Webhooks
   are part of the deployment, so disabling the deployment turns them off with
   everything else it owns.
2. **The mode is set per endpoint, not per HTTP method.** One mode per endpoint
   keeps the declaration and the approval screen simple. A Meta endpoint uses
   `sync` with `decide`, answers the GET handshake from its handler, and checks
   the POST signature with the SDK's HMAC helper.
3. **Intake retention is a runtime setting.** It is set per deployment like the
   other [runtime settings](#runtime-settings), and Trellis changes the
   deployment's intake stream in place when it changes.
4. **Request records use new `trellis.webhooks.v1` capabilities.** Records
   include rejected and pending requests that never became events, which the
   Events capabilities do not describe. An installation that wants one operator
   role adds both sets to it.
