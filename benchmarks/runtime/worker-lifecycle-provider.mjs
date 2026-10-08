// Uses a producer-built npm installation and generated public fixture contract.
import { TrellisService } from "@oatscenter/trellis/service";
import { Result } from "@oatscenter/trellis";
import { participants } from "./generated/index.js";
const provider = await TrellisService.connect({
  trellisUrl: process.env.TRELLIS_URL,
  participant: participants.Provider.participant,
  seed: process.env.TRELLIS_IDENTITY_SEED,
  runtime: {
    verificationWorkers: true,
    timeout: 60_000,
    maxVerificationWorkers: Number(
      process.env.TRELLIS_MAX_VERIFICATION_WORKERS ?? 3,
    ),
  },
}).orThrow();
await provider.handleEcho(async ({ input, client }) => {
  await client.kv.records.put(`entered-${input.value}`, input).orThrow();
  return Result.ok(input);
});
console.log("worker-probe-ready");
await provider.wait();
