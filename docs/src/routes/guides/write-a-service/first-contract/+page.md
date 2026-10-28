---
title: "Your first API and participant"
description: "Declare the smallest valid Trellis API and service participant in native IDL."
---

# Declare an API and participants

```trellis
model CreateOrderRequest { customerId: string; }
model CreateOrderResponse { orderId: string; customerId: string; }

api orders@v1 {
  title "Orders";
  description "Minimal service walkthrough.";
  rpc Create { input CreateOrderRequest; output CreateOrderResponse; }
  capabilities {
    capability orders.create {
      title "Create orders";
      description "Create an order for a customer.";
      grants "The caller may create customer orders.";
      allows { rpc Create; }
    }
  }
}

service OrdersService { implements orders; }
app OrdersCaller { use orders { required capability orders.create; } }
```

The service implements the API. The app requires only `orders.create`, which
allows `Create`; importing types alone grants nothing. Run `trellis install` and
`trellis generate`, then use the configured package's generated `apis` and
`participants`. Exact generated names are authoritative: the authored API token
and generated path remain lowercase (`orders`), while generated language symbols
may use PascalCase (`Orders`). Package version is metadata, not authorization.
