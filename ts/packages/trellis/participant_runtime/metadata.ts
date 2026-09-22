import type { KvRepresentation } from "../kv.ts";
import type { SchemaLike } from "./api.ts";

export const PARTICIPANT_JOBS_METADATA = Symbol.for(
  "@qlever-llc/trellis/participant/jobs",
);
export const PARTICIPANT_KV_METADATA = Symbol.for(
  "@qlever-llc/trellis/participant/kv",
);
export const PARTICIPANT_STORE_METADATA = Symbol.for(
  "@qlever-llc/trellis/participant/store",
);
export const PARTICIPANT_EVENT_CONSUMERS_METADATA = Symbol.for(
  "@qlever-llc/trellis/participant/event-consumers",
);

export type ParticipantJobsMetadata = Record<string, {
  payload: unknown;
  update?: unknown;
  updateSchema?: SchemaLike;
  result: unknown;
}>;
export type ParticipantKvMetadata = Record<string, {
  required: boolean;
  value: unknown;
  schema: KvRepresentation<unknown>;
}>;
export type ParticipantStoreMetadata = Record<string, { required: boolean }>;
export type TrellisValidationIssueHint = {
  code: string;
  message: string;
  note?: string;
  label?: string;
  i18nKey?: string;
  severity?: "error" | "warning" | "info";
};

export type TrellisValidationExtension = {
  label?: string;
  note?: string;
  uiPath?: string;
  issues?: Record<string, TrellisValidationIssueHint>;
};
