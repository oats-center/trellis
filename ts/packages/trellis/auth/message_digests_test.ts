import assert from "node:assert/strict";
import { test } from "node:test";

import {
  liveServerProofDigest,
  sessionEventSigningDigest,
  sessionRequestSigningDigest,
  sessionResponseSigningDigest,
  transferFrameDigest,
  transferServerProofDigest,
} from "./message_digests.ts";
import type {
  SessionEvent,
  SessionRequest,
  SessionResponse,
  TransferFrameDescriptor,
} from "./protocol_wasm.ts";
import { base64urlEncode } from "./utils.ts";
import vectors from "../../../../integration/fixtures/protocol/message-digests/vectors.json" with { type: "json" };

type Vector = {
  kind: string;
  payloadHex: string;
  digest: string;
  contextDigest?: string;
  subject?: string;
  descriptor?: TransferFrameDescriptor;
  request?: SessionRequest;
  event?: SessionEvent;
  response?: SessionResponse;
};

const bytes = (hex: string) =>
  Uint8Array.from(hex.match(/../g) ?? [], (byte) => Number.parseInt(byte, 16));

test("native per-message digests equal the pinned Rust protocol vectors", async () => {
  for (const vector of vectors as Vector[]) {
    const payload = bytes(vector.payloadHex);
    const digest =
      vector.kind === "transferFrame"
        ? await transferFrameDigest(vector.descriptor!, payload)
        : vector.kind === "transferServerProof"
          ? await transferServerProofDigest(
              vector.contextDigest!,
              vector.subject!,
              vector.descriptor!,
              payload,
            )
          : vector.kind === "liveServerProof"
            ? await liveServerProofDigest(
                vector.contextDigest!,
                vector.subject!,
                payload,
              )
            : vector.kind === "sessionRequest"
              ? await sessionRequestSigningDigest(vector.request!, payload)
              : vector.kind === "sessionEvent"
                ? await sessionEventSigningDigest(vector.event!, payload)
                : await sessionResponseSigningDigest(vector.response!, payload);
    assert.equal(base64urlEncode(digest), vector.digest, vector.kind);
  }
});

test("a DATA proof digest binds the frame's payload, sequence and subject", async () => {
  const [frame, , server] = vectors as Vector[];
  const payload = bytes(frame.payloadHex);
  const changed = payload.slice();
  changed[0] ^= 1;
  assert.notEqual(
    base64urlEncode(await transferFrameDigest(frame.descriptor!, changed)),
    frame.digest,
  );
  await assert.rejects(
    transferFrameDigest({ ...frame.descriptor!, sequence: "0" }, payload),
  );
  await assert.rejects(
    transferServerProofDigest(
      server.contextDigest!,
      server.subject!.replace("download", "upload"),
      server.descriptor!,
      payload,
    ),
  );
});
