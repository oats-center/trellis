import {
  normalizeOperationRecordWasm,
  type AuthenticatedCaller,
} from "../../auth/protocol_wasm.ts";
import type { KvRepresentation } from "../../kv.ts";
import type { JsonValue } from "../../auth/utils.ts";
import type {
  DurableOperationRecord,
  RuntimeOperationSnapshot,
} from "../../session.ts";
import type { SendTransferGrant } from "../../transfer.ts";

// A view of the Rust-owned decoded value, not another schema or validator.
type StoredRecord = {
  invocationId: string;
  invocationDigest: string;
  apiId: string;
  operation: string;
  deploymentId: string;
  creatorPrincipalId: string;
  creatorBinding: AuthenticatedCaller["binding"];
  callerSessionKey: string;
  caller: AuthenticatedCaller | null;
  input: unknown;
  snapshot: RuntimeOperationSnapshot;
  revision: number;
  ownerExecutorId: string | null;
  ownerConnectionId: string | null;
  ownerEpoch: number;
  leaseExpiresAtMs: number | null;
  cancellationRequested: boolean;
  cancellationRequestedAt?: string;
  nextSignalSequence: number;
  updateSequence: number;
  signals: {
    sequence: number;
    requestId: string;
    name: string;
    payload: JsonValue;
    acknowledged: boolean;
    acceptedAt: string;
  }[];
  telemetry?: { traceparent: string; tracestate?: string };
  transfer: null | {
    stagingKey: string;
    state: string;
    grant: SendTransferGrant;
    size?: number;
    digest?: string;
    updatedAt?: string;
    contentType?: string;
    fileInfo?: unknown;
  };
};

/** Private repository adapter. TRKV framing and carrier ownership remain in KV. */
export const operationRecordRepresentation: KvRepresentation<DurableOperationRecord> =
  {
    version: 2,
    migrations: {},
    codec: {
      encode(value) {
        const caller = { ...value.caller };
        Reflect.deleteProperty(caller, "type");
        const grant = value.transferGrant
          ? { ...value.transferGrant }
          : undefined;
        const committed: unknown = grant && Reflect.get(grant, "committed");
        if (grant) Reflect.deleteProperty(grant, "committed");
        const info =
          committed && typeof committed === "object" ? committed : undefined;
        return normalizeOperationRecordWasm({
          invocationId: value.invocationId,
          invocationDigest: value.invocationDigest,
          apiId: value.apiId,
          operation: value.operation,
          deploymentId: value.deploymentId,
          creatorPrincipalId: value.creatorPrincipalId,
          creatorBinding: value.creatorBinding,
          callerSessionKey: value.callerSessionKey,
          caller,
          input: value.input,
          snapshot: { ...value.snapshot, revision: value.revision },
          revision: value.revision,
          ownerExecutorId: value.ownerInstanceId || null,
          ownerConnectionId: value.ownerConnectionId || null,
          ownerEpoch: value.ownerEpoch,
          leaseExpiresAtMs: Date.parse(value.leaseExpiresAt),
          cancellationRequested: Boolean(value.cancelRequestedAt),
          ...(value.cancelRequestedAt
            ? { cancellationRequestedAt: value.cancelRequestedAt }
            : {}),
          nextSignalSequence: value.signalSequence + 1,
          updateSequence: value.sequence,
          signals: value.signals.map((signal) => ({
            sequence: signal.sequence,
            requestId: signal.requestId,
            name: signal.signal,
            payload: signal.input ?? null,
            acknowledged: signal.acknowledged,
            acceptedAt: signal.acceptedAt,
          })),
          ...(value.telemetry ? { telemetry: value.telemetry } : {}),
          transfer: grant
            ? {
                stagingKey:
                  value.uploadStorageKey ??
                  `_trellis-transfer-v2-${grant.transferId}`,
                state: info ? "committed" : "pending",
                grant,
                ...(info
                  ? {
                      fileInfo: info,
                      size: Reflect.get(info, "size"),
                      digest: Reflect.get(info, "digest"),
                      updatedAt: Reflect.get(info, "updatedAt"),
                      contentType: Reflect.get(info, "contentType"),
                    }
                  : {}),
              }
            : null,
        });
      },
      decode(value) {
        const record = normalizeOperationRecordWasm(value) as StoredRecord;
        if (!record.caller)
          throw new Error("operation record has no retained caller");
        const grant = record.transfer
          ? { ...record.transfer.grant }
          : undefined;
        const upload =
          record.transfer?.state === "committed" ? record.transfer : undefined;
        if (grant && upload) {
          Reflect.set(grant, "committed", upload.fileInfo);
        }
        return {
          invocationId: record.invocationId,
          invocationDigest: record.invocationDigest,
          apiId: record.apiId,
          operation: record.operation,
          deploymentId: record.deploymentId,
          creatorPrincipalId: record.creatorPrincipalId,
          creatorBinding: record.creatorBinding,
          callerSessionKey: record.callerSessionKey,
          caller: { ...record.caller, type: "verified" },
          input: record.input,
          snapshot: record.snapshot,
          revision: record.revision,
          ownerInstanceId: record.ownerExecutorId ?? "",
          ownerConnectionId: record.ownerConnectionId ?? "",
          ownerEpoch: record.ownerEpoch,
          leaseExpiresAt: new Date(record.leaseExpiresAtMs ?? 0).toISOString(),
          ...(record.cancellationRequested
            ? { cancelRequestedAt: record.cancellationRequestedAt }
            : {}),
          sequence: record.updateSequence,
          signalSequence: record.nextSignalSequence - 1,
          signals: record.signals.map((signal) => ({
            operationId: record.invocationId,
            sequence: signal.sequence,
            requestId: signal.requestId,
            signal: signal.name,
            input: signal.payload,
            acknowledged: signal.acknowledged,
            acceptedAt: signal.acceptedAt,
          })),
          ...(record.telemetry ? { telemetry: record.telemetry } : {}),
          ...(grant ? { transferGrant: grant } : {}),
          ...(upload ? { uploadStorageKey: upload.stagingKey } : {}),
        };
      },
    },
  };
