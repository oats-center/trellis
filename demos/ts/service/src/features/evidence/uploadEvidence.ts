import type { OperationHandler } from "@oats-center/trellis/service";
import { participants } from "../../../trellis/index.js";
import { recordActivity } from "../activity/index.ts";

export const uploadEvidence: OperationHandler<
  typeof participants.Service.participant,
  "Evidence.Upload"
> = async ({ input, op, transfer, client }) => {
  const transferred = await transfer.completed().orThrow();

  await op.started().orThrow();
  await op.progress({
    stage: "staged",
    message:
      `Staged ${transferred.size} bytes of ${input.evidenceType} evidence`,
  }).orThrow();

  const entry = await (await client.store.uploads.open().orThrow()).get(
    transferred.key,
  )
    .orThrow();
  const reader = (await entry.stream().orThrow()).getReader();
  let chunkCount = 0;
  let byteCount = 0;

  await op.progress({
    stage: "processing",
    message: `Inspecting staged evidence at ${transferred.key}`,
  }).orThrow();

  try {
    while (true) {
      const next = await reader.read();
      if (next.done) {
        break;
      }

      chunkCount += 1;
      byteCount += next.value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }

  await op.progress({
    stage: "indexed",
    message: `Indexed ${chunkCount} evidence blocks from ${transferred.key}`,
  }).orThrow();

  const output = {
    evidenceId: input.metadata?.evidenceId ?? input.key,
    key: transferred.key,
    size: BigInt(byteCount),
    ...(input.contentType ? { contentType: input.contentType } : {}),
    ...(input.metadata?.fileName ? { fileName: input.metadata.fileName } : {}),
    disposition: "ready-for-review",
  };

  await client.publishEvidenceUploaded({
    evidenceId: output.evidenceId,
    key: output.key,
    size: output.size,
    ...(output.contentType ? { contentType: output.contentType } : {}),
    ...(output.fileName ? { fileName: output.fileName } : {}),
    evidenceType: input.evidenceType,
    uploadedAt: new Date().toISOString(),
  }).orThrow();
  await recordActivity(client, {
    kind: "evidence-uploaded",
    message: `Uploaded ${input.evidenceType} evidence from ${transferred.key}`,
  });

  return await op.complete(output).orThrow();
};
