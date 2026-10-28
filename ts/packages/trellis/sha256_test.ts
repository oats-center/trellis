import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { createSha256, nativeSha256 } from "./sha256.ts";

test("incremental SHA-256 uses node:crypto on Node and matches one-shot digests across chunking", () => {
  assert.equal(nativeSha256, true);
  const body = Uint8Array.from({ length: 3 * 65_536 + 17 }, (_, i) => i % 251);
  for (const chunk of [1, 7, 65_536, body.length]) {
    const hash = createSha256();
    for (let offset = 0; offset < body.length; offset += chunk) {
      hash.update(body.subarray(offset, offset + chunk));
    }
    assert.deepEqual(
      Buffer.from(hash.digest()).toString("hex"),
      createHash("sha256").update(body).digest("hex"),
      `chunk ${chunk}`,
    );
  }
  assert.equal(
    Buffer.from(createSha256().digest()).toString("hex"),
    createHash("sha256").digest("hex"),
  );
});
