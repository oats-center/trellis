import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "./trellis/index.js";
import { createOrder } from "./service.ts";

// Environment variables are this example's configuration choice, not a Trellis requirement.
function requiredEnv(name: string): string {
  const value = process.env[name];
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
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
try {
  await service.handleCreate(createOrder);
  console.log("Orders service connected; press Ctrl-C to stop.");
  await service.wait();
} finally {
  process.off("SIGINT", stop);
  process.off("SIGTERM", stop);
  await service.stop();
}
