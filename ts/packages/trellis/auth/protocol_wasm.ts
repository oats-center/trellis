import init, {
  initSync,
  type SyncInitInput,
  type VerifiedSessionAuthorityHandle as WasmSessionAuthorityHandle,
} from "./protocol_wasm/trellis_protocol_wasm.js";
import * as protocolWasmModule from "./protocol_wasm/trellis_protocol_wasm.js";
import { PROTOCOL_WASM_BASE64 } from "./protocol_wasm/trellis_protocol_wasm_bytes.ts";
import { base64urlDecode, type JsonValue } from "./utils.ts";

type JsonObject = { [key: string]: JsonValue };

const protocolWasm = protocolWasmModule;

/** Bounds and explicit clock accepted by the shared session-authority verifier. */
export type SessionAuthorityVerificationPolicy = {
  nowUnixSeconds: number;
  allowedClockSkewSeconds: number;
  maximumAuthorityLifetimeSeconds: number;
  maximumAuthorityBytes: number;
  maximumEntries: number;
};

/** Caller-supplied pinned key or key authenticated by a pinned rotation chain. */
export type AuthorityIssuerKey = {
  keyId: string;
  publicKey: string;
  state: "active" | "retired" | "revoked";
};

/** Finite administrative privileges, separate from ordinary capabilities. */
export type PlatformPrivilege =
  | "principals.manage"
  | "roles.manage"
  | "apis.accept"
  | "apis.forceReplace"
  | "clients.manage"
  | "privileges.manage";

/** Explicit trust scope and logical-session state for authority verification. */
export type SessionAuthorityVerificationInput = {
  issuer: AuthorityIssuerKey;
  trellisInstanceId: string;
  audienceNatsAccount: string;
  policy: SessionAuthorityVerificationPolicy;
  purpose: "live" | "historicalEvent";
  revocationCutoff: number | null;
};

/** Exact route identity bound by the Rust-owned request transcript. */
export type SessionRequest = {
  authorityDigest: string;
  apiId: string;
  apiGeneration: string;
  acceptedRevision: string;
  action: string;
  subject: string;
  replySubject: string | null;
  requestId: string;
  issuedAt: number;
};

/** Stable error categories returned by local authorization verification. */
export type AuthorizationVerificationErrorCode =
  | "InvalidInput"
  | "SerializationError"
  | "InvalidFormat"
  | "UnsafeJsonInteger"
  | "InvalidEncoding"
  | "InvalidPublicKey"
  | "InvalidKeyId"
  | "InvalidSignature"
  | "UnknownCriticalExtension"
  | "NonCanonicalSet"
  | "InvalidValidityWindow"
  | "IssuerRevoked"
  | "IssuerRetired"
  | "HistoricalContext"
  | "ContextNotYetValid"
  | "ContextExpired"
  | "ContextLifetimeExceeded"
  | "InvalidSessionKey"
  | "PermissionDenied"
  | "ScopeMismatch"
  | "SessionRevoked"
  | "ContextTooLarge"
  | "ProofIatOutOfRange"
  | "InvalidRequestProof"
  | "ReplySubjectMismatch"
  | "InvalidEventTime"
  | "InvalidEventProof"
  | "EventRevoked";

/** Stable structured local authorization failure. */
export type AuthorizationVerificationError = {
  code: AuthorizationVerificationErrorCode;
  path: string;
};

/** Auth-verified client or provisioned deployment binding. */
export type SessionBinding =
  | { kind: "browser"; clientId: string; origin: string }
  | { kind: "native"; clientId: string; durableDpopJkt: string }
  | {
    kind: "service" | "device";
    deploymentId: string;
    instanceId: string;
    participantId: string;
  };

/** Approved whole-capability identity, with canonical decimal counters. */
export type CapabilityAuthority = {
  capabilityId: string;
  identityGeneration: string;
  consentRevision: string;
};

/** Accepted API stamp captured by the signed authority. */
export type AuthorityApi = {
  apiId: string;
  generation: string;
  acceptedRevision: string;
  catalogSnapshotDigest: string;
};

/** Rust-owned authenticated identity projection; action authorization is separate. */
export type AuthenticatedCaller = {
  authorityDigest: string;
  principalId: string;
  principalKind: "user" | "service" | "device";
  binding: SessionBinding;
  authorizationSessionId: string;
  runtimeId: string;
  sessionPublicKey: string;
  inboxPrefix: string;
  loginSessionId?: string;
  oauthGrantId?: string;
  capabilities: CapabilityAuthority[];
  apis: AuthorityApi[];
  platformPrivileges: PlatformPrivilege[];
};

