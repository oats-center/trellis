import type { Subscription } from "@nats-io/nats-core";
import type { PermissionAtom } from "../../../participant_runtime/api.ts";
import type { RuntimeOperationTransferProgress } from "../../../session.ts";
import type { StoreError } from "../../../errors/StoreError.ts";
import type { TransferError } from "../../../errors/TransferError.ts";
import type { TypedStore } from "../../../store.ts";
import type { FileInfo } from "../../../transfer.ts";
import type { AsyncResult } from "@qlever-llc/result";
import type { StoredTransfer } from "../transfer.ts";
import type { AsyncChunkQueue } from "./queue.ts";
import { sha256 } from "@noble/hashes/sha256";

export type UploadSession = {
  kind: "upload";
  subject: string;
  transferId: string;
  sessionKey: string;
  permission: PermissionAtom | undefined;
  requiredCapabilities: readonly string[];
  expiresAtMs: number;
  store: TypedStore;
  key: string;
  maxBytes?: number;
  contentType?: string;
  metadata?: Record<string, string>;
  onProgress?: (
    progress: RuntimeOperationTransferProgress,
  ) => Promise<void> | void;
  onComplete?: (info: FileInfo) => Promise<void> | void;
  onError?: (error: TransferError) => Promise<void> | void;
  onStored?: (stored: StoredTransfer) => Promise<void> | void;
  subscription: Subscription;
  timeoutId: ReturnType<typeof setTimeout>;
  queue: AsyncChunkQueue;
  putPromise: AsyncResult<void, StoreError>;
  cancellation: AbortController;
  committing: boolean;
  nextSeq: number;
  receivedBytes: number;
  hasher: ReturnType<typeof sha256.create>;
};

export function raceCancellation<T>(
  promise: Promise<T>,
  signal: AbortSignal,
): Promise<T> {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise<T>((resolve, reject) => {
    const cancel = () => {
      finish();
      reject(signal.reason);
    };
    const finish = () => signal.removeEventListener("abort", cancel);
    signal.addEventListener("abort", cancel, { once: true });
    promise.then(
      (value) => {
        finish();
        resolve(value);
      },
      (error) => {
        finish();
        reject(error);
      },
    );
  });
}

export function effectiveUploadMaxBytes(
  argsMaxBytes?: number,
  storeMaxObjectBytes?: number,
): number | undefined {
  if (argsMaxBytes === undefined) return storeMaxObjectBytes;
  if (storeMaxObjectBytes === undefined) return argsMaxBytes;
  return Math.min(argsMaxBytes, storeMaxObjectBytes);
}
