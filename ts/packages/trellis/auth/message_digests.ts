/**
 * Native constructions of the per-message proof digests.
 *
 * These run on every DATA frame, Live message, request, response and event,
 * so they hash with the host's native SHA-256 instead of crossing into the
 * protocol WASM. Each must stay byte-identical to `trellis-protocol`; the
 * pinned vectors in `integration/fixtures/protocol/message-digests` are
 * checked by both the Rust protocol tests and this SDK's tests. Validation
 * that decides authority stays in the shared protocol: a verifier recomputes
 * every digest there, so a signer here only signs its own messages.
 *
 * @internal
 * @module
 */
import {
  type SessionEvent,
  type SessionRequest,
  type SessionResponse,
  transferConstants,
  type TransferFrameDescriptor,
  transferFrameDigest as sharedTransferFrameDigest,
  transferServerProofDigest as sharedTransferServerProofDigest,
  transferVerifyServerProof as sharedTransferVerifyServerProof,
  verifyProofDigest,
} from "./protocol_wasm.ts";
import { trellisCrypto } from "./crypto.ts";
import { base64urlDecode, base64urlEncode } from "./utils.ts";

const TRANSFER_FRAME_DOMAIN_V1 = "trellis.transfer-frame.v1";
const TRANSFER_SERVER_PROOF_DOMAIN_V1 = "trellis.transfer-server-proof.v1";
const LIVE_SERVER_PROOF_DOMAIN_V1 = "trellis.live-server-proof.v1";
const MESSAGE_REQUEST_DOMAIN_V1 = "trellis.message.request.v1";
const MESSAGE_EVENT_DOMAIN_V1 = "trellis.message.event.v1";
const MESSAGE_RESPONSE_DOMAIN_V1 = "trellis.message.response.v1";
const DOWNLOAD_DATA_PREFIX = "transfer.v1.download.data.";

// Synchronous native hashing on Node, Deno and Bun; WebCrypto elsewhere.
const nodeCrypto =
  typeof process === "undefined"
    ? undefined
    : process.getBuiltinModule?.("node:crypto");
const encoder = new TextEncoder();

async function sha256(parts: readonly Uint8Array[]): Promise<Uint8Array> {
  if (nodeCrypto) {
    const hash = nodeCrypto.createHash("sha256");
    for (const part of parts) hash.update(part);
    return new Uint8Array(hash.digest());
  }
  const joined =
    parts.length === 1 ? parts[0] : concat(parts, totalLength(parts));
  return await (await trellisCrypto()).sha256(joined);
}

function totalLength(parts: readonly Uint8Array[]): number {
  let length = 0;
  for (const part of parts) length += part.length;
  return length;
}

function concat(parts: readonly Uint8Array[], length: number): Uint8Array {
  const out = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

/** Each component as a 4-byte big-endian length followed by its bytes. */
function lengthPrefixed(components: readonly Uint8Array[]): Uint8Array[] {
  const out: Uint8Array[] = [];
  for (const component of components) {
    const length = new Uint8Array(4);
    new DataView(length.buffer).setUint32(0, component.length);
    out.push(length, component);
  }
  return out;
}

function fixed(value: string, length: number, field: string): Uint8Array {
  const bytes = base64urlDecode(value);
  if (bytes.length !== length || base64urlEncode(bytes) !== value) {
    throw new Error(
      `${field} must be a canonical ${length}-byte base64url value`,
    );
  }
  return bytes;
}

function u64(value: string, field: string): Uint8Array {
  if (!/^(0|[1-9][0-9]*)$/.test(value)) {
    throw new Error(`${field} must be a canonical decimal counter`);
  }
  const parsed = BigInt(value);
  if (parsed > 0xffff_ffff_ffff_ffffn) throw new Error(`${field} overflows`);
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, parsed);
  return out;
}

function i64(value: number, field: string): Uint8Array {
  if (!Number.isSafeInteger(value))
    throw new Error(`${field} must be an integer`);
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigInt64(0, BigInt(value));
  return out;
}