/** Cryptographically authenticated request binding, before dispatch authorization. */
export type VerifySessionRequestResult =
  | {
    ok: true;
    authorityDigest: string;
    requestProofDigest: string;
    caller: AuthenticatedCaller;
  }
  | { ok: false; error: AuthorizationVerificationError };

/** Received bytes and actual route metadata, not a payload-supplied hash. */
export type VerifySessionRequestArgs = {
  authorityHandle: SessionAuthorityHandle;
  request: SessionRequest;
  payload: Uint8Array;
  proof: string;
  policy: SessionAuthorityVerificationPolicy;
  knownRevoked: boolean;
};

/**
 * Exact signed NATS transport authorization bound into a context.
 *
 * Subject patterns use the NATS grammar: literal tokens, `*` matching exactly
 * one token, and a terminal `>` matching one or more trailing tokens.
 */
export type TransportAuthorizationV1 = {
  /** Wire format; always {@link TRANSPORT_AUTHORIZATION_FORMAT_V1}. */
  format: string;
  /** Target NATS account public id the policy applies to. */
  account: string;
  /** Subject patterns the attachment may publish. */
  publishAllow: string[];
  /** Subject patterns the attachment may subscribe to. */
  subscribeAllow: string[];
  /** Bounded response allowance, or null when responses are not permitted. */
  response: { maxMessages: number; ttlMs: number } | null;
  /** Exclusive Unix-seconds hard deadline, or null when unbounded. */
  hardExpiresAt: number | null;
};

/** Wire format identifier for {@link TransportAuthorizationV1}. */
export const TRANSPORT_AUTHORIZATION_FORMAT_V1 =
  "trellis.transport-authorization.v1";

/** Result of classifying admitted transport policy against currently allowed. */
export type TransportPolicyClass =
  | "current"
  | "upgrade_available"
  | "reduction_required";

/** Immutable signed material authenticated by the Rust-owned parser/verifier. */
export type VerifiedSessionAuthorityProjection = {
  authorityDigest: string;
  authority: JsonObject;
};

/** Opaque shared verification state for one immutable session authority. */
export type SessionAuthorityHandle = WasmSessionAuthorityHandle;

let initialized: Promise<void> | undefined;
let initializedSync = false;

async function wasmBytes(): Promise<Uint8Array> {
  const url = new URL(
    "./protocol_wasm/trellis_protocol_wasm_bg.wasm",
    import.meta.url,
  );
  if (url.protocol === "file:") {
    const { readFile } = await import("node:fs/promises");
    return new Uint8Array(await readFile(url));
  }
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(
      `authorization protocol WASM returned HTTP ${response.status}`,
    );
  }
  return new Uint8Array(await response.arrayBuffer());
}

/** Initialize the shared Rust protocol implementation before asynchronous use. */
export async function initializeProtocolWasm(): Promise<void> {
  initialized ??= (async () => {
    await init({ module_or_path: await wasmBytes() });
  })();
  await initialized;
}

