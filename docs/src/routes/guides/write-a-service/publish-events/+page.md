---
title: "Publishing your own events"
description: "Declare and publish an Orders Shipped event for downstream services."
---

# Publish events

Declare the payload model and event on the owned API:

```trellis
model OrderShippedEvent { orderId: string; }

api orders@v1 {
  // ...
  event Shipped { payload OrderShippedEvent; }
  capabilities {
    // ...
    capability orders.watchShipments {
      title "Watch shipments";
      description "Be told when orders ship.";
      grants "The subscriber learns which orders shipped.";
      allows { subscribe event Shipped; }
    }
  }
}
```

The generated service facade exposes the typed publisher. Subject derivation and permission requirements remain protocol-owned.

Subscribers require a capability that allows `subscribe event Shipped`. After
regeneration, publish with `service.publishShipped({ orderId })` and
handle its `Result`. This sends an event now; it does not promise that a remote
handler has completed. Trellis adds signed event metadata at publish time, so
the application supplies only its declared payload.

When shipment persistence and event enqueue must be atomic, prepare the event
and enqueue it in the same SQL transaction using the outbox adapter. Publish the
committed outbox through its dispatcher. Publishing before committing the
shipment risks an event for data that was rolled back; publishing afterward
without an outbox risks losing the event on a crash.
