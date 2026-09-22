import { sha256 } from "@noble/hashes/sha256";
import type { Subscription } from "@nats-io/nats-core";

import type { PermissionAtom } from "../../../participant_runtime/api.ts";
import type { TypedStore } from "../../../store.ts";
import type { FileInfo } from "../../../transfer.ts";

export type DownloadSession = {
  kind: "download";
  subject: string;
  transferId: string;
  sessionKey: string;
  permission: PermissionAtom;
  requiredCapabilities: readonly string[];
  inboxPrefix: string;
  expiresAtMs: number;
  store: TypedStore;
  key: string;
  info: FileInfo;
  subscription: Subscription;
  timeoutId: ReturnType<typeof setTimeout>;
  reader?: ReadableStreamDefaultReader<Uint8Array>;
  pending?: Uint8Array;
  pendingOffset: number;
  nextSeq: number;
  sentBytes: number;
  hasher: ReturnType<typeof sha256.create>;
  cancellation: AbortController;
};