/** Initialize the shared Rust protocol implementation before synchronous use. */
export function initializeProtocolWasmSync(): void {
  if (initializedSync) return;
  const url = new URL(
    "./protocol_wasm/trellis_protocol_wasm_bg.wasm",
    import.meta.url,
  );
  const runtime = globalThis as Record<string, unknown>;
  const process = runtime["pro" + "cess"] as
    | {
      versions?: { node?: string };
      getBuiltinModule?: (name: string) => {
        readFileSync(path: URL): Uint8Array;
      };
    }
    | undefined;
  if (process?.versions?.node && process.getBuiltinModule) {
    initSync({
      module: process.getBuiltinModule("node:fs").readFileSync(
        url,
      ) as SyncInitInput,
    });
    initializedSync = true;
    return;
  }
  const binary = atob(PROTOCOL_WASM_BASE64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  initSync({ module: bytes as SyncInitInput });
  initializedSync = true;
}

/** Derive an API-qualified event subject through the shared Rust protocol. */
export async function eventSubjectWasm(
  apiId: string,
  action: string,
): Promise<string> {
  await initializeProtocolWasm();
  return protocolWasm.event_subject(apiId, action);
}

/** Compute the shared query binding for an opaque pagination cursor. */
export async function paginationQueryDigest(
  endpoint: string,
  query: unknown,
): Promise<string> {
  await initializeProtocolWasm();
  return protocolWasm.pagination_query_digest(endpoint, JSON.stringify(query));
}

/** Encode a value with the shared versioned pagination cursor codec. */
export async function encodePaginationCursor(
  queryDigest: string,
  after: unknown,
): Promise<string> {
  await initializeProtocolWasm();
  return protocolWasm.encode_pagination_cursor(
    queryDigest,
    JSON.stringify(after),
  );
}

/** Decode and query-bind a value with the shared pagination cursor codec. */
export async function decodePaginationCursor<T>(
  cursor: string,
  queryDigest: string,
): Promise<T> {
  await initializeProtocolWasm();
  return JSON.parse(
    protocolWasm.decode_pagination_cursor(cursor, queryDigest),
  ) as T;
}

/** Verify bounded target authority under the configured pinned trust scope. */
export async function createSessionAuthorityHandleWasm(args: {
  verification: SessionAuthorityVerificationInput;
  authorityBytes: Uint8Array;
}): Promise<{
  handle: SessionAuthorityHandle;
  verified: VerifiedSessionAuthorityProjection;
}> {
  await initializeProtocolWasm();
  const handle = protocolWasm.create_session_authority_handle(
    JSON.stringify(args.verification),
    args.authorityBytes,
  );
  try {
    const verified = JSON.parse(
      handle.projection(),
    ) as VerifiedSessionAuthorityProjection;
    return { handle, verified };
  } catch (error) {
    handle.free();
    throw error;
  }
}

/** Recheck time and sticky session revocation after any asynchronous resolution. */
export function assertSessionAuthorityHandleCurrentWasm(
  handle: SessionAuthorityHandle,
  policy: SessionAuthorityVerificationPolicy,
  knownRevoked: boolean,
): void {
  handle.assert_current(JSON.stringify(policy), knownRevoked);
}

/**
 * Canonical digest of a signed transport-authorization policy.
 *
 * This is the same base64url SHA-256 digest the runtime binds into a context,
 * so browser and raw NATS consumers can confirm they hold the exact policy.
 */
export async function transportAuthorizationDigestWasm(
  policy: TransportAuthorizationV1,
): Promise<string> {
  await initializeProtocolWasm();
  return protocolWasm.transport_authorization_digest(JSON.stringify(policy));
}

/**
 * Classify admitted transport policy `A` against currently allowed policy `D`.
 *
 * Uses the shared Rust/WASM full-witness inclusion implementation rather than
 * reimplementing wildcard containment in TypeScript, so both SDKs agree.
 */
export async function classifyTransportAuthorizationWasm(
  admitted: TransportAuthorizationV1,
  allowed: TransportAuthorizationV1,
  nowUnixSeconds: number,
): Promise<TransportPolicyClass> {
  await initializeProtocolWasm();
  return JSON.parse(
    protocolWasm.classify_transport_authorization(
      JSON.stringify(admitted),
      JSON.stringify(allowed),
      nowUnixSeconds,
    ),
  ) as TransportPolicyClass;
}

/** Authenticate exact request bytes and captured API identity through Rust/WASM. */
export async function verifySessionRequestWasm(
  args: VerifySessionRequestArgs,
): Promise<VerifySessionRequestResult> {
  await initializeProtocolWasm();
  const { authorityHandle, payload, ...input } = args;
  return JSON.parse(
    protocolWasm.verify_session_request(
      authorityHandle,
      JSON.stringify(input),
      payload,
    ),
  ) as VerifySessionRequestResult;
}

/** Wire reason a live observation ended. */
export type LiveEndReasonWire =
  | "complete"
  | "cancelled"
  | "local_shutdown"
  | "setup_timeout"
  | "peer_lost"
  | "disconnected"
  | "authorization_lost"
  | "binding_changed"
  | "consumer_slow"
  | "delivery_gap"
  | "source_error"
  | "protocol_error"
  | "resource_exhausted";

/** Closed wire error taxonomy for live protocol messages. */
export type LiveErrorCodeWire = string;

/** One parsed provider data-channel frame projection. */
export type LiveFrameWire =
  | {
    format: string;
    type: "data";
    sessionId: string;
    seq: string;
    value: unknown;
  }
  | {
    format: string;
    type: "challenge";
    sessionId: string;
    challengeId: string;
    lastSentSeq: string;
  }
  | {
    format: string;
    type: "end";
    sessionId: string;
    finalSeq: string;
    terminal: {
      reason: LiveEndReasonWire;
      error: { code: string; message: string; traceId?: string } | null;
    };
  };

/** One parsed consumer control projection. */
export type LiveControlWire = {
  format: string;
  type: "control";
  sessionId: string;
  controlSeq: string;
  action: "activate" | "pulse" | "ack" | "close" | "end-ack";
  reason?: string;
  challengeId?: string;
  finalSeq?: string;
  receivedSeq?: string;
  consumedSeq?: string;
};

/** One strict signed provider offer projection. */
export type LiveOfferWire = {
  format: string;
  type: "offer";
  kind: "standalone" | "operation";
  openId: string;
  requestId: string;
  sessionId: string;
  baseSubject: string;
  dataSubject: string;
  controlSubject: string;
  provider: {
    connectionId: string;
    sessionKey: string;
    principalId: string;
    participantId: string;
    deploymentId: string;
    instanceId: string;
  };
  consumer: {
    connectionId: string;
    sessionKey: string;
    principalId: string;
    participantId: string;
  };
  limits: {
    maxDataBodyBytes: number;
    windowFrames: number;
    windowBytes: number;
    reservationMs: number;
    heartbeatIntervalMs: number;
    peerInactivityMs: number;
    consumerStallMs: number;
  };
};

/** One wire terminal envelope inside a control response. */
export type LiveWireTerminal = {
  reason: LiveEndReasonWire;
  error: { code: string; message: string; traceId?: string } | null;
};

/** One strict signed control response projection. */
export type LiveControlResponse =
  | {
    kind: "ack";
    body: {
      format: string;
      type: "control-ack";
      sessionId: string;
      controlSeq: string;
      requestId: string;
      action: "activate" | "pulse" | "ack" | "close" | "end-ack";
      state: "activating" | "active" | "closed";
      acceptedReceivedSeq: string;
      acceptedConsumedSeq: string;
      terminal: LiveWireTerminal | null;
      cleanup: "complete" | "incomplete" | null;
    };
  }
  | {
    kind: "error";
    body: {
      format: string;
      type: "control-error";
      sessionId: string;
      controlSeq: string;
      requestId: string;
      code: string;
    };
  };

function parseWasmResult<T>(encoded: string): T {
  const result = JSON.parse(encoded) as { ok: true } & T | {
    ok: false;
    error: { code: string; path: string };
  };
  if (!result.ok) {
    throw new Error(
      `live protocol error ${result.error.code} at ${result.error.path}`,
    );
  }
  return result as T;
}

/** Shared live timing, window and admission constants from the protocol. */
export type LiveConstants = {
  openReservationMs: number;
  controlTimeoutMs: number;
  heartbeatIntervalMs: number;
  challengeRetryMs: number;
  peerInactivityMs: number;
  consumerStallMs: number;
  ackMaxDelayMs: number;
  ackFrameThreshold: number;
  windowFrames: number;
  windowBytes: number;
  maxOpenBodyBytes: number;
  maxControlBodyBytes: number;
  headerReserveBytes: number;
  cleanupGraceMs: number;
  closeExchangeMs: number;
  closeRetryMs: number;
  tombstoneMs: number;
  maxProviderSessions: number;
  maxProviderSessionsPerCaller: number;
  maxConsumerSessions: number;
  maxTombstones: number;
};

let cachedLiveConstants: LiveConstants | undefined;

function materializeLiveConstants(): LiveConstants {
  initializeProtocolWasmSync();
  cachedLiveConstants ??= JSON.parse(
    protocolWasm.live_constants(),
  ) as LiveConstants;
  return cachedLiveConstants;
}

/** Return the shared live constants generated from the Rust protocol.
 *
 * The protocol WASM is materialized on first property access so a module that
 * captures these constants at import time does not perform WASM I/O until a
 * constant is actually read.
 */
export function liveConstants(): LiveConstants {
  return new Proxy({} as LiveConstants, {
    get(_target, property) {
      return materializeLiveConstants()[property as keyof LiveConstants];
    },
  });
}

/** Compute the negotiated DATA body limit through the shared protocol. */
export function liveNegotiateMaxDataBodyBytes(
  consumer: number,
  provider: number,
): number {
  initializeProtocolWasmSync();
  return protocolWasm.live_negotiate_max_data_body_bytes(consumer, provider);
}

/** Generate one canonical nonce from the shared Rust RNG. */
export function liveGenerateNonce(): string {
  initializeProtocolWasmSync();
  return protocolWasm.live_generate_nonce();
}

/** Derive the exact live delivery subject through the shared protocol. */
export function liveDataSubject(
  providerConnectionId: string,
  consumerConnectionId: string,
  sessionId: string,
): string {
  initializeProtocolWasmSync();
  return protocolWasm.live_data_subject(
    providerConnectionId,
    consumerConnectionId,
    sessionId,
  );
}

/** Derive the exact owner-directed control subject through the shared protocol. */
export function liveObserveSubject(
  baseSubject: string,
  providerConnectionId: string,
  sessionId: string,
): string {
  initializeProtocolWasmSync();
  return protocolWasm.live_observe_subject(
    baseSubject,
    providerConnectionId,
    sessionId,
  );
}

/** Derive the nonqueued owner-control subscription through the shared protocol. */
export function liveObserveWildcardSubject(
  baseSubject: string,
  providerConnectionId: string,
): string {
  initializeProtocolWasmSync();
  return protocolWasm.live_observe_wildcard_subject(
    baseSubject,
    providerConnectionId,
  );
}

/** Validate one subject as a canonical live-session route. */
export function liveValidateSubject(subject: string): void {
  initializeProtocolWasmSync();
  protocolWasm.live_validate_subject(subject);
}

/** Parse one canonical unsigned 64-bit wire counter. */
export function liveParseU64s(value: string): number {
  initializeProtocolWasmSync();
  return protocolWasm.live_parse_u64s(value);
}

/** Parse one strict JSON consumer control through the shared protocol. */
export function liveParseControl(raw: Uint8Array): LiveControlWire {
  initializeProtocolWasmSync();
  return JSON.parse(protocolWasm.live_parse_control(raw)) as LiveControlWire;
}

/** Parse one strict JSON provider data-channel frame through the shared protocol.
 *
 * `maxDataBodyBytes` is the negotiated application-data body limit; DATA is
 * bounded by it while CHALLENGE/END use the tighter protocol-control limit.
 */
export function liveParseFrame(
  raw: Uint8Array,
  maxDataBodyBytes: number,
): LiveFrameWire {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocolWasm.live_parse_frame(raw, maxDataBodyBytes),
  ) as LiveFrameWire;
}

