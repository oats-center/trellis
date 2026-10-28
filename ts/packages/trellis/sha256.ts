import { sha256 as portableSha256 } from "@noble/hashes/sha256";

/** Incremental SHA-256; every backend produces identical digests. @internal */
export type IncrementalSha256 = {
  update(data: Uint8Array): void;
  digest(): Uint8Array;
};

// `node:crypto` is reachable synchronously on Node, Deno and Bun through
// `process.getBuiltinModule`. Browsers have no incremental WebCrypto digest, so
// they keep the portable implementation. This is a runtime capability choice;
// both backends compute the same SHA-256.
const nodeCrypto =
  typeof process === "undefined"
    ? undefined
    : process.getBuiltinModule?.("node:crypto");

/** Whether the native `node:crypto` backend is available. @internal */
export const nativeSha256 = nodeCrypto !== undefined;

/** Start one incremental SHA-256 with the fastest available backend. @internal */
export function createSha256(): IncrementalSha256 {
  if (nodeCrypto) {
    const hash = nodeCrypto.createHash("sha256");
    return {
      update(data) {
        hash.update(data);
      },
      digest() {
        return hash.digest();
      },
    };
  }
  const hash = portableSha256.create();
  return {
    update(data) {
      hash.update(data);
    },
    digest() {
      return hash.digest();
    },
  };
}
