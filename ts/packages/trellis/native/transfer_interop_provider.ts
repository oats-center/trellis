import assert from "node:assert/strict";
import { Result } from "@oatscenter/result";
import { createInterface } from "node:readline";
import { participants } from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { TrellisService } from "../service/mod.ts";

const stdin = createInterface({ input: process.stdin });
const messages = stdin[Symbol.asyncIterator]();
const first = await messages.next();
if (first.done) throw new Error("provider identity required");
const config = JSON.parse(first.value) as { trellisUrl: string; seed: string };
const service = await TrellisService.connect({
  ...config,
  participant: participants.Provider.participant,
}).orThrow();
try {
  const store = await service.store.files.open().orThrow();
  await store
    .put(
      "download",
      Uint8Array.from({ length: 5 * 1024 * 1024 + 7 }, (_, i) => i % 251),
    )
    .orThrow();
  await service.handleDownload(async ({ input, context }) => {
    assert(context.caller.type === "verified");
    const transfer = await service
      .createTransfer({
        direction: "receive",
        store: "files",
        key: "download",
        sessionKey: context.caller.sessionPublicKey,
        connectionId: context.caller.runtimeId,
        contextDigest: context.caller.authorityDigest,
        permission: context.permission,
        requiredCapabilities: context.requiredCapabilities,
        inboxPrefix: context.inboxPrefix,
      })
      .orThrow();
    return Result.ok({ ...input, transfer });
  });
  await service.handleUpload(async ({ input, op, transfer }) => {
    assert(transfer);
    const body = await transfer.stream().orThrow();
    let offset = 0;
    for await (const chunk of body) {
      for (const byte of chunk) assert.equal(byte, offset++ % 251);
    }
    assert.equal(offset, 5 * 1024 * 1024 + 7);
    return await op.complete(input).orThrow();
  });
  console.log(JSON.stringify({ stage: "ready" }));
  await messages.next();
} finally {
  stdin.close();
  await service.stop();
}
