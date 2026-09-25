import { assertEquals, assertThrows } from "@std/assert";

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
import vectors from "../../../../integration/fixtures/protocol/live-protocol/vectors.json" with {
  type: "json",
};

type ProofVector = {
  name: string;
  keySeedHex: string;
  contextDigest: string;
  subject: string;
  bodyUtf8: string;
  proof: string;
};

Deno.test("provider server-message proofs match the pinned language-neutral vectors", async () => {
  for (const vector of vectors as ProofVector[]) {
    const keySeed = new Uint8Array(
      (vector.keySeedHex.match(/../g) ?? []).map((byte) =>
        Number.parseInt(byte, 16)
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
    assertEquals(
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
    assertThrows(() =>
      liveVerifyServerProof(
        vector.proof,
        vector.contextDigest,
        `${vector.subject}x`,
        body,
        publicKey,
      )
    );
  }
});

Deno.test("live subject derivation, nonces, and counters agree with Rust", () => {
  const nonce = liveGenerateNonce();
  assertEquals(nonce.length, 22);
  assertEquals(liveParseU64s("0"), 0);
  assertEquals(liveParseU64s("18446744073709551615"), 18446744073709551615);
  for (const invalid of ["", "00", "01", "+1", "-1", "1.0"]) {
    assertThrows(() => liveParseU64s(invalid), Error, undefined, invalid);
  }

  const provider = "provider-connection";
  const consumer = "consumer-connection";
  const dataSubject = liveDataSubject(provider, consumer, nonce);
  assertEquals(
    dataSubject,
    `live.v1.data.${base64urlEncode(utf8(provider))}.${
      base64urlEncode(utf8(consumer))
    }.${nonce}`,
  );
  liveValidateSubject(dataSubject);
  const base = "live.v1.route.YXBpQHYx.ZGVwLTAx.Watch";
  assertEquals(
    liveObserveSubject(base, provider, nonce),
    `${base}.observe.${base64urlEncode(utf8(provider))}.${nonce}`,
  );
  assertEquals(
    liveObserveWildcardSubject(base, provider),
    `${base}.observe.${base64urlEncode(utf8(provider))}.*`,
  );
  assertThrows(() => liveValidateSubject("live.v1.data.*.x.y.z"));
});

Deno.test("live control and frame parsing is strict across the shared bridge", () => {
  const session = base64urlEncode(new Uint8Array(16));
  const control =
    `{"format":"trellis.live.v1","type":"control","sessionId":"${session}","controlSeq":"2","action":"ack","receivedSeq":"4","consumedSeq":"4"}`;
  const parsed = liveParseControl(utf8(control));
  assertEquals(parsed.action, "ack");
  assertEquals(parsed.controlSeq, "2");
  const firstHash = liveLogicalControlHash(utf8(control));
  assertEquals(firstHash, liveLogicalControlHash(utf8(control)));
  assertThrows(() => liveParseControl(utf8(control.replace('"2"', '"02"'))));
  assertThrows(() =>
    liveParseControl(utf8(control.replace('"ack"', '"unknown"')))
  );

  const frame =
    `{"format":"trellis.live.v1","type":"challenge","sessionId":"${session}","challengeId":"${session}","lastSentSeq":"0"}`;
  const parsedFrame = liveParseFrame(utf8(frame), 1_048_576);
  assertEquals(parsedFrame.type, "challenge");
  assertThrows(() =>
    liveParseFrame(
      utf8(frame.replace('"lastSentSeq":"0"', '"lastSentSeq":"00"')),
      1_048_576,
    )
  );
  const end =
    `{"format":"trellis.live.v1","type":"end","sessionId":"${session}","finalSeq":"1","terminal":{"reason":"complete","error":null}}`;
  assertEquals(liveParseFrame(utf8(end), 1_048_576).type, "end");
  assertThrows(() =>
    liveParseFrame(utf8(end.replace('"complete"', '"peer_lost"')), 1_048_576)
  );
});

Deno.test("proof digest is a 32-byte SHA-256 binding of subject and body", () => {
  const contextDigest = base64urlEncode(new Uint8Array(32).fill(9));
  const body = utf8("{}");
  const digest = liveServerProofDigest(contextDigest, "s", body);
  assertEquals(digest.length, 32);
  assertEquals(
    base64urlEncode(digest),
    base64urlEncode(liveServerProofDigest(contextDigest, "s", body)),
  );
  assertThrows(() => liveServerProofDigest("not-a-digest", "s", body));
});
