import type {
  AsyncResult,
  ReceiveTransferGrant,
  StoreError,
  TransferError,
} from "@oatscenter/trellis";
import type {
  ConnectedTrellisService,
  RpcHandler,
} from "@oatscenter/trellis/service";
import { type participants } from "../trellis/index.js";
import type { getSiteSummary } from "../../shared/field_data.ts";

export type ReceiveTransferIssuer = {
  createTransfer(args: {
    direction: "receive";
    store: string;
    key: string;
    sessionKey: string;
    expiresInMs?: number;
  }): AsyncResult<ReceiveTransferGrant, TransferError>;
  store?: {
    uploads?: {
      binding?: { ttlMs?: number };
      waitFor?(key: string, options?: {
        timeoutMs?: number;
        pollIntervalMs?: number;
      }): AsyncResult<unknown, StoreError>;
    };
  };
};

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
