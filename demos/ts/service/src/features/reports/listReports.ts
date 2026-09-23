import { ok } from "@oats-center/trellis";
import type { RpcHandler } from "@oats-center/trellis/service";
import { type participants } from "../../../trellis/index.js";
import { listReports as listReportRecords } from "./reportStore.ts";
import { paginate } from "../../pagination.ts";

type Handler = RpcHandler<
  typeof participants.Service.participant,
  "Reports.List"
>;

/** Lists completed closeout reports generated during this demo service run. */
export const listReports: Handler = async ({ input }) => {
  return ok(
    await paginate({
      endpoint: "demo.fieldops.Reports.List",
      cursor: input.page?.cursor,
      limit: input.page?.limit,
      rows: listReportRecords(),
      key: (report) => `${report.publishedAt}\0${report.reportId}`,
      compare: (left, right) => left > right ? -1 : left < right ? 1 : 0,
    }),
  );
};
