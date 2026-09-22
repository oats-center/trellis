import { ASSIGNED_INSPECTIONS } from "../../../../shared/field_data.ts";
import { ok } from "@qlever-llc/trellis";
import type { RpcHandler } from "@qlever-llc/trellis/service";
import { type participants } from "../../../trellis/index.js";
import { paginate } from "../../pagination.ts";

type Handler = RpcHandler<
  typeof participants.Service.participant,
  "Assignments.List"
>;

export const listAssignments: Handler = async ({ input }) => {
  return ok(
    await paginate({
      endpoint: "demo.fieldops.Assignments.List",
      cursor: input.page?.cursor,
      limit: input.page?.limit,
      rows: ASSIGNED_INSPECTIONS,
      key: (assignment) => assignment.inspectionId,
    }),
  );
};
