import init, {
  initSync,
  type SyncInitInput,
  type VerifiedAuthorizationContextHandle as WasmAuthorizationContextHandle,
} from "./protocol_wasm/trellis_protocol_wasm.js";
import * as protocolWasmModule from "./protocol_wasm/trellis_protocol_wasm.js";
import { PROTOCOL_WASM_BASE64 } from "./protocol_wasm/trellis_protocol_wasm_bytes.ts";
import { base64urlDecode, type JsonValue } from "./utils.ts";

type JsonObject = { [key: string]: JsonValue };

const protocolWasm = protocolWasmModule;

/** Verification policy accepted by the Rust authorization protocol. */
export type AuthorizationVerificationPolicy = {
  nowUnixSeconds: number;
  allowedClockSkewSeconds: number;
  maximumContextLifetimeSeconds: number;
  maximumContextBytes: number;
  maximumPermissions: number;
};

/** Context verification policy fields used to schedule refresh. */
export type AuthorizationContextVerificationPolicy =
  & AuthorizationVerificationPolicy
  & {
    refreshLeadSeconds: number;
    refreshJitterSeconds: number;
  };

/** Online issuer entry and signed context supplied to a local verifier. */
export type AuthorizationContextVerificationInput = {
  issuer: AuthorizationIssuerKey;
  context: unknown;
};

/** Public issuer entry authenticated by the configured Trellis origin. */
export type AuthorizationIssuerKey = {
  keyId: string;
  publicKey: string;
  state: "active" | "retired" | "revoked";
};

/** Meta-authority not represented by ordinary permission atoms. */
export type PlatformPrivilege = "trellis.auth::admin";

/** One exact API or participant-resource permission target. */
export type PermissionTarget =
  | {
    kind: "apiSurface";
    api: string;
    surface: "rpc" | "operation" | "event" | "live" | "state";
    name: string;
  }
  | {
    kind: "participantResource";
    participant: string;
    resource: "kv" | "store" | "jobQueue" | "eventConsumer" | "state";
    name: string;
  }
  | {
    kind: "operationSignal";
    api: string;
    operation: string;
    signal: string;
  };

/** One exact machine-enforceable permission atom. */
export type PermissionAtom = {
  target: PermissionTarget;
  action:
    | "call"
    | "invoke"
    | "observe"
    | "cancel"
    | "control"
    | "publish"
    | "subscribe"
    | "read"
    | "write"
    | "delete"
    | "submit"
    | "process"
    | "consume";
};

/** Grant set projection returned by local authorization verification. */
export type GrantSet = {
  format: "trellis.grant-set.v1";
  permissions: PermissionAtom[];
};

/** Verified request caller projection. */
export type VerifiedAuthorizationRequestProjection = { contextDigest: string };

/** Verified event publisher projection. */
export type VerifiedAuthorizationEventPublisher = {
  kind: "user" | "service" | "device";
  deploymentId: string | null;
  instanceId: string | null;
  participantId: string;
  connectionId: string;
  loginSessionId: string | null;
};

