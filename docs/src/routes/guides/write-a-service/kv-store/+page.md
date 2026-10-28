---
title: "Adding a database"
description: "Declare a service-owned KV bucket and open a typed KV client from the runtime binding."
---

# Declare service resources

Participant-owned resources are native IDL declarations, not runtime constructor options.

```trellis
model Order { orderId: string; customerId: string; status: string; }

service OrdersService {
  implements orders;
  kv orders {
    title "Orders";
    description "Current order records.";
    schema Order;
    version 1;
    history 1;
    ttl 0ms;
  }
}
```

Connect with `TrellisService.connect(...)` and use the resolved `service.kv` handle. Resource binding payloads are runtime internals.

`schema` names the stored model and `version` the version of that stored
representation; change the version rather than silently changing the stored
shape under the same one. Run `trellis generate` so the generated
`service.kv.orders` handle carries the correct value type. Trellis
provisions the physical binding during deployment apply; the service
must not create a bucket or derive its physical name.

Use returned revisions for compare-and-set mutations when concurrent writers
must not silently overwrite each other. Handle missing keys and revision
conflicts as expected outcomes. A KV write cannot atomically commit an unrelated
SQL transaction: use the SQL outbox when a database mutation and event must
commit together.
