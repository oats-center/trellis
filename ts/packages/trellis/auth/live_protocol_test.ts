import assert from "node:assert/strict";
import { test } from "node:test";
// Live control hashing serves requests: load the provider protocol build.
import "./protocol_wasm_provider.ts";

import {
  liveDataSubject,
  liveGenerateNonce,
  liveLogicalControlHash,
  liveObserveSubject,
  liveObserveWildcardSubject,
  liveParseControl,
  liveParseFrame,
  liveParseU64s,
  liveServerProofDigest,
  liveValidateSubject,
  liveVerifyServerProof,
} from "./protocol_wasm.ts";
import { trellisCrypto } from "./crypto.ts";
import { base64urlEncode, utf8 } from "./utils.ts";
import vectors from "../../../../integration/fixtures/protocol/live-protocol/vectors.json" with { type: "json" };

type ProofVector = {
  name: string;
  keySeedHex: string;
  contextDigest: string;
  subject: string;
  bodyUtf8: string;
  proof: string;
};

test("provider server-message proofs match the pinned language-neutral vectors", async () => {
  for (const vector of vectors as ProofVector[]) {
    const keySeed = new Uint8Array(
      (vector.keySeedHex.match(/../g) ?? []).map((byte) =>
        Number.parseInt(byte, 16),
      ),
    );
    const signer = await (await trellisCrypto()).signerFromSeed(keySeed);
    const publicKey = signer.publicKey;
    const body = utf8(vector.bodyUtf8);
    const digest = liveServerProofDigest(
      vector.contextDigest,
      vector.subject,
      body,
    );
    const signature = await signer.sign(digest);
    assert.deepEqual(
      base64urlEncode(signature),
      vector.proof,
      `${vector.name}: TS signature must equal the pinned Rust proof`,
    );
    liveVerifyServerProof(
      vector.proof,
      vector.contextDigest,
      vector.subject,
      body,
      publicKey,
    );
    assert.throws(() =>
      liveVerifyServerProof(
        vector.proof,
        vector.contextDigest,
        `${vector.subject}x`,
        body,
        publicKey,
      ),
    );
  }
});

test("live subject derivation, nonces, and counters agree with Rust", () => {
  const nonce = liveGenerateNonce();
  assert.deepEqual(nonce.length, 22);
  assert.deepEqual(liveParseU64s("0"), 0);
  assert.deepEqual(liveParseU64s("18446744073709551615"), 18446744073709551615);
  for (const invalid of ["", "00", "01", "+1", "-1", "1.0"]) {
    assert.throws(() => liveParseU64s(invalid), Error, invalid);
  }

  const provider = "provider-connection";
  const consumer = "consumer-connection";
  const dataSubject = liveDataSubject(provider, consumer, nonce);
  assert.deepEqual(
    dataSubject,
    `live.v1.data.${base64urlEncode(utf8(provider))}.${base64urlEncode(
      utf8(consumer),
    )}.${nonce}`,
  );
  liveValidateSubject(dataSubject);
  const base = "live.v1.route.YXBpQHYx.ZGVwLTAx.Watch";
  assert.deepEqual(
    liveObserveSubject(base, provider, nonce),
    `${base}.observe.${base64urlEncode(utf8(provider))}.${nonce}`,
  );
  assert.deepEqual(
    liveObserveWildcardSubject(base, provider),
    `${base}.observe.${base64urlEncode(utf8(provider))}.*`,
  );
  assert.throws(() => liveValidateSubject("live.v1.data.*.x.y.z"));
});

test("live control and frame parsing is strict across the shared bridge", () => {
  const session = base64urlEncode(new Uint8Array(16));
  const control = `{"format":"trellis.live.v1","type":"control","sessionId":"${session}","controlSeq":"2","action":"ack","receivedSeq":"4","consumedSeq":"4"}`;
  const parsed = liveParseControl(utf8(control));
  assert.deepEqual(parsed.action, "ack");
  assert.deepEqual(parsed.controlSeq, "2");
  const firstHash = liveLogicalControlHash(utf8(control));
  assert.deepEqual(firstHash, liveLogicalControlHash(utf8(control)));
  assert.throws(() => liveParseControl(utf8(control.replace('"2"', '"02"'))));
  assert.throws(() =>
    liveParseControl(utf8(control.replace('"ack"', '"unknown"'))),
  );

  const frame = `{"format":"trellis.live.v1","type":"challenge","sessionId":"${session}","challengeId":"${session}","lastSentSeq":"0"}`;
  const parsedFrame = liveParseFrame(utf8(frame), 1_048_576);
  assert.deepEqual(parsedFrame.type, "challenge");
  assert.throws(() =>
    liveParseFrame(
      utf8(frame.replace('"lastSentSeq":"0"', '"lastSentSeq":"00"')),
      1_048_576,
    ),
  );
  const end = `{"format":"trellis.live.v1","type":"end","sessionId":"${session}","finalSeq":"1","terminal":{"reason":"complete","error":null}}`;
  assert.deepEqual(liveParseFrame(utf8(end), 1_048_576).type, "end");
  assert.throws(() =>
    liveParseFrame(utf8(end.replace('"complete"', '"peer_lost"')), 1_048_576),
  );
});

test("proof digest is a 32-byte SHA-256 binding of subject and body", () => {
  const contextDigest = base64urlEncode(new Uint8Array(32).fill(9));
  const body = utf8("{}");
  const digest = liveServerProofDigest(contextDigest, "s", body);
  assert.deepEqual(digest.length, 32);
  assert.deepEqual(
    base64urlEncode(digest),
    base64urlEncode(liveServerProofDigest(contextDigest, "s", body)),
  );
  assert.throws(() => liveServerProofDigest("not-a-digest", "s", body));
});