/** Parse one strict signed provider offer through the shared protocol. */
export function liveParseOffer(raw: Uint8Array): LiveOfferWire {
  initializeProtocolWasmSync();
  return JSON.parse(protocolWasm.live_parse_offer(raw)) as LiveOfferWire;
}

/** Parse one strict signed control response through the shared protocol. */
export function liveParseControlResponse(
  raw: Uint8Array,
): LiveControlResponse {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocolWasm.live_parse_control_response(raw),
  ) as LiveControlResponse;
}

/** Compute the canonical logical-open hash through the shared protocol. */
export function liveLogicalOpenHash(identity: object): string {
  initializeProtocolWasmSync();
  return protocolWasm.live_logical_open_hash(JSON.stringify(identity));
}

/** Compute the canonical logical-control hash through the shared protocol. */
export function liveLogicalControlHash(raw: Uint8Array): string {
  initializeProtocolWasmSync();
  return protocolWasm.live_logical_control_hash(raw);
}

/** Build the provider server-message proof digest over exact transmitted bytes. */
export function liveServerProofDigest(
  contextDigest: string,
  subject: string,
  rawBody: Uint8Array,
): Uint8Array {
  initializeProtocolWasmSync();
  return base64urlDecode(
    protocolWasm.live_server_proof_digest(contextDigest, subject, rawBody),
  );
}

