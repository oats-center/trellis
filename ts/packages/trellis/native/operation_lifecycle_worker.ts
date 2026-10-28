import { ok } from "@oatscenter/result";
import { createInterface } from "node:readline";
import { participants } from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { TrellisService } from "../service/mod.ts";

const messages = createInterface({ input: process.stdin })[
  Symbol.asyncIterator
]();
const next = await messages.next();
if (next.done) throw new Error("provisioned worker identity required");
const { trellisUrl, seed, holdCleanup } = JSON.parse(next.value) as {
  trellisUrl: string;
  seed: string;
  holdCleanup: boolean;
};
const service = await TrellisService.connect({
  trellisUrl,
  seed,
  participant: participants.NativeOptionalProvider.participant,
}).orThrow();
try {
  await service.handleRead(({ input }) => ok(input));
  await service.handleExtend(async ({ op, signal, resuming }) => {
    console.log(
      JSON.stringify({ stage: "entered", operationId: op.id, resuming }),
    );
    if (!signal.aborted) {
      await new Promise<void>((resolve) =>
        signal.addEventListener("abort", () => resolve(), { once: true }),
      );
    }
    console.log(
      JSON.stringify({ stage: "cleaning", operationId: op.id, resuming }),
    );
    if (holdCleanup) await messages.next();
    return await op.cancel().orThrow();
  });
  console.log(JSON.stringify({ stage: "ready" }));
  await new Promise<void>(() => {});
} finally {
  await service.stop();
}
