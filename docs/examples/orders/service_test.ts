import assert from "node:assert/strict";
import { test } from "node:test";
import { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { TrellisService } from "@oatscenter/trellis/service";
import { apps, participants } from "./trellis/index.js";

import { createOrder } from "./service.ts";

test("orders caller invokes the real service", async () => {
  const cli = process.env.TRELLIS_TEST_CLI_BIN;
  const server = process.env.TRELLIS_TEST_SERVER_BIN;
  if ((cli === undefined) !== (server === undefined)) {
    throw new Error(
      "Set both TRELLIS_TEST_CLI_BIN and TRELLIS_TEST_SERVER_BIN or neither",
    );
  }
  const runtime = await TrellisTestRuntime.start(
    cli && server
      ? { trellis: { source: { kind: "path", cli, server } } }
      : undefined,
  );
  try {
    const identity = await runtime.registerService({
      name: "orders",
      contract: participants.OrdersService.participant,
    });
    const service = await TrellisService.connect({
      participant: participants.OrdersService.participant,
      name: "orders-service",
      trellisUrl: runtime.oauthIssuer,
      seed: identity.seed,
    }).orThrow();
    let exit: Promise<unknown> | undefined;
    try {
      await service.handleCreate(createOrder);
      exit = service.wait().catch((error: unknown) => error);
      const client = await runtime.connectClient({
        name: "caller",
        contract: apps.OrdersCaller.request,
      });
      const order = await client.create({ customerId: "customer-1" }).orThrow();
      assert.equal(order.customerId, "customer-1");
      assert.match(order.orderId, /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
    } finally {
      await service.stop();
      const failure = await exit;
      if (failure) throw failure;
    }
  } finally {
    await runtime.stop();
  }
});
