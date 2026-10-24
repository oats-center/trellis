import assert from "node:assert/strict";
import { test } from "node:test";
import { decodeTransferGrant, TransferError } from "./index.ts";

test("untrusted malformed transfer grants return a typed error rather than throwing", () => {
  const cyclic: { self?: unknown } = {};
  cyclic.self = cyclic;
  for (const value of [undefined, null, {}, "not a grant", cyclic]) {
    const decoded = decodeTransferGrant(value);
    assert(decoded.isErr());
    assert(decoded.error instanceof TransferError);
  }
});