function validateDataDescriptor(
  descriptor: TransferFrameDescriptor,
  payload: Uint8Array,
): void {
  fixed(descriptor.transferId, 16, "transferId");
  if (
    descriptor.kind !== "data" ||
    descriptor.terminal !== undefined ||
    (descriptor.direction !== "send" && descriptor.direction !== "receive") ||
    descriptor.sequence === "0" ||
    payload.length === 0 ||
    payload.length > transferConstants().maxFrameBytes
  ) {
    throw new Error("invalid DATA sequence, size or terminal metadata");
  }
}

async function dataFrameDigest(
  descriptor: TransferFrameDescriptor,
  payload: Uint8Array,
): Promise<Uint8Array> {
  validateDataDescriptor(descriptor, payload);
  u64(descriptor.sequence, "sequence");
  // The canonical JSON of a DATA descriptor: four string members in key order.
  const canonical = `{"direction":"${descriptor.direction}","kind":"data","sequence":"${descriptor.sequence}","transferId":"${descriptor.transferId}"}`;
  return await sha256(
    lengthPrefixed([
      encoder.encode(TRANSFER_FRAME_DOMAIN_V1),
      encoder.encode(canonical),
      await sha256([payload]),
    ]),
  );
}

/**
 * Compact digest a Transfer request proof signs: DATA natively, the few
 * control, completion, EOF and signal frames through the shared protocol.
 */
export async function transferFrameDigest(
  descriptor: TransferFrameDescriptor,
  payload: Uint8Array,
): Promise<Uint8Array> {
  if (descriptor.kind !== "data") {
    return sharedTransferFrameDigest(descriptor, payload);
  }
  return await dataFrameDigest(descriptor, payload);
}

/** Digest a provider signs for a download DATA frame or a signal. */
export async function transferServerProofDigest(
  contextDigest: string,
  subject: string,
  descriptor: TransferFrameDescriptor,
  payload: Uint8Array,
): Promise<Uint8Array> {
  if (descriptor.kind !== "data") {
    return sharedTransferServerProofDigest(
      contextDigest,
      subject,
      descriptor,
      payload,
    );
  }
  const tokens = subject.startsWith(DOWNLOAD_DATA_PREFIX)
    ? subject.slice(DOWNLOAD_DATA_PREFIX.length).split(".")
    : [];
  if (
    descriptor.direction !== "receive" ||
    tokens.length !== 3 ||
    tokens[2] !== descriptor.transferId
  ) {
    throw new Error(
      "provider proof must address this transfer's download DATA subject",
    );
  }
  for (const token of tokens.slice(0, 2)) {
    const identity = new TextDecoder("utf-8", { fatal: true }).decode(
      base64urlDecode(token),
    );
    if (
      !/^[A-Za-z0-9_-]+$/.test(token) ||
      base64urlEncode(encoder.encode(identity)) !== token ||
      identity.length === 0 ||
      identity.trim() !== identity
    ) {
      throw new Error("invalid logical identity token");
    }
  }
  const context = fixed(contextDigest, 32, "contextDigest");
  return await sha256(
    lengthPrefixed([
      encoder.encode(TRANSFER_SERVER_PROOF_DOMAIN_V1),
      context,
      encoder.encode(subject),
      await dataFrameDigest(descriptor, payload),
    ]),
  );
}

/** Digest a provider signs for one Live server message. */
export async function liveServerProofDigest(
  contextDigest: string,
  subject: string,
  rawBody: Uint8Array,
): Promise<Uint8Array> {
  return await sha256(
    lengthPrefixed([
      encoder.encode(LIVE_SERVER_PROOF_DOMAIN_V1),
      fixed(contextDigest, 32, "contextDigest"),
      encoder.encode(subject),
      await sha256([rawBody]),
    ]),
  );
}

