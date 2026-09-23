import { isErr, ok } from "@oats-center/trellis";
import type { RpcHandler } from "@oats-center/trellis/service";
import { type participants } from "../../../trellis/index.js";

type Handler = RpcHandler<
  typeof participants.Service.participant,
  "Sites.Get"
>;

export const getSite: Handler = async ({ input, client }) => {
  const entry = await client.kv.siteSummaries.get(input.siteId).take();

  return ok({ site: isErr(entry) ? undefined : entry });
};
