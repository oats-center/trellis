import { Result } from "@oatscenter/trellis";
import type { RpcHandler } from "@oatscenter/trellis/service";
import { participants } from "orders-trellis";

/** Returns an example order receipt; this walkthrough does not persist orders. */
export const createOrder: RpcHandler<
  typeof participants.OrdersService.participant,
  "Create"
> = (
  { input },
) => Result.ok({ orderId: crypto.randomUUID(), customerId: input.customerId });