/** Verify a provider's Live server-message proof over exact bytes. */
export async function liveVerifyServerProof(
  proof: string,
  contextDigest: string,
  subject: string,
  rawBody: Uint8Array,
  providerKey: string,
): Promise<void> {
  verifyProofDigest(
    proof,
    await liveServerProofDigest(contextDigest, subject, rawBody),
    providerKey,
  );
}

/** Verify a provider's proof over one download DATA, EOF or signal frame. */
export async function transferVerifyServerProof(
  proof: string,
  contextDigest: string,
  subject: string,
  descriptor: TransferFrameDescriptor,
  payload: Uint8Array,
  providerKey: string,
): Promise<void> {
  if (descriptor.kind !== "data") {
    sharedTransferVerifyServerProof(
      proof,
      contextDigest,
      subject,
      descriptor,
      payload,
      providerKey,
    );
    return;
  }
  verifyProofDigest(
    proof,
    await transferServerProofDigest(
      contextDigest,
      subject,
      descriptor,
      payload,
    ),
    providerKey,
  );
}

/** SHA-256 of exact payload bytes, for the shared verifiers. */
export async function payloadSha256(payload: Uint8Array): Promise<Uint8Array> {
  return await sha256([payload]);
}

/** Digest a caller signs for one request publication. */
export async function sessionRequestSigningDigest(
  request: SessionRequest,
  payload: Uint8Array,
): Promise<Uint8Array> {
  return await sha256([
    concatAll(
      lengthPrefixed([
        encoder.encode(MESSAGE_REQUEST_DOMAIN_V1),
        fixed(request.authorityDigest, 32, "authorityDigest"),
        encoder.encode(request.apiId),
        u64(request.apiGeneration, "apiGeneration"),
        u64(request.acceptedRevision, "acceptedRevision"),
        encoder.encode(request.action),
        encoder.encode(request.subject),
        encoder.encode(request.replySubject ?? ""),
        encoder.encode(request.requestId),
        i64(request.issuedAt, "issuedAt"),
        await sha256([payload]),
      ]),
    ),
  ]);
}

/** Digest a publisher signs for one event publication. */
export async function sessionEventSigningDigest(
  event: SessionEvent,
  payload: Uint8Array,
): Promise<Uint8Array> {
  return await sha256([
    concatAll(
      lengthPrefixed([
        encoder.encode(MESSAGE_EVENT_DOMAIN_V1),
        fixed(event.authorityDigest, 32, "authorityDigest"),
        encoder.encode(event.apiId),
        u64(event.apiGeneration, "apiGeneration"),
        u64(event.acceptedRevision, "acceptedRevision"),
        encoder.encode(event.action),
        encoder.encode(event.subject),
        encoder.encode(event.eventId),
        encoder.encode(event.eventTime),
        await sha256([payload]),
      ]),
    ),
  ]);
}

/** Digest a provider signs for one response to an opening request. */
export async function sessionResponseSigningDigest(
  response: SessionResponse,
  payload: Uint8Array,
): Promise<Uint8Array> {
  return await sha256([
    concatAll(
      lengthPrefixed([
        encoder.encode(MESSAGE_RESPONSE_DOMAIN_V1),
        encoder.encode(response.trellisInstanceId),
        encoder.encode(response.audienceNatsAccount),
        fixed(response.providerAuthorityDigest, 32, "providerAuthorityDigest"),
        fixed(
          response.providerCertificateDigest,
          32,
          "providerCertificateDigest",
        ),
        encoder.encode(response.apiId),
        u64(response.apiGeneration, "apiGeneration"),
        encoder.encode(response.action),
        fixed(response.requestAuthorityDigest, 32, "requestAuthorityDigest"),
        encoder.encode(response.requestId),
        fixed(response.requestProofDigest, 32, "requestProofDigest"),
        encoder.encode(response.subject),
        encoder.encode(response.kind),
        await sha256([payload]),
        i64(response.issuedAt, "issuedAt"),
      ]),
    ),
  ]);
}

function concatAll(parts: readonly Uint8Array[]): Uint8Array {
  return concat(parts, totalLength(parts));
}