/** Verify one provider server-message proof against the pinned provider key. */
export function liveVerifyServerProof(
  proof: string,
  contextDigest: string,
  subject: string,
  rawBody: Uint8Array,
  providerKey: string,
): void {
  initializeProtocolWasmSync();
  protocolWasm.live_verify_server_proof(
    proof,
    contextDigest,
    subject,
    rawBody,
    providerKey,
  );
}

/** Canonical Transfer session subject family. */
export type TransferSubjectKind =
  | "upload-data"
  | "download-data"
  | "control"
  | "signal";

/** Logical final object declaration authenticated by completion or EOF. */
export type TransferTerminalWire = {
  finalSeq: string;
  size: number;
  digest: string;
};

/** Compact authenticated identity; file bytes remain the raw NATS payload. */
export type TransferFrameDescriptor = {
  transferId: string;
  direction: "send" | "receive";
  sequence: string;
  kind: "data" | "complete" | "eof" | "control" | "signal";
  terminal?: TransferTerminalWire;
};

/** Shared caller-control prefix. All counters remain decimal strings. */
type TransferControlBase = {
  format: "trellis.transfer.v2";
  type: "control";
  transferId: string;
  controlSeq: string;
  receivedSeq: string;
  consumedSeq: string;
};

/** Strict v2 caller controls, parsed and validated by canonical Rust/WASM. */
export type TransferControlWire =
  & TransferControlBase
  & (
    | { action: "activate"; receiveMaxFrameBytes: number }
    | { action: "credit" | "cancel"; consumedBytes: string }
    | { action: "end-ack"; consumedBytes: string; finalSeq: string }
  );

