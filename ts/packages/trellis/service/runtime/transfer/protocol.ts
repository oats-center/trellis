import { Result, type Result as ResultType } from "@qlever-llc/result";
import {
  headers as natsHeaders,
  type Msg,
  type NatsConnection,
} from "@nats-io/nats-core";

import { TransferError } from "../../../errors/TransferError.ts";
import type { StoreInfo } from "../../../store.ts";
import type { FileInfo } from "../../../transfer.ts";

export const UPLOAD_SUBJECT_PREFIX = "transfer.v1.upload";
export const DOWNLOAD_SUBJECT_PREFIX = "transfer.v1.download";
export const TRANSFER_SEQUENCE_HEADER = "trellis-transfer-seq";
export const TRANSFER_EOF_HEADER = "trellis-transfer-eof";
export const TRANSFER_CONTROL_HEADER = "trellis-transfer-control";
export const DEFAULT_TRANSFER_CHUNK_BYTES = 256 * 1024;

export function fileInfoFromStoreInfo(info: StoreInfo): FileInfo {
  return {
    key: info.key,
    size: info.size,
    updatedAt: info.updatedAt,
    ...(info.digest ? { digest: info.digest } : {}),
    ...(info.contentType ? { contentType: info.contentType } : {}),
    metadata: info.metadata,
  };
}

export function replyError(msg: Msg, error: TransferError): void {
  const headers = natsHeaders();
  headers.set("status", "error");
  msg.respond(JSON.stringify(error.toSerializable()), { headers });
}

export function publishError(
  nc: NatsConnection,
  subject: string,
  error: TransferError,
): void {
  const headers = natsHeaders();
  headers.set("status", "error");
  nc.publish(subject, JSON.stringify(error.toSerializable()), { headers });
}

export function parseSeq(msg: Msg): ResultType<number, TransferError> {
  const raw = msg.headers?.get(TRANSFER_SEQUENCE_HEADER);
  if (!raw) {
    return Result.err(
      new TransferError({
        operation: "transfer",
        context: { reason: "missing_sequence" },
      }),
    );
  }
  const value = Number(raw);
  if (!Number.isInteger(value) || value < 0) {
    return Result.err(
      new TransferError({
        operation: "transfer",
        context: { reason: "invalid_sequence", raw },
      }),
    );
  }
  return Result.ok(value);
}
