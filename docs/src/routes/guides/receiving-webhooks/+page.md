---
title: "Receiving webhooks"
description: "Declare a webhook endpoint in IDL, set its signing secret, handle accepted requests as events, and answer handshakes with a decide handler."
---

# Receiving webhooks

A service that receives webhooks from Stripe, GitHub, or a partner system
doesn't run its own HTTP listener. It declares an endpoint in its IDL. Trellis
serves the endpoint on its own origin, checks that each request is genuine,
records it, and hands accepted requests to the service as `Received` events
through the endpoint's own durable consumer. If the service is down, requests
wait.

The design, with every status code and limit, is in
[Receiving Webhooks](/design/core/webhooks).

## Declare an endpoint

An endpoint names its verifiers, and each verifier names the settings that
hold its secrets. Keys, tokens, and passwords must be `secret` settings of the
same service. The service package needs a direct dependency on the `trellis`
package, which declares `trellis.webhooks@v1`.

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
    verify hmac {
      key config stripeSigningSecret;
      algorithm sha256;
      signed "{timestamp}.{body}";
      signature { header "Stripe-Signature"; separator ","; prefix "v1="; encoding hex; }
      timestamp { header "Stripe-Signature"; separator ","; prefix "t="; format unix_seconds; tolerance 5m; }
    }
    dedupe "{json:/id}";
  }
}
```

Stripe puts each event's ID in the `id` field of the JSON body, so
`dedupe "{json:/id}"` drops Stripe's retries of the same event. `{json:...}`
takes a JSON Pointer such as `/data/object/id`, `{form:name}` reads a field of
a form-encoded body, and `{header:Name}` reads a header, for senders such as
GitHub that send the ID as `X-GitHub-Delivery`. A request whose ID can't be
read, such as a body that isn't JSON, is delivered without deduplication;
it's never refused for that.

Other verifiers check a fixed token in a header or the query string
(`verify token`), HTTP Basic credentials (`verify basic`), or the client
address against a `list<string>` setting (`verify address`). An endpoint that
checks nothing says `verify none;`, and approving it needs an explicit
acknowledgement.

Trellis checks every verifier, the signature included, before it answers. A
wrong signature or credential gets 401 and a secret that isn't set gets 503,
so a wrong secret shows up as failed deliveries in the sender's dashboard. An
endpoint is `async` by default: once the checks pass, Trellis stores the
request and answers 2xx, then delivers it to the service. Use `mode sync;`
when the sender reads the response, for a handshake or a reply document.
Stripe reads only the status, so the example stays `async`: Stripe gets its
2xx as soon as the request is stored, and events wait while the service is
down. Every member except `title` and `description` is part of the build, so
changing one waits for approval.

## Find the URL and set the secret

After approval, each endpoint has one URL:

```sh
trellis svc billing webhooks
trellis svc billing webhooks stripe
```

Give that URL to the sender, then set the secret it shows you:

```sh
trellis svc billing config set-secret stripeSigningSecret
```

The Console's service page has the same in its Webhooks section, with a copy
button for each URL.

When the sender changes its secret, token, or password, set the new one.
Trellis accepts the previous value for 24 hours, so requests the sender made
before the change still pass. Only the value the change replaced is accepted,
never an older one. For each secret, the endpoint shows until when the old
value is accepted, when a request last matched the new value, and when one
last matched the old value since the change. Each secret has its own times,
and a change starts them afresh, so an old secret's use before the change
never shows up as use after it. Deleting the previous settings revision
with `trellis svc billing config delete-revision <revision>` ends the overlap
at once.

A stored request is checked under the policy it was accepted with, when it is
processed and when it is verified again. Its token or password, checked on
arrival, keeps counting while the endpoint's verifiers and the settings they
read (address ranges, the Basic username) are unchanged. A request that
arrived with the old value during the overlap still passes if it is processed
or verified again after the overlap ends. Deleting the settings
revision that holds the old value withdraws it from waiting requests too.
Otherwise the request is rejected, and its record says why. A signed request is
checked again under the current secrets and address ranges when it is
processed or verified again, with the old secret accepted on the same terms.

## Handle requests in TypeScript

Each endpoint is a handle on `service.webhooks`. The event carries the body
byte for byte in `event.body`. `event.text()`, `event.json()`, and
`event.form()` read it as text, JSON, or form fields; each returns a `Result`
that holds a `ValidationError` when the body isn't UTF-8 text or isn't JSON.

```ts
const service = await TrellisService.connect({
  trellisUrl,
  participant,
  seed,
}).orThrow();

