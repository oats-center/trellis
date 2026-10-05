import type {
  ConnectedTrellisService,
  RpcHandler,
} from "@oatscenter/trellis/service";
import { type participants } from "../trellis/index.js";
import type { getSiteSummary } from "../../shared/field_data.ts";

export type ReceiveTransferIssuer = Pick<
  FieldOpsService,
  "createTransfer" | "store"
>;

export type ActivityFeedEventNames = {
  auditRecorded: "Audit.Recorded";
  reportsPublished: "Reports.Published";
  evidenceUploaded: "Evidence.Uploaded";
  sitesRefreshed: "Sites.Refreshed";
};

export type FieldOpsDeps = {
  transferIssuer: ReceiveTransferIssuer;
  getSiteSummary: typeof getSiteSummary;
  activityFeedEventNames: ActivityFeedEventNames;
};

export type FieldOpsService = ConnectedTrellisService<
  typeof participants.Service.participant
>;
export type FieldOpsHandlerClient = Parameters<
  RpcHandler<typeof participants.Service.participant, "Assignments.List">
>[0]["client"];
