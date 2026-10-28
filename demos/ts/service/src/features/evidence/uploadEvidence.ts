import { UnexpectedError } from "@oatscenter/trellis";
import type { OperationHandler } from "@oatscenter/trellis/service";
import { participants } from "../../../trellis/index.js";
import { recordActivity } from "../activity/index.ts";

export const uploadEvidence: OperationHandler<
  typeof participants.Service.participant,
  "Evidence.Upload"
> = async ({ input, op, transfer, client, signal }) => {
  if (signal.aborted) return;
  if (!transfer) {
    await op
      .fail(
        new UnexpectedError({
          cause: new Error("Active evidence upload requires a transfer"),
        }),
      )
      .orThrow();
    return;
  }
  const transferred = await transfer.completed().orThrow();

  await op.started().orThrow();
  await op
    .progress({
      stage: "staged",
      message: `Staged ${transferred.size} bytes of ${input.evidenceType} evidence`,
    })
    .orThrow();

  let chunkCount = 0;
  let byteCount = 0;

  await op
    .progress({
      stage: "processing",
      message: `Inspecting staged evidence for ${input.key}`,
    })
    .orThrow();

  const source = (await transfer.stream().orThrow()).pipeThrough(
    new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        chunkCount += 1;
        byteCount += chunk.byteLength;
        controller.enqueue(chunk);
      },
    }),
  );
  await (
    await client.store.uploads.open().orThrow()
  )
    .put(input.key, source, {
      ...(input.contentType ? { contentType: input.contentType } : {}),
      metadata: input.metadata,
    })
    .orThrow();

  await op
    .progress({
      stage: "indexed",
      message: `Indexed ${chunkCount} evidence blocks at ${input.key}`,
    })
    .orThrow();

  const output = {
    evidenceId: input.metadata?.evidenceId ?? input.key,
    key: input.key,
    size: BigInt(byteCount),
    ...(input.contentType ? { contentType: input.contentType } : {}),
    ...(input.metadata?.fileName ? { fileName: input.metadata.fileName } : {}),
    disposition: "ready-for-review",
  };

  await client
    .publishEvidenceUploaded({
      evidenceId: output.evidenceId,
      key: output.key,
      size: output.size,
      ...(output.contentType ? { contentType: output.contentType } : {}),
      ...(output.fileName ? { fileName: output.fileName } : {}),
      evidenceType: input.evidenceType,
      uploadedAt: new Date().toISOString(),
    })
    .orThrow();
  await recordActivity(client, {
    kind: "evidence-uploaded",
    message: `Uploaded ${input.evidenceType} evidence at ${input.key}`,
  });

  return await op.complete(output).orThrow();
};
