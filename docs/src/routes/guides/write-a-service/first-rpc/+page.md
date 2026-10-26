---
title: "Writing our first RPC"
description: "Add an Orders Get RPC: its models, API declaration, capability, and handler."
---

# Add an RPC

Declare request and response models, then reference them from the API surface:

```trellis
model GetOrderRequest { orderId: string; }
model GetOrderResponse { orderId: string; status: string; }

api orders@v1 {
  // ...the existing Create RPC and capability
  rpc Get { input GetOrderRequest; output GetOrderResponse; }
  capabilities {
    capability orders.read {
      title "Read orders";
      description "Read an order.";
      grants "The caller may read customer orders.";
      allows { rpc Get; }
    }
  }
}
```

After `trellis generate`, import the generated descriptor or participant facade. Runtime code does not execute the IDL source and does not construct API metadata.

Run `trellis generate` after the edit. Connect the
generated service participant, then register `service.handleGet(...)` before
`service.wait()`. The handler receives typed `input` and returns a `Result`
containing the declared output. Keep database access in an application-owned
closure instead of adding fields to the Trellis handler context.

A caller requires the capability that allows the RPC,
`app OrdersCaller { use orders { required capability orders.read; } }`, and
regenerates. It can then call `client.get({ orderId })` without knowing
the NATS subject or wire envelope. A selected surface still requires granted
authority; generated methods are not an authorization bypass.

Test a real returned order through this generated caller. Unknown orders should
use the [declared error](/guides/write-a-service/retrieve-order), not an empty
successful response or an untyped thrown exception.
