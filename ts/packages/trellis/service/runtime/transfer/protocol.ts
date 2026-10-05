import type { StoreInfo } from "../../../store.ts";
import type { FileInfo } from "../../../transfer.ts";

/** Project backend-neutral store metadata into the public file surface. */
export function fileInfoFromStoreInfo(info: StoreInfo): FileInfo {
  return {
    key: info.key,
    size: info.size,
    updatedAt: info.updatedAt,
    ...(info.digest ? { digest: info.digest.replace(/=+$/, "") } : {}),
    ...(info.contentType ? { contentType: info.contentType } : {}),
    metadata: info.metadata,
  };
}
