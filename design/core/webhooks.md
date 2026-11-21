---
title: Receiving Webhooks
description: Proposal for HTTP webhook endpoints that services declare in IDL, with Trellis-owned intake, verification, durability, and delivery as events.
order: 47
---

# Design: Receiving Webhooks

> **Status: proposal awaiting review.** Nothing in this document is
> implemented. The [open questions](#open-questions) at the end need a decision
> before work starts.

Services that integrate with Stripe, GitHub, Slack, or PayPal have to receive
HTTP callbacks from them. Today each service runs its own HTTP listener for
that, outside Trellis: it terminates TLS, checks signatures with hand-written
code, keeps a secret in an environment variable, and loses the delivery if it
crashes before writing it down. This proposal moves the transport into Trellis.
A service declares an endpoint in its IDL, Trellis serves it at a stable URL,
and accepted deliveries reach the service as durable events.

Sending webhooks to other systems is out of scope.

## Prerequisites

- [trellis-patterns.md](./trellis-patterns.md): platform boundary, events,
  runtime subject spaces
- [config-patterns.md](./config-patterns.md): settings and sealed secrets
- [../contracts/trellis-idl.md](../contracts/trellis-idl.md): resource
  declarations and `config` members
- [../auth/service-updates.md](../auth/service-updates.md): how new resources
  wait for approval
- [data-retention.md](./data-retention.md): retention policy rules

## Principles

- **Trellis adapts to senders.** Senders will not change for Trellis. Each one
  has its own signature scheme, handshake, and response expectations, and the
  declaration has to describe them. A sender no built-in verifier covers is
  handled by the service through the [decide RPC](#the-decide-rpc).
- **Trellis owns transport, durability, and endpoint identity.** It serves the
  endpoint, keeps the request bytes exactly as received, enforces size, rate,
  and replay limits, records every attempt, and turns accepted deliveries into
  Trellis events. Retry, dead letters, and replay then come from the Events
  subsystem rather than a second mechanism.
- **The service owns meaning.** Trellis never parses a payload into a domain
  type. The service reads the bytes, decides what they mean, and publishes its
  own domain events if other services need them.
- **An endpoint is a resource.** It is declared, approved, and detached like
  KV, Store, Job, and Consumer resources, and a new or changed endpoint waits
  for an administrator.

## What senders do

The design has to cover at least these:

| Sender            | Authentication                                                                                                    | Expects back                                                                   |
| ----------------- | ----------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Stripe            | `Stripe-Signature: t=<unix>,v1=<hex>`; HMAC-SHA256 over `<t>.<body>`; several `v1` during a secret roll           | Any 2xx; retries for up to 3 days                                              |
| GitHub            | `X-Hub-Signature-256: sha256=<hex>`; HMAC-SHA256 over the body; no timestamp; `X-GitHub-Delivery` ID              | Any 2xx within 10 s; no automatic redelivery                                   |
| Slack Events API  | `X-Slack-Signature: v0=<hex>` over `v0:<timestamp>:<body>`; `X-Slack-Request-Timestamp`                           | 2xx within 3 s; a `url_verification` request must echo `challenge` in the body |
| Twilio            | `X-Twilio-Signature`; base64 HMAC-SHA1 over the full URL plus the form parameters sorted by name                  | A TwiML document in the response body                                          |
| Meta (WhatsApp)   | `X-Hub-Signature-256` on POST; a `GET` with `hub.mode`, `hub.verify_token`, `hub.challenge` when subscribing      | The `hub.challenge` value as the GET response body                             |
| Microsoft Graph   | A `clientState` value inside the JSON body; first a POST with `?validationToken=`                                 | The decoded `validationToken` as `text/plain`                                  |
| PayPal            | Transmission headers verified by calling PayPal's `verify-webhook-signature` API                                  | Any 2xx                                                                        |
| GitLab, many SaaS | A static token in a header (`X-Gitlab-Token`) or the query string                                                 | Any 2xx                                                                        |
| Partner systems   | Basic auth, mutual TLS, or only a source address range; bodies in JSON, form encoding, XML or SOAP, often batched | Sometimes a SOAP acknowledgement                                               |

Some senders compress bodies, some batch many events into one request, and some
send nothing that can be verified, so the receiver confirms by fetching the
object by ID from the sender's API.

## Model

A `webhook` resource on a service declares one endpoint. For each one Trellis
runs the same pipeline:

1. **Intake.** The Trellis listener receives the request at the endpoint's
   URL. It checks method, size, encoding, rate, and source address before
   reading further.
2. **Verification.** Built-in verifiers declared on the endpoint run against
   the exact bytes, using secrets from the service's settings. An endpoint may
   also hand the request to the service through the decide RPC.
3. **Recording.** Every attempt is recorded with its outcome. An accepted
   delivery becomes a `trellis.webhooks@v1` `Received` event on the event
   stream.
4. **Delivery.** The endpoint's own durable consumer delivers that event to
   the service's instances, with the declared concurrency and retry. Exhausted
   deliveries become dead letters, replayable like any other.

The **mode** decides where the answer to the sender falls in that pipeline:

- `async` stores the raw request durably, answers 2xx at once, and verifies
  afterwards. The sender gets 2xx even for a request that later fails
  verification. Nothing is lost while the service is down, and a delivery
  rejected because a secret was wrong can be verified again once it is fixed.
- `sync` verifies first, calls the decide RPC when declared, records the
  delivery, and only then answers. Handshakes and senders that read the
  response body need it. A failed verification gets a 401.

## Declaration

`webhook` joins `state`, `kv`, `store`, `job`, and `consumer` as a resource
header. Only services may declare it. Like other resources it may be
`optional` and needs a `title` and `description`.

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

Members of a `webhook` body:

| Member                                | Meaning                                                                                                            | Default                  |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------ | ------------------------ |
| `mode async \| sync`                  | When the sender gets its answer; see [Delivery modes](#delivery-modes)                                             | `async`                  |
| `methods [POST, GET]`                 | Accepted HTTP methods; anything else gets 405                                                                      | `[POST]`                 |
| `verify <kind> { ... }`               | A built-in verifier. Several `verify` members must all pass. `verify none;` states there is none                   | required unless `decide` |
| `decide { deadline 2s; }`             | Call the service's decide RPC with the raw request                                                                 | absent                   |
| `status 200;`                         | Status for an accepted request when no decide response sets one                                                    | `200`                    |
| `max_body 256KiB;`                    | Largest body accepted, after decoding                                                                              | `256KiB`                 |
| `body_store <store>;`                 | A `store` resource of the same service that holds bodies too large to travel inside the event                      | absent                   |
| `content_encoding [gzip];`            | Encodings Trellis decodes before verifying; any other `Content-Encoding` gets 415                                  | `[]` (identity only)     |
| `dedupe "<template>" { window 72h; }` | Sender-provided delivery ID used to drop duplicates; see [Replay protection](#replay-protection-and-deduplication) | absent                   |
| `concurrency N;`                      | Event deliveries to the service in flight at once                                                                  | `1`                      |
| `retry { ... }`                       | Event delivery retry, the same block a `consumer` takes                                                            | the consumer default     |

Compile-time rules:

- A `config` reference must name a field of the same service. A key, token, or
  password must be a `secret` field of type `string` or `string | null`. Source
  ranges, certificate fingerprints, and usernames may be plain settings.
- An endpoint with neither `verify` nor `decide` is an error. Unverified
  endpoints say `verify none;`, which cannot be combined with another verifier.
- `max_body` above what fits in one event (about 700 KiB with the default
  1 MiB broker payload, after base64 and the envelope) requires `body_store`.
  The hard limit is 64 MiB.
- `decide` deadlines are at most 10 s in `sync` mode and 60 s in `async` mode.
- Templates are checked: an unknown placeholder or a `{timestamp}` without a
  `timestamp` block is an error.
- Every member except `title` and `description` is part of the participant
  digest, so changing one is a new build and goes through
  [service updates](../auth/service-updates.md).

The generated SDKs give each declared endpoint a handle on the service, with
one registration for delivered events and, when `decide` is declared, one for
the decide RPC. Exact signatures belong in the language docs.

### Example: GitHub

GitHub signs only the body and sends no timestamp, so its delivery ID is the
replay guard. GitHub does not redeliver on its own, which makes `async` the
safer mode: a delivery rejected because the secret was wrong can be
[verified again](#observability) after the fix.

```trellis
service Repos {
  config githubWebhookSecret {
    title "GitHub webhook secret";
    description "The secret entered in the repository's webhook settings.";
    type string;
    secret;
  }

  webhook github {
    title "GitHub";
    description "Push, pull request, and issue events from GitHub.";
    mode async;
    verify hmac {
      key config githubWebhookSecret;
      algorithm sha256;
      signed "{body}";
      signature { header "X-Hub-Signature-256"; prefix "sha256="; encoding hex; }
    }
    dedupe "{header:X-GitHub-Delivery}" { window 72h; }
    concurrency 4;
  }
}
```

### Example: Slack, with its handshake

Slack's `url_verification` request must get the `challenge` value back within
3 seconds, so the endpoint is `sync` and the service answers through `decide`.
Trellis checks the signature first; the service only ever sees signed requests.

```trellis
service Chat {
  config slackSigningSecret {
    title "Slack signing secret";
    description "Signing secret from the Slack app's Basic Information page.";
    type string;
    secret;
  }

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
}
```

The service's decide handler returns `ignore` with a `200 text/plain` response
holding the challenge for a `url_verification` body. For any other body it
returns `accept` with no response, which answers the declared status, and sets
`dedupeKey` to the body's `event_id`, since Slack retries carry the same ID in
the body rather than a header.

### Example: a sender confirmed by calling back

PayPal's notifications are confirmed by sending their transmission headers to
PayPal's `verify-webhook-signature` API. No built-in verifier can do that, so
the endpoint has only `decide`. It stays `async`: Trellis stores the request
and answers 2xx, then calls the decide RPC until the service answers.

```trellis
service Payments {
  config paypalWebhookId {
    title "PayPal webhook ID";
    description "ID PayPal assigned to this webhook, sent with each verification call.";
    type string;
  }
  config paypalClientSecret {
    title "PayPal client secret";
    description "Secret for the REST app that calls PayPal's verification API.";
    type string;
    secret;
  }

  webhook paypal {
    title "PayPal";
    description "Payment notifications, confirmed with PayPal before they become events.";
    mode async;
    decide { deadline 30s; }
  }
}
```

A sender that only sends an object ID works the same way: the decide handler
fetches the object from the sender's API and accepts the delivery if it
exists. A service may also declare `verify none;` without `decide` and do that
check in its event handler, at the cost of an
[acknowledgement at approval](#who-can-enable-an-endpoint).

## URLs and routing

Each endpoint has one URL per deployment:

```text
https://<origin>/webhooks/<webhookId>
```

`<webhookId>` is 128 random bits in unpadded base64url (22 characters), assigned
when the resource is first allocated and stored with its resource identity.
That identity is the deployment, the participant, the kind, and the local
name, so the URL stays the same across builds, service updates, Trellis
upgrades, disabling and enabling the deployment, and detaching and reattaching
the endpoint. It changes only when an administrator destroys the resource or
rotates the URL. Two deployments of the same build get different URLs. All
instances of one deployment share its URL.

The ID is opaque on purpose. A readable path such as
`/webhooks/billing/stripe` would reveal service names, change on a rename, and
let anyone enumerate endpoints. An unguessable URL is not authentication, but
it keeps scanners off endpoints that rely on `verify none`.

Rotation: `trellis svc <id> webhooks <name> rotate-url` assigns a new ID. The
old one keeps answering until `retire-url`, so the sender can be switched over
first.

Routing on the listener:

- `/webhooks/` is a reserved prefix on the Trellis origin (`[http]
public_origin`). The path must match exactly; the query string passes
  through. Unknown IDs get 404.
- The Console origin (`[console] origin`) does not serve webhook paths, in the
  same way sign-in pages refuse the Console host.
- An optional `[webhooks] origin` names a hostname of its own that the same
  listener answers. That host serves only `/webhooks/*`, `/healthz`, and
  `/readyz`, and the Trellis origin then answers 404 for webhook paths. This
  keeps sender traffic and decide responses off the origin that serves sign-in.
  See [open question 2](#open-questions).
- The `Host` check matches the configured origin, as it does for the OAuth
  routes. The URL that `{url}` stands for in a signature template is built from
  that configured origin and the raw request target, never from forwarding
  headers.
- Behind a `proxy` transport the client address is the one the listener
  already derives from `trusted_proxies`. The proxy must keep `Host` and must
  not rewrite the path or query, or URL-signing senders such as Twilio fail.
- Webhook routes never send CORS headers and have their own request budget,
  separate from the sign-in budget, so a flood of webhooks cannot lock people
  out of sign-in.
- With generated TLS (`mode = "generated"`), senders on the internet will not
  trust the certificate. The Console marks webhook URLs with a warning in that
  case.

The routes are served wherever the platform role runs, since intake needs
Auth's sealed secrets and the deployment's declarations. A split `webhooks`
role is not proposed.

## Verifiers

Trellis first applies the checks that need no secret: method, `Content-Length`
against `max_body`, `Content-Encoding`, rate limits, and `verify address`.
A failure there is answered at once in both modes with the status in
[Responses](#responses), and nothing about the body is stored. The remaining
verifiers run in declaration order against the stored bytes. All must pass.
Every comparison of a secret or signature is constant-time.

### `hmac`

```trellis
verify hmac {
  key config <secret field>;
  key_prefix "whsec_";            // optional; stripped before decoding
  key_encoding text;              // text | hex | base64 | base64url
  algorithm sha256;               // sha1 | sha256 | sha512
  signed "<template>";
  signature { header "<name>"; separator "<sep>"; prefix "<prefix>"; encoding hex; }
  timestamp { header "<name>"; separator "<sep>"; prefix "<prefix>"; format unix_seconds; tolerance 5m; }
}
```

- **Key.** The secret's value with `key_prefix` removed, decoded by
  `key_encoding`. `text` uses the UTF-8 bytes unchanged, which is what Stripe,
  GitHub, and Slack expect. Standard Webhooks senders use `key_prefix "whsec_";
key_encoding base64;`.
- **Signature extraction.** Trellis reads every value of the named header. With
  `separator`, each value is split on it and each item trimmed of spaces.
  Items that start with `prefix` carry a signature, which is the rest of the
  item; items without it are ignored. Without `separator`, the whole value is
  one item. Up to 8 signatures are tried. `encoding` is `hex`
  (case-insensitive), `base64`, or `base64url`.
- **Signed content.** `signed` is a template whose result is the exact bytes
  the HMAC covers. Placeholders:

  | Placeholder         | Bytes                                                                                                 |
  | ------------------- | ----------------------------------------------------------------------------------------------------- |
  | `{body}`            | The body as received, or decoded per `content_encoding`                                               |
  | `{timestamp}`       | The timestamp text exactly as extracted, not reformatted                                              |
  | `{header:Name}`     | The first value of that header as received; a missing header fails verification                       |
  | `{method}`          | The request method                                                                                    |
  | `{url}`             | Configured origin, path, and raw query, as the sender addressed it                                    |
  | `{path}`, `{query}` | The raw path; the raw query without `?`                                                               |
  | `{form_sorted}`     | The body parsed as `application/x-www-form-urlencoded`, sorted by name, each name then value, decoded |

  `{{` and `}}` are literal braces. Twilio is `algorithm sha1; signed
"{url}{form_sorted}"; signature { header "X-Twilio-Signature"; encoding
base64; }`. Standard Webhooks is `signed
"{header:webhook-id}.{header:webhook-timestamp}.{body}"` with `separator " ";
prefix "v1,"`.

- **Timestamp.** Extracted the same way as a signature. `format` is
  `unix_seconds`, `unix_millis`, or `rfc3339`. The request fails if the
  timestamp is more than `tolerance` away from the time Trellis received it.
  The comparison uses the receive time even when verification runs later, so an
  `async` delivery verified a day later, after a secret was fixed, still
  passes.
- **Match.** The request passes when any extracted signature equals the HMAC
  under any [accepted key](#secrets-and-rotation). The attempt records which
  key matched.

### `token`

```trellis
verify token { secret config gitlabToken; header "X-Gitlab-Token"; }
verify token { secret config hookToken; query "token"; }
verify token { secret config apiKey; header "Authorization"; prefix "Bearer "; }
```

The header value, after `prefix`, or the query parameter must equal the secret.
Trellis removes the header or query parameter from everything it records and
delivers, since it is the secret itself.

### `basic`

```trellis
verify basic { username config partnerUser; password config partnerPassword; }
```

HTTP Basic credentials. The `Authorization` header is removed from records
and deliveries.

### `mtls`

```trellis
verify mtls { fingerprints config partnerCertificates; }
verify mtls { ca config partnerCa; subject_alt_name config partnerHost; }
```

The client certificate must have one of the listed SHA-256 fingerprints, or
chain to the CA given in PEM and name the given SAN. This follows how Trellis
already accepts certificates by fingerprint. It needs the `tls` transport, which
then asks webhook connections for an optional client certificate. Behind a
proxy it needs the proxy to forward the certificate in a header named by
server configuration, trusted only from `trusted_proxies`. See
[open question 9](#open-questions). The certificate's fingerprint, subject, and
SANs are part of the delivery.

### `address`

```trellis
verify address { ranges config stripeAddresses; }
```

`ranges` is a plain `list<string>` setting of CIDR ranges, so an administrator
can follow the sender's published list without a new build. The client
address is the one the listener derives, which behind a proxy depends on
`trusted_proxies` being right. Address checks are weak alone; combine them with
another verifier where the sender allows.

### `none`

No verification. The approval screen and the Console mark the endpoint
unauthenticated, and approving it needs an explicit acknowledgement.

### `decide`

The service is the verifier. See [The decide RPC](#the-decide-rpc).

## Secrets and rotation

Verifiers read the deployment's effective settings, the same values
`Config.Own.Get` returns to its instances. Instance overrides are ignored: the
endpoint belongs to the deployment, not to one instance. The intake runs in
the platform role, which already holds the storage key that seals service
secrets. It decrypts a verifier's secrets in memory, keeps them per settings
revision, rereads on `Config.Changed`, and never writes them anywhere. This is
a new reader of service secrets and should be named as such in the security
review.

Rotation needs an overlap, since the sender and Trellis cannot switch at the
same instant and Stripe signs with both keys during its own roll. When a
secret a verifier uses changes, the value from the previous revision stays
accepted for 24 hours from the change. An administrator ends the overlap early
by deleting that earlier revision (`trellis svc <id> config delete-revision`),
which destroys the old secret as it already does today. Each attempt records
whether the current or the previous key matched, and the Console shows when
the previous key was last used, so the administrator can tell when the sender
has switched. See [open question 7](#open-questions).

A required secret that is not set holds the service's instances as any missing
setting does. For the endpoint, an `async` request is stored and waits in
`pending` until the secret is set, then verified; a `sync` request gets 503.

## Delivery modes

### `async`

1. Checks that need no secret run.
2. Trellis appends the raw request (method, URL, headers, body, client
   address, certificate, receive time) to the intake stream
   `TRELLIS_WEBHOOK_INTAKE` and waits for the JetStream acknowledgement.
3. It answers the declared status. A failed append answers 503.
4. A verification worker reads the intake stream, runs the built-in verifiers,
   then the decide RPC if declared, and applies deduplication.
5. An accepted delivery is published as a `Received` event. The attempt row
   records the outcome either way.

### `sync`

1. Checks that need no secret run.
2. Built-in verifiers run. A failure answers 401.
3. The decide RPC runs if declared, within its deadline.
4. Deduplication runs on the verified delivery.
5. An accepted delivery is published as a `Received` event, and the answer is
   sent only after the broker acknowledges it. A failed publish answers 503,
   even when the service already accepted, so the sender retries and nothing
   is acknowledged that is not stored.

Rejected `sync` requests also go to the intake stream, so an administrator can
see what failed and verify it again.

### Responses

| Situation                                                  | Status                                                   |
| ---------------------------------------------------------- | -------------------------------------------------------- |
| Unknown or retired ID, detached endpoint                   | 404                                                      |
| Endpoint disabled by an administrator                      | 503                                                      |
| Method not declared                                        | 405 with `Allow`                                         |
| Body larger than `max_body`                                | 413                                                      |
| `Content-Encoding` not declared                            | 415                                                      |
| Rate limit                                                 | 429 with `Retry-After`                                   |
| `verify address` fails                                     | 403                                                      |
| `sync`: verifier fails, or decide rejects with no response | 401                                                      |
| `sync`: no instance answers decide in time, or it errors   | 503                                                      |
| Storage unavailable or full                                | 503                                                      |
| Accepted, ignored, or duplicate                            | The decide response, else the declared `status`          |
| `async`, after storing                                     | The declared `status`, whatever verification later finds |

Every error response has an empty body, so it gives a prober nothing.

## The decide RPC

Trellis calls the service over a runtime subject owned by the endpoint's
binding, in the way jobs and live sessions have runtime subject spaces:

```text
webhooks.v1.decide.<b64(deploymentId)>.<b64(endpoint)>
```

The request is signed by the platform provider like any Trellis RPC call, and
the SDK checks that before the handler runs. Only instances whose build
declares the endpoint subscribe, in one queue group per deployment. Request
and response are Trellis-owned types in the `trellis.webhooks@v1` API:

```trellis
enum WebhookMode { "async"; "sync"; }
enum WebhookVerdict { "accept"; "ignore"; "reject"; }

model WebhookDecideRequest {
  delivery: WebhookDelivery;
  mode: WebhookMode;
  /// 1 for the first call; async decides are retried.
  attempt: uint32;
  deadline: timestamp;
}

model WebhookResponse {
  status: uint32;
  headers: list<WebhookField>;
  body: bytes;
}

model WebhookDecision {
  verdict: WebhookVerdict;
  /// Sent to the sender in sync mode; ignored in async mode.
  response?: WebhookResponse;
  /// Recorded on the attempt, never sent to the sender.
  reason?: string;
  /// Replaces the declared dedupe key, for senders whose ID is in the body.
  dedupeKey?: string;
}
```

- `accept`: the request is genuine and becomes a `Received` event.
- `ignore`: the request is genuine but is not an event. Handshakes, pings, and
  requests the handler fully processed inside the call return this, so the
  service does not see them twice.
- `reject`: verification failed. The attempt is recorded as rejected.

An unknown verdict, since IDL enums are open, counts as `reject`. A handler
error or no answer within the deadline counts as unavailable: `sync` answers
503, and `async` calls again with backoff (5 s, 30 s, 2 m, 10 m, then every
10 m) until the intake stream's retention runs out, when the attempt becomes
`expired`.

A `sync` handler may do the work itself and return a real answer, such as
TwiML for Twilio or a SOAP acknowledgement. It should then return `ignore`, or
`accept` and make the event handler idempotent.

Trellis checks the response before sending it, since it is served on a Trellis
origin:

- `status` between 200 and 599.
- At most 16 headers. `Set-Cookie`, hop-by-hop headers, and the security
  headers below are refused.
- `Content-Type` one of `text/plain`, `application/json`, `application/xml`,
  `text/xml`, `application/soap+xml`, or
  `application/x-www-form-urlencoded`, with an optional charset.
- `body` at most 64 KiB.
- Trellis adds `Content-Security-Policy: sandbox; default-src 'none'`,
  `X-Content-Type-Options: nosniff`, and `Cache-Control: no-store`.

A response that breaks a rule is replaced by 502 and the attempt records
`invalid_decision`. Without these rules a service could serve a script on the
origin that hosts sign-in.

## The delivery event

The built-in `trellis.webhooks@v1` API declares:

```trellis
model WebhookField { name: string; value: string; }

model WebhookCertificate {
  sha256: string;
  subject: string;
  subjectAltNames: list<string>;
}

enum WebhookCheckKind { "hmac"; "token"; "basic"; "mtls"; "address"; "none"; "decide"; }

model WebhookCheck {
  kind: WebhookCheckKind;
  /// "current" or "previous" for a keyed check.
  key: string | null;
}

model WebhookDelivery {
  deliveryId: ulid;
  deploymentId: string;
  endpoint: string;
  receivedAt: timestamp;
  clientAddress: string;
  clientCertificate?: WebhookCertificate;
  method: string;
  url: string;
  headers: list<WebhookField>;
  contentType: string | null;
  body?: bytes;
  bodyObject?: string;
  bodySize: uint64;
  bodySha256: string;
  checks: list<WebhookCheck>;
  dedupeKey: string | null;
}

api webhooks@v1 {
  event Received { payload WebhookDelivery; params [deploymentId, endpoint]; }
}
```

- **Bytes.** `body` holds the bytes exactly as received, or decoded per
  `content_encoding`, and `bodySha256` is over the same bytes. Trellis never
  re-encodes, normalizes line endings, or parses JSON, form, or XML bodies.
  The SDK handle offers text, JSON, and form views over those bytes.
- **Headers.** All request headers in received order, names lowercased,
  repeated headers as separate entries. Removed: hop-by-hop headers,
  `Cookie`, `Proxy-Authorization`, `Authorization` when `basic` or a `token`
  uses it, a `token` header, and `X-Forwarded-*`/`Forwarded` (the derived
  `clientAddress` replaces them). Signature headers stay, since they are not
  secret and a service may want to check them again. Request headers are
  limited to 100 entries and 32 KiB in total.
- **URL.** The URL as addressed, with a `token` query parameter removed.
- **Large bodies.** A body that does not fit the event goes to the declared
  `body_store` under the key `webhooks/<endpoint>/<deliveryId>`, and the event
  carries `bodyObject` instead of `body`. The store's own TTL decides how long
  it lives. A replay after that finds no object, and the SDK reports it as a
  missing body rather than an empty one.
- **Batches.** One HTTP request is one delivery. Splitting a batch into domain
  events is the service's work.
- **Subscription.** No capability selects `Received`. The `webhook` resource
  materializes a durable consumer filtered to its own deployment and endpoint
  tokens, and only that binding grants the subscription. One endpoint belongs
  to one deployment; other services receive the owner's domain events.
- **Order.** Delivery is at least once with no ordering guarantee, as for any
  consumer.

The platform provider publishes the event, so its authorization context is
Trellis's own. The sender has no Trellis identity; what the event says about
the sender is only what `checks` records.

## Replay protection and deduplication

- A timestamp tolerance rejects captured requests replayed later, for senders
  that sign a timestamp.
- `dedupe` names a template, usually a delivery ID header. After verification
  passes, Trellis stores the key in KV `trellis_webhook_dedupe` with the
  endpoint's window as TTL (default 72 h, at most the event stream's 7 days).
  A second verified delivery with the same key is recorded as `duplicate`, gets
  the original's answer, and publishes nothing.
- Keys are claimed only after verification. Claiming them at intake would let
  anyone who learns a delivery ID send a forged request first and have the
  real one dropped.
- Without `dedupe` there is no deduplication. Hashing the body instead would
  drop legitimate repeats such as two identical pings, so it is not a
  default; a decide handler can set `dedupeKey` to whatever the sender
  guarantees.
- The event's `deliveryId` is also the JetStream message ID, so a publish
  retried after a lost acknowledgement does not record twice.

## When something is down

| Situation                                   | `async`                                                                                           | `sync`                                                 |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| Service has no running instance             | Accepted; events wait in the consumer; decide calls retry                                         | Built-in only: accepted, events wait. With decide: 503 |
| Service handler fails on the event          | Consumer retry, then a dead letter                                                                | Same                                                   |
| Secret not set                              | Stored as `pending`, verified once set                                                            | 503                                                    |
| Deployment disabled                         | Accepted and held, bounded by intake retention; nothing is delivered until enabled                | 503                                                    |
| Endpoint disabled by an administrator       | 503                                                                                               | 503                                                    |
| Trellis restarting                          | The listener is down; senders retry. A request answered 2xx was already acknowledged by JetStream | Same                                                   |
| JetStream unavailable or intake stream full | 503                                                                                               | 503                                                    |

**Upgrades.** Intake uses the declaration of the deployment's installed
revision. A pending service update changes nothing until it is approved. During
a rolling upgrade, old and new builds both consume `Received` events, since the
event type is Trellis-owned and the same for both. Decide calls go only to
instances whose build declares the endpoint. A new build that drops an endpoint
detaches it once installed: its URL answers 404, events already recorded stay
in the detached consumer, and declaring it again reattaches the same URL and
backlog after approval.

**Claims.** A service added without its IDL has no declaration, so it has no
endpoint and no URL. Secrets can still be set on the claim beforehand, as for
any setting. The endpoint and its URL exist once the claim is approved with a
build that declares them.

**Trellis upgrades.** Webhook IDs, the intake stream, attempts, and dedupe keys
are persistent state and survive an upgrade unchanged.

## Security

- **No Trellis identity.** Webhook requests never reach NATS admission or
  OAuth. They create no session and carry no capability. A verified delivery
  proves only that the sender knew a secret or held a certificate, so services
  treat its contents as untrusted input.
- **Secrets stay in settings.** Keys, tokens, and passwords are `secret`
  settings, sealed as all service secrets are, set with `set-secret`, and never
  shown. Server configuration holds no per-endpoint secret.
- **No oracle.** Records, logs, and error responses never contain the expected
  signature, the computed HMAC, or any secret. A verification reason names the
  failed check ("signature mismatch", "timestamp 412 s outside tolerance",
  "missing header X-Slack-Signature") and nothing computed.
- **Abuse limits.** Each endpoint has a request rate limit, and each client
  address has a limit on failed requests across all endpoints. Both default to
  values Auth stores and administrators change in the Console or CLI, not
  server configuration. Bodies must arrive within 10 s. In-flight `sync` decide
  calls are capped per endpoint, and requests over the cap get 503. The intake
  stream discards new messages when full rather than old ones, so a flood gets
  503s instead of evicting deliveries still waiting for verification.
- **Decompression.** A declared encoding is decoded with `max_body` applied to
  the decoded size and the decoding stopped as soon as it is passed.
- **Logging.** One structured log line per attempt: attempt ID, deployment,
  endpoint, outcome, status, body size, duration, client address. Never header
  values, query values, bodies, or secrets.
- **Response hardening.** See [The decide RPC](#the-decide-rpc).

### Who can enable an endpoint

A `webhook` declaration is a resource, so a build that adds one waits for an
administrator like any `addedResources` change. Any change to an existing
endpoint other than its title or description is a `changedResources` change
and also waits, since there is no safe direction: a longer tolerance, a removed
verifier, a different mode, or a larger body each change what reaches the
service. Approval takes `trellis.auth.v1:users.manage`, as for other
resources. The approval screen shows each endpoint's mode, methods, verifiers
in plain words ("HMAC-SHA256 over timestamp and body, 5 minute tolerance"),
whether the service decides, and the body limit.

An endpoint with `verify none` needs an explicit acknowledgement on approval,
as a shared API does: `acknowledgeUnauthenticatedWebhooks` on
`ServiceUpdates.Decide`, `Deployments.Create`, and `Deployments.Apply`; a
warning and checkbox in the Console; `--acknowledge-unauthenticated-webhooks`
in the CLI.

An approved endpoint is enabled. Administrators can disable and enable it at
any time without a new build.

## Observability

The built-in `trellis.webhooks@v1` API carries the administration RPCs, under
three administrative capabilities in the default administration group:

| Capability                                | Allows                                                                                 |
| ----------------------------------------- | -------------------------------------------------------------------------------------- |
| `trellis.webhooks.v1:webhooks.read`       | `Webhooks.List`, `Webhooks.Get`, `Attempts.List`, `Attempts.Get` without bodies        |
| `trellis.webhooks.v1:webhooks.readBodies` | Attempt bodies and header values                                                       |
| `trellis.webhooks.v1:webhooks.manage`     | `Webhooks.SetEnabled`, `Webhooks.RotateUrl`, `Webhooks.RetireUrl`, `Attempts.Reverify` |

Bodies get their own capability because they often hold personal or payment
data that an operator watching delivery health does not need.

An attempt is one HTTP request to a known endpoint: its ID, receive time,
client address, method, body size and hash, outcome, reason, the key that
matched, the decide verdict and latency, the response status, total duration,
and the event's delivery ID when one was published. Outcomes are `accepted`,
`ignored`, `duplicate`, `pending`, `rejected`, `limited`, `unavailable`,
`invalid_decision`, and `expired`. Requests to unknown IDs are counted in
metrics but not recorded, so they cannot fill storage.

`Attempts.Reverify` runs the current verifiers again over a stored attempt that
was rejected or pending, using its original bytes and receive time. It is how a
delivery rejected because of a wrong secret is recovered after the fix. A
verified result publishes the event; deduplication still applies.

**Console.** Each service page gets a Webhooks section listing its endpoints:
the URL with a copy button, mode, verifiers in plain words, enabled state, the
secrets each verifier uses and whether they are set, the rotation overlap and
when the previous key was last used, and counts by outcome over the last 24
hours. Each endpoint opens a recent-attempts table filtered by outcome, with an
attempt view showing headers and, with `webhooks.readBodies`, the body. A
rejected attempt offers Verify again; a published one links to the event in
the Events view and to its dead letter if it has one.

**CLI.**

```text
trellis svc <id> webhooks
trellis svc <id> webhooks <name>
trellis svc <id> webhooks <name> attempts [--outcome <outcome>] [--since <duration>]
trellis svc <id> webhooks <name> attempt <attempt-id> [--body]
trellis svc <id> webhooks <name> reverify <attempt-id>
trellis svc <id> webhooks <name> enable|disable
trellis svc <id> webhooks <name> rotate-url|retire-url
```

**Metrics.** `trellis.webhooks.requests` (attributes `trellis.webhook.endpoint`,
`trellis.webhook.outcome`), `trellis.webhooks.request.duration`,
`trellis.webhooks.decide.duration`, and a `trellis.webhooks.pending` gauge.

## Retention

Each new data class gets a policy, following [data-retention.md](./data-retention.md):

| Data class                           | Store                                      | Default policy                                     | Setting                                                | Depends on it                                  |
| ------------------------------------ | ------------------------------------------ | -------------------------------------------------- | ------------------------------------------------------ | ---------------------------------------------- |
| Webhook intake (`webhookIntake`)     | stream `TRELLIS_WEBHOOK_INTAKE`            | 7 days or 1 GiB, discard new                       | `[webhooks] intake_retention_days`, `intake_max_bytes` | `async` verification, decide retries, reverify |
| Webhook attempts (`webhookAttempts`) | `webhooks.sqlite`                          | 30 days after receipt                              | `[webhooks] attempt_retention_days`                    | Console and CLI attempt views                  |
| Dedupe keys                          | KV `trellis_webhook_dedupe`                | per-key TTL, the endpoint's window, at most 7 days | IDL `dedupe ... { window }`                            | Duplicate suppression                          |
| Accepted deliveries                  | event stream, Events history, dead letters | the existing event policies                        | `[events] ...`                                         | Delivery, replay                               |
| Large bodies                         | the service's `body_store`                 | the store's TTL                                    | IDL                                                    | Delivery and replay of large bodies            |

The intake retention cannot be shorter than the longest `async` decide retry,
or deliveries would expire while the service is merely slow to come back. An
attempt row outlives the bytes in the intake stream; once those are gone the
attempt can be read but not verified again.

Accepted deliveries also land in Events history, which keeps full payloads for
90 days. For webhook bodies that may be longer than an installation wants to
keep personal data. See [open question 4](#open-questions).

## Development and tests

Tests send real HTTP requests to the endpoint URL of a service started by the
testkit, with secrets set through the same settings RPCs production uses. The
testkits expose each endpoint's URL; they add no shortcut that skips intake or
verification. Receiving webhooks from a hosted sender on a developer machine
needs a public tunnel, which is the developer's concern.

## Out of scope

- Sending webhooks.
- Parsing or transforming payloads into domain types or domain events.
- One endpoint delivering to several services.
- Endpoints created at runtime, such as one URL per tenant (see
  [open question 10](#open-questions)).
- Ordering guarantees.
- Long-lived inbound connections (Slack Socket Mode, server-sent events) and
  polling integrations.
- Devices and apps declaring endpoints.
- Providing publicly trusted certificates or tunnels.

## Open questions

1. **URL form.** Opaque `/webhooks/<webhookId>` or a readable
   `/webhooks/<service>/<endpoint>`?
   Recommendation: opaque. It does not change on a rename, reveals nothing, and
   cannot be enumerated. The Console's copy button removes the cost of a
   meaningless string.
2. **Separate origin or listener.** Is the optional `[webhooks] origin`
   hostname on the same listener enough, or is a separate bind address needed,
   so that webhooks can face the internet while the Trellis listener stays
   private?
   Recommendation: ship the optional origin first. It mirrors `[console]
origin` and isolates decide responses from sign-in. Add a separate listener
   when an installation needs the network split.
3. **Verify before answering in `async`.** Built-in verifiers are cheap, so
   `async` could verify before the 2xx and answer 401 to forgeries while still
   storing the bytes.
   Recommendation: keep verify-after as agreed. With a wrong secret, GitHub
   gets a 401 and never redelivers, so verify-after is what lets those
   deliveries be recovered. Rate limits and the intake byte cap bound the
   storage cost. Installations that want 401s for forgeries use `sync`.
4. **Bodies in Events history.** Events history keeps full payloads for 90
   days, longer than the 7-day event stream.
   Recommendation: for `Received` events, Events records metadata and
   `bodySha256` but not the body. Delivery and replay use the event stream and
   the dead-letter journal, which keep the bytes for as long as replay needs
   them.
5. **Declarative handshakes.** Slack's challenge, Graph's `validationToken`,
   and Meta's `hub.challenge` each need a few lines in a decide handler. Should
   the IDL have built-in responders, or a per-method mode so Meta's GET can be
   `sync` while its POSTs stay `async`?
   Recommendation: not in the first version. Use `sync` with `decide`, and ship
   SDK helpers for those three handshakes. Revisit if most endpoints end up
   writing the same handler.
6. **Where rate limits live.** IDL, server configuration, or Auth's database?
   Recommendation: Auth's database, edited in the Console and CLI, with
   defaults (50 requests per second per endpoint, burst 200; 20 failed
   requests per minute per client address). Senders' volumes differ by
   installation, and the build cannot know them.
7. **Rotation overlap.** A fixed 24 hours after a secret changes, or a
   per-endpoint `rotation_overlap` in IDL?
   Recommendation: an IDL member with a 24 h default. A sender with a shorter
   or longer roll is a property of the integration, which the IDL already
   describes.
8. **Disabled deployments.** Should an `async` endpoint keep accepting while
   its deployment is disabled?
   Recommendation: yes, within intake retention. Disabling a deployment stops
   it acting, and a sender that gets 503 for days may disable the endpoint on
   its side. An administrator who wants the endpoint closed disables the
   endpoint itself.
9. **mTLS behind a proxy.** Support a forwarded client certificate header?
   Recommendation: yes, but only from `trusted_proxies` and only under a
   header name the operator configures. Without it, `verify mtls` requires the
   `tls` transport, and the approval screen says so.
10. **Per-tenant endpoints.** Multi-tenant services want one URL and one secret
    per customer, created at runtime.
    Recommendation: leave it out of the first version. A service can route by
    a query parameter on one endpoint and verify with `decide` against its own
    tenant records. Design runtime endpoints separately once a real service
    needs them.
11. **Large bodies.** Use the service's own `body_store`, or a Trellis-owned
    object store?
    Recommendation: the service's store. Its TTL and capacity are already
    approved and visible, and the service decides when a body is no longer
    needed. A Trellis-owned store would need a retention rule tied to dead
    letters, which can stay open indefinitely.
12. **Approval capability.** `users.manage`, as for other resources, or a new
    capability for webhooks?
    Recommendation: `users.manage`, with the acknowledgement for `verify
none`. The question an approver answers is the same as for any resource:
    should this service get this.
