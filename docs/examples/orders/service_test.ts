import { assertEquals, assertMatch } from "@std/assert";
import { TrellisTestRuntime } from "@qlever-llc/trellis-test";
import { TrellisService } from "@qlever-llc/trellis/service";
import { participants } from "orders-trellis";

import { createOrder } from "./service.ts";

Deno.test("orders caller invokes the real service", async () => {
  const runtime = await TrellisTestRuntime.start({
    trellis: {
      command: {
        cmd: Deno.env.get("TRELLIS_TEST_SERVER_BIN") ?? "trellis-server",
        args: ["--config", "{config}", "all"],
      },
    },
  });
  try {
    const identity = await runtime.registerService({
      name: "orders",
      contract: participants.OrdersService.participant,
    });
    const service = await TrellisService.connect({
      participant: participants.OrdersService.participant,
      name: "orders-service",
      trellisUrl: runtime.trellisUrl,
      seed: identity.seed,
    }).orThrow();
    let exit: Promise<unknown> | undefined;
    try {
      await service.handleCreate(createOrder);
      exit = service.wait().catch((error: unknown) => error);
      const client = await runtime.connectClient({
        name: "caller",
        contract: participants.OrdersCaller.participant,
      });
      const order = await client.create({ customerId: "customer-1" })
        .orThrow();
      assertEquals(order.customerId, "customer-1");
      assertMatch(order.orderId, /^[0-9a-f-]{36}$/);
    } finally {
      await service.stop();
      const failure = await exit;
      if (failure) throw failure;
    }
  } finally {
    await runtime.stop();
  }
});
