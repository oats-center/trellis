import { Result } from "@oatscenter/trellis";
import type { RpcHandler } from "@oatscenter/trellis/service";
import { participants } from "./trellis/index.js";
import { ulid } from "ulid";

/** Returns an example order receipt; this walkthrough does not persist orders. */
export const createOrder: RpcHandler<
  typeof participants.OrdersService.participant,
  "Create"
> = ({ input }) => Result.ok({ orderId: ulid(), customerId: input.customerId });