/** Logical FileInfo in provider completion, without storage identity. */
export type TransferFileInfoWire = {
  key: string;
  size: number;
  updatedAt: string;
  digest: string;
  contentType?: string;
  metadata: Record<string, string>;
};

/** Logical provider or consumer identity pinned by the grant. */
export type TransferGrantIdentityWire = {
  connectionId: string;
  sessionKey: string;
};

/** Shared public grant coordinates, with no physical backend identifiers. */
type TransferGrantBaseWire = {
  format: "trellis.transfer.v2";
  type: "TransferGrant";
  service: string;
  transferId: string;
  expiresAt: string;
  provider: TransferGrantIdentityWire;
  consumer: TransferGrantIdentityWire;
  dataSubject: string;
  controlSubject: string;
  signalSubject: string;
  maxFrameBytes: number;
  windowFrames: number;
  windowBytes: number;
};

/** Prepared caller-to-provider session, matching the public send grant shape. */
export type TransferSendGrantWire = TransferGrantBaseWire & {
  direction: "send";
  maxBytes?: number;
  contentType?: string;
  metadata?: Record<string, string>;
};

/** Prepared provider-to-caller session, matching the public receive grant shape. */
export type TransferReceiveGrantWire = TransferGrantBaseWire & {
  direction: "receive";
  info: TransferFileInfoWire;
};

/** Canonically validated direction-specific public grant. */
export type TransferGrantWire =
  | TransferSendGrantWire
  | TransferReceiveGrantWire;

/** Canonical terminal error categories, not physical backend diagnostics. */
export type TransferErrorCode =
  | "invalid_request"
  | "permission_denied"
  | "expired"
  | "authorization_lost"
  | "disconnected"
  | "delivery_gap"
  | "window_exceeded"
  | "payload_too_large"
  | "integrity_failed"
  | "storage_failed"
  | "commit_failed"
  | "closed"
  | "not_found"
  | "resource_exhausted"
  | "control_conflict"
  | "control_gap"
  | "cancelled";

/** Strict provider signals; consumed credit is distinct from durable commit. */
export type TransferSignalWire =
  & {
    format: "trellis.transfer.v2";
    transferId: string;
  }
  & (
    | {
      type: "activated";
      controlSeq: string;
      requestId: string;
      maxFrameBytes: number;
      windowFrames: number;
      windowBytes: number;
    }
    | {
      type: "credit";
      receivedSeq: string;
      consumedSeq: string;
      consumedBytes: string;
    }
    | { type: "committed"; finalSeq: string; info: TransferFileInfoWire }
    | { type: "cancelled" }
    | { type: "error"; code: TransferErrorCode }
  );

/** Ordered upload completion on the same subject as upload DATA. */
export type TransferCompleteWire = TransferTerminalWire & {
  format: "trellis.transfer.v2";
  type: "complete";
  transferId: string;
};

/** Canonical protocol limits and header names exported by Rust/WASM. */
export type TransferConstants = {
  format: "trellis.transfer.v2";
  maxFrameBytes: number;
  windowFrames: number;
  windowBytes: number;
  creditFrameStep: number;
  creditByteStep: number;
  creditMaxDelayMs: number;
  headerReserve: number;
  maxControlBytes: number;
  sequenceHeader: string;
  controlHeader: string;
  terminalHeader: string;
  proofHeader: string;
};