/** Verified event publisher projection. */
export type VerifiedAuthorizationEventProjection =
  & VerifiedAuthorizationRequestProjection
  & {
    publisher: VerifiedAuthorizationEventPublisher;
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

/** Result envelope returned by a local request authorization verifier. */
export type VerifyAuthorizationRequestResult =
  | ({ ok: true } & VerifiedAuthorizationRequestProjection)
  | { ok: false; error: AuthorizationVerificationError };

/** Result envelope returned by a local event authorization verifier. */
export type VerifyAuthorizationEventResult =
  | ({ ok: true } & VerifiedAuthorizationEventProjection)
  | { ok: false; error: AuthorizationVerificationError };

/** Arguments for local context-bound request authorization. */
export type VerifyAuthorizationRequestArgs = {
  contextHandle: AuthorizationContextHandle;
  subject: string;
  reply: string | null;
  payload: Uint8Array;
  iat: number;
  requestId: string;
  proof: string;
  requiredPermissions: PermissionAtom[];
  policy: AuthorizationVerificationPolicy;
};

/** Arguments for local context-bound event authorization. */
export type VerifyAuthorizationEventArgs = {
  contextHandle: AuthorizationContextHandle;
  descriptorIdentity: string;
  subject: string;
  payload: Uint8Array;
  eventId: string;
  eventTime: string;
  proof: string;
  policy: AuthorizationVerificationPolicy;
  revokedAt?: number | null;
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

/** Projection returned after verifying an online-issued context. */
export type VerifiedAuthorizationContextTokenProjection = {
  issuer: AuthorizationIssuerKey;
  contextDigest: string;
  refreshAt: number;
  context: Record<string, unknown> & {
    ownerKind: "deployment" | "user";
    ownerId: string;
    grantRevision: number;
    principalId: string;
    principalKind: "user" | "service" | "device";
    participantId: string;
    identityKeyId: string | null;
    loginSessionId: string | null;
    deploymentId: string | null;
    instanceId: string | null;
    issuerKeyId: string;
    connectionId: string;
    sessionKey: string;
    inboxPrefix: string;
    issuedAt: number;
    notBefore: number;
    expiresAt: number;
    grants: GrantSet;
    platformPrivileges: PlatformPrivilege[];
    transportAuthorization: TransportAuthorizationV1;
  };
};

/** Opaque Rust/WASM verification state for one authorization context. */
export type AuthorizationContextHandle = WasmAuthorizationContextHandle;

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

/** Verify a complete signed authorization context through Rust/WASM. */
export async function verifyAuthorizationContextWasm(args: {
  issuer: AuthorizationIssuerKey;
  context: unknown;
  policy: AuthorizationContextVerificationPolicy;
}): Promise<VerifiedAuthorizationContextTokenProjection> {
  const { handle, verified } = await createAuthorizationContextHandleWasm(args);
  handle.free();
  return verified;
}

/** Verify and retain one authorization context for repeated proof checks. */
export async function createAuthorizationContextHandleWasm(args: {
  issuer: AuthorizationIssuerKey;
  context: unknown;
  policy: AuthorizationContextVerificationPolicy;
  historical?: boolean;
}): Promise<{
  handle: AuthorizationContextHandle;
  verified: VerifiedAuthorizationContextTokenProjection;
}> {
  await initializeProtocolWasm();
  const handle = protocolWasm.create_authorization_context_handle(
    JSON.stringify(args.issuer),
    JSON.stringify(args.context),
    JSON.stringify(wasmVerificationPolicy(args.policy)),
    args.historical ?? false,
  );
  try {
    const result = JSON.parse(
      handle.projection(),
    ) as VerifiedAuthorizationContextTokenProjection;
    const jitter = contextJitter(
      result.contextDigest,
      args.policy.refreshJitterSeconds,
    );
    return {
      handle,
      verified: {
        ...result,
        refreshAt: result.context.expiresAt - args.policy.refreshLeadSeconds -
          jitter,
      },
    };
  } catch (error) {
    handle.free();
    throw error;
  }
}

/** Require an opaque verified context to be currently eligible. */
export function assertAuthorizationContextHandleCurrentWasm(
  handle: AuthorizationContextHandle,
  policy: AuthorizationContextVerificationPolicy,
): void {
  handle.assert_current(JSON.stringify(wasmVerificationPolicy(policy)));
}

/** Recheck signed request freshness after verification; does not verify its proof. @internal */
export function assertAuthorizationRequestCurrentWasm(
  handle: AuthorizationContextHandle,
  iat: number,
  policy: AuthorizationContextVerificationPolicy,
): VerifyAuthorizationRequestResult {
  return JSON.parse(
    handle.assert_request_current(
      iat,
      JSON.stringify(wasmVerificationPolicy(policy)),
    ),
  ) as VerifyAuthorizationRequestResult;
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

/**
 * Encode a permission target as the canonical bytes an `AuthPermissionAtom`
 * carries.
 *
 * The protocol crate owns validation and the canonical (RFC 8785) encoding, so
 * TypeScript authors the logical target and never reconstructs the byte form.
 */
export async function encodePermissionTargetWasm(
  target: PermissionTarget,
): Promise<Uint8Array> {
  await initializeProtocolWasm();
  return protocolWasm.encode_permission_target(JSON.stringify(target));
}

/** Build a participant-resource permission target. */
export function participantResourceTarget(args: {
  participant: string;
  resource: "kv" | "store" | "jobQueue" | "eventConsumer" | "state";
  name: string;
}): PermissionTarget {
  return { kind: "participantResource", ...args };
}

/** Verify one context-bound request proof using actual received request bytes. */
export async function verifyAuthorizationRequestWasm(
  args: VerifyAuthorizationRequestArgs,
): Promise<VerifyAuthorizationRequestResult> {
  await initializeProtocolWasm();
  const { contextHandle, payload, ...input } = args;
  return JSON.parse(
    protocolWasm.verify_authorization_request(
      contextHandle,
      JSON.stringify({
        ...input,
        policy: wasmVerificationPolicy(args.policy),
      }),
      payload,
    ),
  ) as VerifyAuthorizationRequestResult;
}

/** Verify a compact Transfer proof against exact pinned session coordinates. */
export async function verifyTransferAuthorizationRequestWasm(
  args: VerifyAuthorizationRequestArgs,
  providerId: string,
  expectedConsumerConnection: string,
  transferId: string,
): Promise<VerifyAuthorizationRequestResult> {
  await initializeProtocolWasm();
  const { contextHandle, payload, ...input } = args;
  return JSON.parse(
    protocolWasm.verify_transfer_authorization_request(
      contextHandle,
      JSON.stringify({ ...input, policy: wasmVerificationPolicy(args.policy) }),
      payload,
      providerId,
      expectedConsumerConnection,
      transferId,
    ),
  ) as VerifyAuthorizationRequestResult;
}

/** Verify one context-bound event proof, including historical time/revocation checks. */
export async function verifyAuthorizationEventWasm(
  args: VerifyAuthorizationEventArgs,
): Promise<VerifyAuthorizationEventResult> {
  await initializeProtocolWasm();
  const { contextHandle, payload, ...input } = args;
  return JSON.parse(
    protocolWasm.verify_authorization_event(
      contextHandle,
      JSON.stringify({
        ...input,
        policy: wasmVerificationPolicy(args.policy),
      }),
      payload,
    ),
  ) as VerifyAuthorizationEventResult;
}

/** Encode only the fields accepted by the Rust verifier. @internal */
export function wasmVerificationPolicy(
  policy: AuthorizationVerificationPolicy,
): AuthorizationVerificationPolicy {
  return {
    nowUnixSeconds: policy.nowUnixSeconds,
    allowedClockSkewSeconds: policy.allowedClockSkewSeconds,
    maximumContextLifetimeSeconds: policy.maximumContextLifetimeSeconds,
    maximumContextBytes: policy.maximumContextBytes,
    maximumPermissions: policy.maximumPermissions,
  };
}

function contextJitter(contextDigest: string, maximum: number): number {
  const bytes = base64urlDecode(contextDigest);
  let value = 0n;
  for (const byte of bytes.slice(0, 8)) value = (value << 8n) | BigInt(byte);
  return Number(value % BigInt(maximum + 1));
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