await service.webhooks.stripe
  .onReceived(async ({ event }) => {
    await recordPayment(event.json().orThrow());
  })
  .orThrow();

await service.wait();
```

Delivery is at least once with no ordering guarantee, so the handler must be
safe to run twice. `dedupe` drops the sender's own retries on a best-effort
basis, including retries that arrive at the same moment; deduplicating by
business meaning, such as one charge per payment ID, is the service's job.

## Answer handshakes with a decide handler

A `sync` endpoint that declares `decide { deadline 2s; }` calls the service
after the standard verifiers pass. The handler returns `accept`, `ignore`, or
`reject`, and in `sync` mode can give the sender a response.

```ts
import { webhookResponse } from "@oatscenter/trellis/service";

type SlackBody = { type: string; challenge?: string; event_id?: string };

await service.webhooks.slack
  .decide((request) => {
    const body = request.delivery.json().orThrow() as SlackBody;
    if (body.type === "url_verification") {
      return {
        verdict: "ignore",
        response: webhookResponse({
          contentType: "text/plain",
          body: body.challenge,
        }),
      };
    }
    return { verdict: "accept", dedupeKey: body.event_id };
  })
  .orThrow();
```

`request.delivery` has the same body views as the event. The handler can run
more than once for the same delivery. Trellis refuses
HTML and script content types, cookies, and responses over 64 KiB, and answers
502 instead. `webhookHmac`, `verifyWebhookHmac`, and `withinWebhookWindow`
help a handler check a signature scheme no standard verifier describes.

## Handle requests in Rust

The generated provider has one method per endpoint:

```rust
use trellis_rs::client::WebhookBody as _;

let stripe = provider.stripe()?;
let mut deliveries = stripe.subscribe().await?;
while let Some(delivery) = deliveries.next().await {
    let delivery = delivery?;
    let event: serde_json::Value = delivery.json()?;
    record_payment(&event).await?;
}
```

`delivery.body.0` holds the bytes as received. The `WebhookBody` trait adds
`text()`, `json::<T>()`, and `form()`, which return a `WebhookBodyError` when
the body isn't UTF-8 text or isn't the JSON you asked for.

`decide(handler)` on the same handle answers decide calls; it keeps answering
until the returned handle is dropped.

## Watch and recover

```sh
trellis svc billing webhooks stripe attempts --outcome rejected
trellis svc billing webhooks stripe attempt <attempt-id>
trellis svc billing webhooks stripe reverify <attempt-id>
```

`attempt` and `reverify` act only on a request of the endpoint you name. A
request rejected because the secret was wrong can be verified again once the
secret is fixed, while the intake stream still holds it (7 days by default).
A request refused at intake for a wrong token or password cannot: its
credential was never stored.

Each endpoint also shows in the service's health as a check named
`webhook:<endpoint>`, with no code in the service. It turns degraded while the
endpoint is disabled, while a secret or setting its verifiers read isn't set,
and while the newest verified request of the last 10 minutes was rejected; its
summary says which, such as
`the latest request was rejected: signature does not match`. It clears by
itself once the secret is set, a later request is accepted, or 10 minutes pass
without a rejection. A request refused before verification, such as one over
the size limit, still gets a record but doesn't change the check. The check appears in `trellis svc billing instances` and
on the service's Console page.

Runtime settings belong to the deployment:

```sh
trellis svc billing webhooks stripe set max-body 512KiB
trellis svc billing webhooks stripe set dedupe-window 24h
trellis svc billing webhooks stripe set max-body default
trellis svc billing webhooks stripe disable
trellis svc billing webhooks stripe rotate-url
```

Durations take `ms`, `s`, `m`, `h`, or `d`, sizes take `B`, `KiB`, `MiB`, or
`GiB`, and counts are plain numbers; `set max-body 5m` is refused. `rotate-url`
gives the endpoint a new URL and keeps the old one working until `retire-url`.

Requests to `async` endpoints wait for verification in the deployment's intake
stream, which keeps them 7 days and holds at most 64 MiB by default. A full
stream answers new requests with 503 rather than drop ones still waiting.
`set intake-max-bytes 256MiB` raises the size, but NATS reserves the whole size
on its disk as soon as the stream exists, so keep it well inside what the
broker has.

## Testing

Tests send real HTTPS requests to the endpoint URL of a service the testkit
started, with secrets set through the normal settings RPCs. Nothing bypasses
intake or verification. With the TypeScript testkit, list the endpoints with
`runtime.callAdminRpc("webhooksEndpointsList", { service: deploymentId })`. With the Rust testkit, `runtime.webhook_urls(&identity)` returns them by
endpoint name.
