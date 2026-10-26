---
title: "Use a live observation for filtered live views"
description: "Expose caller-filtered live views instead of forwarding broad service events to browsers."
---

# Declare live observations

Live observations provide caller-filtered live views.

```trellis
model OrdersWatchInput { customerId: string; }
model OrdersWatchEvent { orderId: string; status: string; }

api orders@v1 {
  // ...
  live Watch { input OrdersWatchInput; event OrdersWatchEvent; }
  capabilities {
    // ...
    capability orders.watch {
      title "Watch orders";
      description "Follow a customer's orders as they change.";
      grants "The caller sees order status changes for the customers it asks about.";
      allows { live Watch; }
    }
  }
}
```

Generate the SDK and implement the generated live registration in the service.

Register `service.handleWatch`
before starting the service. Its handler returns an async iterable of typed
events and observes `signal` for cancellation. Release upstream subscriptions
in `finally` when iteration ends or the caller cancels.

The caller requires `orders.watch` in its `use orders { ... }` block and calls
the generated `watch` method with its filter input and an `AbortSignal`. Consume the
returned iterable and abort when the view or task no longer needs it. Live observations are
live observations, not durable event replay or a replacement for state snapshots.

Use one live test that receives a frame and then cancels. Assert that the
provider stops or iteration ends; fixed sleeps and internal frame counters do
not prove the lifecycle boundary.
