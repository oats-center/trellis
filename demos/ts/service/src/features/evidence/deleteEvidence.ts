import { ok } from "@oats-center/trellis";
import type { RpcHandler } from "@oats-center/trellis/service";
import { type participants } from "../../../trellis/index.js";
import { recordActivity } from "../activity/index.ts";

type Handler = RpcHandler<
  typeof participants.Service.participant,
  "Evidence.Delete"
>;

/** Deletes a stored evidence object from the demo evidence locker. */
export const deleteEvidence: Handler = async ({ input, client }) => {
  await (await client.store.uploads.open().orThrow()).delete(input.key)
    .orThrow();
  await recordActivity(client, {
    kind: "evidence-deleted",
    message: `Deleted evidence upload ${input.key}`,
  });

  return ok({ key: input.key, deleted: true });
};
