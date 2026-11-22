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
    dedupe "{header:Idempotency-Key}";
  }
}
```

Other verifiers check a fixed token in a header or the query string
(`verify token`), HTTP Basic credentials (`verify basic`), or the client
address against a `list<string>` setting (`verify address`). An endpoint that
checks nothing says `verify none;`, and approving it needs an explicit
acknowledgement.

An endpoint is `async` by default: Trellis stores the request, answers 2xx at
once, and verifies afterwards. Use `mode sync;` when the sender reads the
response, for a handshake or a reply document. Every member except `title` and
`description` is part of the build, so changing one waits for approval.

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

An `async` request is checked under the policy it was accepted with. Its token
or password, checked on arrival, keeps counting only while the value it
matched is still accepted and the endpoint's verifiers and the settings they
read (address ranges, the Basic username) are unchanged. Otherwise the request
is rejected, and its record says why. A signed request is checked again under
the secrets and address ranges in force when it is processed or verified
again.

## Handle requests in TypeScript

Each endpoint is a handle on `service.webhooks`. The event carries the body
byte for byte; parse it yourself.

```ts
const service = await TrellisService.connect({
  trellisUrl,
  participant,
  seed,
}).orThrow();

await service.webhooks.stripe
  .onReceived(async ({ event }) => {
    const payload = JSON.parse(new TextDecoder().decode(event.body));
    await recordPayment(payload);
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

await service.webhooks.slack
  .decide((request) => {
    const body = JSON.parse(new TextDecoder().decode(request.delivery.body));
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

The handler can run more than once for the same delivery. Trellis refuses
HTML and script content types, cookies, and responses over 64 KiB, and answers
502 instead. `webhookHmac`, `verifyWebhookHmac`, and `withinWebhookWindow`
help a handler check a signature scheme no standard verifier describes.

## Handle requests in Rust

The generated provider has one method per endpoint:

```rust
let stripe = provider.stripe()?;
let mut deliveries = stripe.subscribe().await?;
while let Some(delivery) = deliveries.next().await {
    let delivery = delivery?;
    record_payment(&delivery.body).await?;
}
```

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

## Testing

Tests send real HTTPS requests to the endpoint URL of a service the testkit
started, with secrets set through the normal settings RPCs. Nothing bypasses
intake or verification. With the TypeScript testkit, list the endpoints with
`runtime.callAdminRpc("webhooksEndpointsList", { service: deploymentId })`. With the Rust testkit, `runtime.webhook_urls(&identity)` returns them by
endpoint name.
