---
title: Trellis
description: Trellis connects services, apps, and devices through typed contracts, with built-in permissions, events, live views, and background jobs.
layout: false
---

<script>
  import Hero from '#lib/landing/Hero.svelte'
  import Section from '#lib/landing/Section.svelte'
  import Bento from '#lib/landing/Bento.svelte'
  import Timeline from '#lib/landing/Timeline.svelte'
  import UseCases from '#lib/landing/UseCases.svelte'
  import Closing from '#lib/landing/Closing.svelte'
</script>

<div class="family-page">

<Hero />

<Section eyebrow="Features" title="What Trellis gives you" lede="The parts every connected system ends up building, done once: typed APIs, permissions, delivery, and background work." link={{ to: '/guides/concepts', label: 'Trellis concepts' }}>
<Bento />
</Section>

<Section eyebrow="How it works" title="From contract to running service" lede="The tutorial follows these steps for an orders service. Each one is a command or a file you write." link={{ to: '/guides/write-a-service', label: 'Tutorial: write a service' }}>
<Timeline />
</Section>

<Section split eyebrow="Example" title="One contract, typed on both sides" lede="The contract names the request, its types, and the capability a caller needs. The service handler is typed from the generated SDK, so a payload that drifts from the contract doesn't compile." link={{ to: '/guides/authoring-contracts', label: 'Author contracts' }}>
<Tabs activeName="contract.trellis">
<TabPanel name="contract.trellis">

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
      consequence "The caller may create customer orders.";
      consent_revision 1;
      allows { rpc Create; }
    }
  }
}

service OrdersService { implements orders; }
agent OrdersCaller { use orders { required capability orders.create; } }
```

</TabPanel>
<TabPanel name="service.ts">

```ts
import { Result } from "@oatscenter/trellis";
import type { RpcHandler } from "@oatscenter/trellis/service";
import { participants } from "./trellis/index.js";
import { ulid } from "ulid";

export const createOrder: RpcHandler<
  typeof participants.OrdersService.participant,
  "Create"
> = ({ input }) => Result.ok({ orderId: ulid(), customerId: input.customerId });
```

</TabPanel>
</Tabs>
</Section>

<Section eyebrow="Who uses it" title="Services, apps, devices, and tools" lede="Every kind of program connects the same way and is held to the same permissions.">
<UseCases />
</Section>

<Closing />

</div>
