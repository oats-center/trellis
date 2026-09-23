import { UnexpectedError } from "@oatscenter/trellis";
import type { OperationHandler } from "@oatscenter/trellis/service";
import { participants } from "../../../trellis/index.js";

import { recordActivity } from "../activity/index.ts";

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export const refreshSite: OperationHandler<
  typeof participants.Service.participant,
  "Sites.Refresh"
> = async ({ input, op, client }) => {
  await op.started().orThrow();
  await op.progress({
    stage: "queued",
    message: `Queued summary refresh for ${input.siteId}`,
  }).orThrow();
  await pause(900);

  const job = await client.jobs.refreshSiteSummary.create({
    siteId: input.siteId,
  }).orThrow();
  await pause(700);

  await op.progress({
    stage: "refreshing",
    message: `Refreshing field status for ${input.siteId}`,
  }).orThrow();

  const completedJob = await job.wait().orThrow();
  if (completedJob.state !== "completed" || !completedJob.result) {
    await op.fail(
      new UnexpectedError({
        cause: new Error(
          `Site refresh job ${job.id} ended as ${completedJob.state}`,
        ),
      }),
    ).orThrow();
    return;
  }

  await pause(700);

  const result = completedJob.result;
  const completed = await op.complete(result).orThrow();

  await client.publishSitesRefreshed({
    refreshId: result.refreshId,
    site: result.site,
    refreshedAt: new Date().toISOString(),
  }).orThrow();
  await pause(700);

  await recordActivity(client, {
    kind: "site-refreshed",
    message: `Refreshed ${result.site.siteName}`,
    relatedSiteId: result.site.siteId,
  });

  return completed;
};
