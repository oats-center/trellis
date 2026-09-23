import { TrellisService } from "@oats-center/trellis/service";
import { participants } from "orders-trellis";
import { createOrder } from "./service.ts";

// Environment variables are this example's configuration choice, not a Trellis requirement.
function requiredEnv(name: string): string {
  const value = Deno.env.get(name);
  if (!value) throw new Error(`Missing ${name}`);
  return value;
}

const service = await TrellisService.connect({
  participant: participants.OrdersService.participant,
  trellisUrl: requiredEnv("TRELLIS_URL"),
  seed: requiredEnv("TRELLIS_IDENTITY_SEED"),
}).orThrow();

const stop = () => {
  void service.stop();
};
Deno.addSignalListener("SIGINT", stop);
Deno.addSignalListener("SIGTERM", stop);
try {
  await service.handleCreate(createOrder);
  console.log("Orders service connected; press Ctrl-C to stop.");
  await service.wait();
} finally {
  Deno.removeSignalListener("SIGINT", stop);
  Deno.removeSignalListener("SIGTERM", stop);
  await service.stop();
}
