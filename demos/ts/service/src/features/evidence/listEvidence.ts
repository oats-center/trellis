import { ok } from "@qlever-llc/trellis";
import type { RpcHandler } from "@qlever-llc/trellis/service";
import { type participants } from "../../../trellis/index.js";

type Handler = RpcHandler<
  typeof participants.Service.participant,
  "Evidence.List"
>;

function evidenceIdForKey(
  key: string,
  metadata: Record<string, string>,
): string {
  return metadata.evidenceId || key;
}

/** Lists image evidence staged in the demo object store. */
export const listEvidence: Handler = async ({ input, client }) => {
  const page = await (await client.store.uploads.open().orThrow()).list({
    prefix: input.prefix ?? "evidence/",
    cursor: input.page?.cursor,
    limit: input.page?.limit,
  }).orThrow();
  const evidence = [];

  for (const info of page.entries) {
    evidence.push({
      evidenceId: evidenceIdForKey(info.key, info.metadata),
      key: info.key,
      size: BigInt(info.size),
      ...(info.contentType ? { contentType: info.contentType } : {}),
      evidenceType: info.metadata.evidenceType || "image",
      ...(info.metadata.fileName ? { fileName: info.metadata.fileName } : {}),
      uploadedAt: info.updatedAt,
    });
  }

  return ok({
    items: evidence,
    page: page.nextCursor === undefined ? {} : { nextCursor: page.nextCursor },
  });
};