/** Read canonical Transfer constants, initializing WASM only on first access. */
export function transferConstants(): TransferConstants {
  let constants: TransferConstants | undefined;
  return new Proxy({} as TransferConstants, {
    get(_target, property) {
      initializeProtocolWasmSync();
      constants ??= JSON.parse(
        protocolWasm.transfer_constants(),
      ) as TransferConstants;
      return constants[property as keyof TransferConstants];
    },
  });
}

/** Generate a canonical random transfer ID. */
export function transferGenerateId(): string {
  initializeProtocolWasmSync();
  return protocolWasm.transfer_generate_id();
}

/** Derive an exact session subject from both logical endpoint identities. */
export function transferSubject(
  kind: TransferSubjectKind,
  provider: string,
  consumer: string,
  transferId: string,
): string {
  initializeProtocolWasmSync();
  return protocolWasm.transfer_subject(kind, provider, consumer, transferId);
}

/** Validate an exact subject and return its canonical family. */
export function transferValidateSubject(subject: string): TransferSubjectKind {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocolWasm.transfer_validate_subject(subject),
  ) as TransferSubjectKind;
}

/** Parse a wire counter as bigint without a lossy number conversion. */
export function transferParseCounter(value: string): bigint {
  initializeProtocolWasmSync();
  return BigInt(protocolWasm.transfer_parse_counter(value));
}

/** Negotiate usable DATA capacity from both actual NATS payload limits. */
export function transferNegotiateMaxFrameBytes(
  consumer: number,
  provider: number,
): number {
  initializeProtocolWasmSync();
  return protocolWasm.transfer_negotiate_max_frame_bytes(consumer, provider);
}

/** Validate a direction-specific public grant before any transport acquisition. */
export function transferParseGrant(raw: Uint8Array): TransferGrantWire {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocolWasm.transfer_parse_grant(raw),
  ) as TransferGrantWire;
}

/** Strictly parse and validate a caller control through Rust/WASM. */
export function transferParseControl(raw: Uint8Array): TransferControlWire {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocolWasm.transfer_parse_control(raw),
  ) as TransferControlWire;
}

/** Strictly parse and validate a provider signal through Rust/WASM. */
export function transferParseSignal(raw: Uint8Array): TransferSignalWire {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocolWasm.transfer_parse_signal(raw),
  ) as TransferSignalWire;
}

/** Strictly parse ordered upload completion through Rust/WASM. */
export function transferParseComplete(raw: Uint8Array): TransferCompleteWire {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocolWasm.transfer_parse_complete(raw),
  ) as TransferCompleteWire;
}

/** Strictly parse zero-byte EOF's authenticated terminal header. */
export function transferParseTerminal(raw: Uint8Array): TransferTerminalWire {
  initializeProtocolWasmSync();
  return JSON.parse(
    protocolWasm.transfer_parse_terminal(raw),
  ) as TransferTerminalWire;
}

/** Return the compact 32-byte request-proof payload; never copies file bytes into it. */
export function transferFrameDigest(
  descriptor: TransferFrameDescriptor,
  payload: Uint8Array,
): Uint8Array {
  initializeProtocolWasmSync();
  return base64urlDecode(
    protocolWasm.transfer_frame_digest(JSON.stringify(descriptor), payload),
  );
}

/** Return the Transfer-only provider signature digest over actual subject and bytes. */
export function transferServerProofDigest(
  contextDigest: string,
  subject: string,
  descriptor: TransferFrameDescriptor,
  payload: Uint8Array,
): Uint8Array {
  initializeProtocolWasmSync();
  return base64urlDecode(
    protocolWasm.transfer_server_proof_digest(
      contextDigest,
      subject,
      JSON.stringify(descriptor),
      payload,
    ),
  );
}

/** Verify the provider proof against its pinned session key and current context. */
export function transferVerifyServerProof(
  proof: string,
  contextDigest: string,
  subject: string,
  descriptor: TransferFrameDescriptor,
  payload: Uint8Array,
  providerKey: string,
): void {
  initializeProtocolWasmSync();
  protocolWasm.transfer_verify_server_proof(
    proof,
    contextDigest,
    subject,
    JSON.stringify(descriptor),
    payload,
    providerKey,
  );
}
