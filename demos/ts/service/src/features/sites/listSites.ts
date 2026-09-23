import { isErr, ok } from "@oats-center/trellis";
import type { SiteSummary } from "../../../../shared/field_data.ts";
import type { RpcHandler } from "@oats-center/trellis/service";
import { type participants } from "../../../trellis/index.js";
import { paginate } from "../../pagination.ts";

type Handler = RpcHandler<
  typeof participants.Service.participant,
  "Sites.List"
>;

export const listSites: Handler = async ({ input, client }) => {
  const sites: SiteSummary[] = [];
  const keys = await client.kv.siteSummaries.keys(">").orThrow();

  for await (const key of keys) {
    const entry = await client.kv.siteSummaries.get(key).take();
    if (!isErr(entry) && entry !== undefined) {
      sites.push(entry);
    }
  }

  return ok(
    await paginate({
      endpoint: "demo.fieldops.Sites.List",
      cursor: input.page?.cursor,
      limit: input.page?.limit,
      rows: sites,
      key: (site) => `${site.siteName}\0${site.siteId}`,
    }),
  );
};
