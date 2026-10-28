import { ok } from "@oatscenter/result";
import { createInterface } from "node:readline";
import { participants } from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { TrellisService } from "../service/mod.ts";

const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const input = await lines.next();
if (input.done) throw new Error("provisioned identity required");
const { trellisUrl, seed } = JSON.parse(input.value) as {
  trellisUrl: string;
  seed: string;
};
const service = await TrellisService.connect({
  trellisUrl,
  participant: participants.NativeOptionalProvider.participant,
  seed,
}).orThrow();
try {
  await service.handleRead(({ input }) => ok(input));
  await service.handleExtend(async ({ input, op }) => {
    const emit = await op.nextSignal("Continue").orThrow();
    await op.emitUpdate({ value: "TypeScript provider update" }).orThrow();
    await op.acknowledgeSignal(emit.sequence).orThrow();
    const finish = await op.nextSignal("Continue").orThrow();
    await op.acknowledgeSignal(finish.sequence).orThrow();
    return await op.complete(input).orThrow();
  });
  console.log(JSON.stringify({ stage: "ready" }));
  await lines.next();
} finally {
  await service.stop();
}
