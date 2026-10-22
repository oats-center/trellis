import type { VerifiedSessionAuthorityHandle as WasmSessionAuthorityHandle } from "./protocol_wasm/client/trellis_protocol_wasm.js";
import * as clientProtocolWasm from "./protocol_wasm/client/trellis_protocol_wasm.js";
import type * as ProviderProtocolWasm from "./protocol_wasm/provider/trellis_protocol_wasm.js";
import { base64urlDecode, type JsonValue } from "./utils.ts";

type JsonObject = { [key: string]: JsonValue };

/** Generated bindings of the request-serving protocol build. */
export type ProviderProtocolWasmModule = typeof ProviderProtocolWasm;

// Callers use the client build. Services and devices select the provider build,
// a superset, before first use; one process runs exactly one instance.
let protocolWasm: typeof clientProtocolWasm = clientProtocolWasm;
let providerProtocolWasm: ProviderProtocolWasmModule | undefined;
let protocolWasmUrl = new URL(
  "./protocol_wasm/client/trellis_protocol_wasm_bg.wasm",
  import.meta.url,
);

/**
 * Select the provider protocol build for a request-serving process.
 *
 * Called once by the service and device entrypoints at import, before any
 * protocol use. Selecting after the client build was loaded fails, because
 * values created by one instance are invalid in another.
 */
export function selectProviderProtocolWasm(
  bindings: ProviderProtocolWasmModule,
  url: URL,
): void {
  if (providerProtocolWasm) return;
  if (initialized || initializedSync) {
    throw new Error(
      "Trellis loaded its client protocol build before the service or device entrypoint; import that entrypoint first",
    );
  }
  providerProtocolWasm = bindings;
  protocolWasm = bindings;
  protocolWasmUrl = url;
}

function providerProtocol(): ProviderProtocolWasmModule {
  if (!providerProtocolWasm) {
    throw new Error(
      "this protocol operation serves requests; import @oatscenter/trellis/service or /device",
    );
  }
  return providerProtocolWasm;
}

/** Bindings of the selected protocol build, initialized for synchronous use. */
export function protocolBindings(): typeof clientProtocolWasm {
  initializeProtocolWasmSync();
  return protocolWasm;
}

/** Validate framework-private Operation state through the shared native codec. */
export function normalizeOperationRecordWasm(value: unknown): unknown {
  initializeProtocolWasmSync();
  return JSON.parse(
    providerProtocol().normalize_operation_record(JSON.stringify(value)),
  );
}

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

/** Exact Rust-owned OAuth admission signature domain. */
export const OAUTH_ADMISSION_FORMAT_V1 = "trellis.oauth-admission.v1";

/** Auth-certified OAuth client binding, not caller-asserted permissions. */
export type OAuthAdmissionBinding =
  | Readonly<{ bindingKind: "browser"; browserOrigin: string }>
  | Readonly<{ bindingKind: "native"; nativeDpopJkt: string }>;

/** Immutable projection of the Rust-owned signed OAuth admission body. */
export type OAuthAdmission = Readonly<{
  format: typeof OAUTH_ADMISSION_FORMAT_V1;
  issuerKeyId: string;
  trellisInstanceId: string;
  audienceNatsAccount: string;
  oauthGrantId: string;
  refreshFamilyId: string;
  loginSessionId: string;
  principalId: string;
  clientId: string;
  binding: OAuthAdmissionBinding;
  persistentDpopJkt: string;
  runtimeId: string;
  ephemeralNkey: string;
  issuedAt: number;
  notBefore: number;
  expiresAt: number;
}>;

/** Exact signed envelope returned by successful shared verification. */
export type SignedOAuthAdmission = Readonly<{
  credential: OAuthAdmission;
  signature: string;
}>;

/** Explicit pinned scope, runtime key and bounded validity for Rust verification. */
export type OAuthAdmissionVerificationInput = Readonly<{
  issuer: Readonly<AuthorityIssuerKey>;
  trellisInstanceId: string;
  audienceNatsAccount: string;
  ephemeralNkey: string;
  nowUnixSeconds: number;
  maximumLifetimeSeconds: number;
}>;

/** Structured success or current shared authorization error. */
export type VerifyOAuthAdmissionResult =
  | Readonly<{ ok: true; admission: SignedOAuthAdmission }>
  | Readonly<{ ok: false; error: AuthorizationVerificationError }>;

/**
 * Authenticate strict signed OAuth bytes with the existing Rust verifier.
 * Callers must additionally check runtime/client/DPoP binding and saved grant
 * identity before installing the credential or connecting.
 */
export async function verifyOAuthAdmissionWasm(
  bytes: Uint8Array,
  verification: OAuthAdmissionVerificationInput,
): Promise<VerifyOAuthAdmissionResult> {
  await initializeProtocolWasm();
  const result = JSON.parse(
    protocolWasm.verify_oauth_admission(bytes, JSON.stringify(verification)),
  ) as VerifyOAuthAdmissionResult;
  if (result.ok) {
    Object.freeze(result.admission.credential.binding);
    Object.freeze(result.admission.credential);
    Object.freeze(result.admission);
  } else {
    Object.freeze(result.error);
  }
  return Object.freeze(result);
}

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

/** Rust-owned original event identity and accepted-catalog stamp. */
export type SessionEvent = {
  authorityDigest: string;
  apiId: string;
  apiGeneration: string;
  acceptedRevision: string;
  action: string;
  subject: string;
  eventId: string;
  eventTime: string;
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
  | "InvalidResponseProof"
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

/** Rust-owned provider response transcript fields, before verification. */
export type SessionResponse = {
  trellisInstanceId: string;
  audienceNatsAccount: string;
  providerAuthorityDigest: string;
  providerCertificateDigest: string;
  apiId: string;
  apiGeneration: string;
  action: string;
  requestAuthorityDigest: string;
  requestId: string;
  requestProofDigest: string;
  subject: string;
  kind: "success" | "error";
  issuedAt: number;
};

/** Exact retained opening and actual transport metadata for provider verification. */
export type VerifySessionResponseArgs = {
  providerHandle: SessionAuthorityHandle;
  response: SessionResponse;
  payload: Uint8Array;
  proof: string;
  /** Untrusted certificate bytes are authenticated by Rust, not merely decoded. */
  certificate: JsonObject;
  certificateIssuer: AuthorityIssuerKey;
  pendingRequest: SessionRequest;
  pendingRequestDigest: string;
  /** Route-owned surface/name/direction, never inferred from a response. */
  action: JsonObject;
  expectedDeploymentId: string | null;
  receivedSubject: string;
  policy: SessionAuthorityVerificationPolicy;
  knownRevoked: boolean;
};

/** Only a successful shared verification authenticates application result bytes. */
export type VerifySessionResponseResult =
  | { ok: true; providerAuthorityDigest: string; response: SessionResponse }
  | { ok: false; error: AuthorizationVerificationError };

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
  "current" | "upgrade_available" | "reduction_required";

/** Immutable signed material authenticated by the Rust-owned parser/verifier. */
export type VerifiedSessionAuthorityProjection = {
  authorityDigest: string;
  authority: SessionAuthority;
};

/** Authenticated Rust projection, not a TypeScript contract-authoring schema. */
export type SessionAuthority = Omit<AuthenticatedCaller, "authorityDigest"> & {
  format: string;
  issuerKeyId: string;
  trellisInstanceId: string;
  audienceNatsAccount: string;
  providerBindings: {
    apiId: string;
    generation: string;
    deploymentId: string;
    implementationDigest: string;
    providerCertificateDigest: string;
  }[];
  resourceBindingsDigest?: string;
  issuedAt: number;
  notBefore: number;
  expiresAt: number;
  extensions: JsonObject;
  critical: string[];
  signature: string;
};

/** Native protocol's ephemeral-key proof for the first own-context lookup. */
export type AdmissionGet = {
  trellisInstanceId: string;
  runtimeId: string;
  sessionPublicKey: string;
  attachmentId: string;
  requestNonce: string;
  issuedAt: number;
};

/** Native protocol's issuer-authenticated installation response. */
export type AdmissionContext = {
  format: string;
  issuerKeyId: string;
  trellisInstanceId: string;
  audienceNatsAccount: string;
  runtimeId: string;
  requestNonce: string;
  requestSubject: string;
  replySubject: string;
  sessionAuthority: string;
  registry: JsonObject;
  issuedAt: number;
  expiresAt: number;
};

/** Sign the native admission transcript, including actual request/reply subjects. */
export async function signAdmissionGetWasm(
  request: AdmissionGet,
  subject: string,
  reply: string,
  seed: Uint8Array,
): Promise<string> {
  await initializeProtocolWasm();
  return protocolWasm.sign_admission_get(
    JSON.stringify(request),
    subject,
    reply,
    seed,
  );
}

/** Native provisioned CONNECT proof; Auth alone resolves principal and implementation. */
export async function signProvisionedAdmissionWasm(
  credential: {
    format: "trellis.provisioned-admission.v1";
    trellisInstanceId: string;
    audienceNatsAccount: string;
    credentialId: string;
    runtimeId: string;
    ephemeralNkey: string;
    issuedAt: number;
    expiresAt: number;
  },
  seed: Uint8Array,
): Promise<string> {
  await initializeProtocolWasm();
  return providerProtocol().sign_provisioned_admission(
    JSON.stringify(credential),
    seed,
  );
}

/** Authenticate exact own-context bytes against a selected issuer and pending lookup. */
export async function verifyAdmissionContextWasm(
  bytes: Uint8Array,
  issuer: AuthorityIssuerKey,
  request: AdmissionGet,
  subject: string,
  reply: string,
  account: string,
  now: number,
): Promise<AdmissionContext> {
  await initializeProtocolWasm();
  const result: { response: AdmissionContext } = JSON.parse(
    protocolWasm.verify_admission_context(
      bytes,
      JSON.stringify({
        issuer,
        request,
        subject,
        reply,
        account,
        nowUnixSeconds: now,
      }),
    ),
  );
  return result.response;
}

/** Compiler-owned exact dispatcher identity. */
export type CatalogActionIdentity = {
  kind: "rpc" | "event" | "operation" | "live" | "state";
  name: string;
  direction:
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

/** Issuer-authenticated immutable catalog projection returned by Rust. */
export type CatalogSnapshot = {
  issuerKeyId: string;
  apiId: string;
  generation: string;
  acceptedRevision: string;
  definitionDigest: string;
};

/** Authenticated sticky logical-session retirement, also used for history. */
export type SessionRevocation = {
  issuerKeyId: string;
  authorizationSessionId: string;
  effectiveCutoff: number;
  latestContextExpiry: number;
};

/** Exact protocol-owned public issuer metadata lookup. */
export type IssuerMetadataQuery =
  | { kind: "authorityResolve"; contextDigest: string }
  | { kind: "authorityStatus"; authorizationSessionId: string }
  | { kind: "issuerKey"; keyId: string }
  | { kind: "issuerChain"; pinnedKeyId: string; targetKeyId: string };

/** Retained scope and nonce for a signed issuer metadata exchange. */
export type IssuerMetadataRequest = {
  trellisInstanceId: string;
  audienceNatsAccount: string;
  requestNonce: string;
  query: IssuerMetadataQuery;
};

/** Issuer-authenticated result; missing and unavailable are never authority. */
export type IssuerMetadataResponse = {
  issuerKeyId: string;
  request: IssuerMetadataRequest;
  requestSubject: string;
  replySubject: string;
  issuedAt: number;
  expiresAt: number;
  result:
    { kind: "value"; payload: string } | { kind: "missing" | "unavailable" };
};

/** Read the shared protocol's admitted-only metadata route. */
export async function issuerMetadataSubjectWasm(): Promise<string> {
  await initializeProtocolWasm();
  return protocolWasm.issuer_metadata_subject();
}

/** Authenticate exact request, route, reply destination, freshness and pinned issuer. */
export async function verifyIssuerMetadataWasm(
  bytes: Uint8Array,
  verification: {
    issuer: AuthorityIssuerKey;
    request: IssuerMetadataRequest;
    requestSubject: string;
    replySubject: string;
    nowUnixSeconds: number;
  },
): Promise<IssuerMetadataResponse> {
  await initializeProtocolWasm();
  return parseWasmResult<{ response: IssuerMetadataResponse }>(
    protocolWasm.verify_issuer_metadata(JSON.stringify(verification), bytes),
  ).response;
}

/** Trusted installation/account and signer supplied to metadata verification. */
export type MetadataVerification = {
  issuer: AuthorityIssuerKey;
  trellisInstanceId: string;
  audienceNatsAccount: string;
  nowUnixSeconds: number;
  installedSequence?: string;
};

/** Authenticate one bounded cold-path record in the canonical protocol implementation. */
export async function verifyAuthorityMetadataWasm(
  kind: "catalog" | "provider" | "rotation" | "revocation",
  bytes: Uint8Array,
  verification: MetadataVerification,
): Promise<{
  digest: string;
  record: JsonObject;
  successor?: AuthorityIssuerKey;
}> {
  await initializeProtocolWasm();
  return parseWasmResult(
    protocolWasm.verify_authority_metadata(
      kind,
      bytes,
      JSON.stringify(verification),
    ),
  );
}

/** Authenticate the catalog and exact delegated or certified provider-event publication rights. */
export async function authorizeCatalogActionWasm(
  authority: SessionAuthorityHandle,
  catalog: Uint8Array,
  verification: MetadataVerification & {
    providerPublication?: {
      certificate: JsonObject;
      issuer: AuthorityIssuerKey;
      at: number;
      publishedAt?: number;
    };
  },
  action: CatalogActionIdentity,
): Promise<boolean> {
  await initializeProtocolWasm();
  return parseWasmResult<{ authorized: boolean }>(
    protocolWasm.authorize_catalog_action(
      authority,
      catalog,
      JSON.stringify(verification),
      JSON.stringify(action),
    ),
  ).authorized;
}

/** Original broker provenance; never synthesized from a publisher's header. */
export type OriginalEventPublication = {
  publishedAt: string;
  streamSequence: string;
};

/** Verify live or historical event bytes with actual retained broker provenance. */
export async function verifySessionEventWasm(args: {
  authorityHandle: SessionAuthorityHandle;
  event: SessionEvent;
  payload: Uint8Array;
  proof: string;
  policy: SessionAuthorityVerificationPolicy;
  originalPublication: OriginalEventPublication | null;
  knownRevoked: boolean;
}): Promise<
  | { ok: true; authorityDigest: string; publisherAuthority: SessionAuthority }
  | { ok: false; error: AuthorizationVerificationError }
> {
  await initializeProtocolWasm();
  const { authorityHandle, payload, ...verification } = args;
  return JSON.parse(
    protocolWasm.verify_session_event(
      authorityHandle,
      JSON.stringify(verification),
      payload,
    ),
  );
}

/** Opaque shared verification state for one immutable session authority. */
export type SessionAuthorityHandle = WasmSessionAuthorityHandle;

let initialized: Promise<void> | undefined;
let initializedSync = false;

/** Initialize the selected protocol build before asynchronous use. */
export async function initializeProtocolWasm(): Promise<void> {
  if (initializedSync) return;
  initialized ??= (async () => {
    const bindings = protocolWasm;
    const url = protocolWasmUrl;
    if (url.protocol === "file:") {
      const { readFile } = await import("node:fs/promises");
      await bindings.default({ module_or_path: await readFile(url) });
      return;
    }
    // Browsers fetch the module as a separate cacheable asset.
    await bindings.default({ module_or_path: fetch(url) });
  })();
  await initialized;
  initializedSync = true;
}

/**
 * Initialize the selected protocol build for synchronous use.
 *
 * Server runtimes read the module from disk. Browsers cannot load it
 * synchronously; connecting initializes it first.
 */
export function initializeProtocolWasmSync(): void {
  if (initializedSync) return;
  const fs =
    protocolWasmUrl.protocol === "file:"
      ? globalThis.process?.getBuiltinModule?.("node:fs")
      : undefined;
  if (!fs) {
    throw new Error(
      "Trellis protocol is not initialized; connect, or await initializeProtocolWasm(), first",
    );
  }
  protocolWasm.initSync({ module: fs.readFileSync(protocolWasmUrl) });
  initializedSync = true;
}

/** Verify before decoding a provider result or completing its pending request. */
export async function verifySessionResponseWasm(
  args: VerifySessionResponseArgs,
): Promise<VerifySessionResponseResult> {
  await initializeProtocolWasm();
  const { providerHandle, payload, ...verification } = args;
  return JSON.parse(
    protocolWasm.verify_session_response(
      providerHandle,
      JSON.stringify(verification),
      payload,
    ),
  ) as VerifySessionResponseResult;
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
    providerProtocol().verify_session_request(
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
    binding: SessionBinding;
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
  const result = JSON.parse(encoded) as
    | ({ ok: true } & T)
    | {
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
export function liveParseControlResponse(raw: Uint8Array): LiveControlResponse {
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
  return providerProtocol().live_logical_control_hash(raw);
}

/** Canonical Transfer session subject family. */
export type TransferSubjectKind =
  "upload-data" | "download-data" | "control" | "signal";

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
  format: "trellis.transfer.v1";
  type: "control";
  transferId: string;
  controlSeq: string;
  receivedSeq: string;
  consumedSeq: string;
};

/** Strict v2 caller controls, parsed and validated by canonical Rust/WASM. */
export type TransferControlWire = TransferControlBase &
  (
    | {
        action: "activate";
        receiveMaxFrameBytes: number;
        /** A receiving consumer's admitted window; downloads only. */
        receiveWindowFrames?: number;
        receiveWindowBytes?: number;
      }
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
  format: "trellis.transfer.v1";
  type: "TransferGrant";
  service: string;
  origin: { apiId: string; action: CatalogActionIdentity };
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
  TransferSendGrantWire | TransferReceiveGrantWire;

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
export type TransferSignalWire = {
  format: "trellis.transfer.v1";
  transferId: string;
} & (
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
  format: "trellis.transfer.v1";
  type: "complete";
  transferId: string;
};

/** Canonical protocol limits and header names exported by Rust/WASM. */
export type TransferConstants = {
  format: "trellis.transfer.v1";
  maxFrameBytes: number;
  windowFrames: number;
  windowBytes: number;
  maxWindowFrames: number;
  maxWindowBytes: number;
  processReceiveBytes: number;
  creditFrameStep: number;
  creditByteStep: number;
  creditMaxDelayMs: number;
  headerReserve: number;
  operationUploadLifetimeSeconds: number;
  operationUploadMaxBytes: number;
  maxControlBytes: number;
  sequenceHeader: string;
  controlHeader: string;
  terminalHeader: string;
  proofHeader: string;
};

/** Read canonical Transfer constants, initializing WASM only on first access. */
export function transferConstants(): TransferConstants {
  return transferConstantsProxy;
}
let transferConstantsValue: TransferConstants | undefined;
// Read lazily, once: importing a module never loads the protocol build.
const transferConstantsProxy = new Proxy({} as TransferConstants, {
  get(_target, property) {
    initializeProtocolWasmSync();
    transferConstantsValue ??= JSON.parse(
      protocolWasm.transfer_constants(),
    ) as TransferConstants;
    return transferConstantsValue[property as keyof TransferConstants];
  },
});

/** Generate a canonical random transfer ID. */
export function transferGenerateId(): string {
  initializeProtocolWasmSync();
  return providerProtocol().transfer_generate_id();
}

/** Derive an exact session subject from both logical endpoint identities. */
export function transferSubject(
  kind: TransferSubjectKind,
  provider: string,
  consumer: string,
  transferId: string,
): string {
  initializeProtocolWasmSync();
  return providerProtocol().transfer_subject(
    kind,
    provider,
    consumer,
    transferId,
  );
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

/**
 * Receive window `[frames, bytes]` to offer from `available` admission bytes,
 * or `undefined` when less than one frame fits. Shared with the Rust SDK.
 */
export function transferReceiveWindow(
  available: number,
  maxFrameBytes: number,
): [number, number] | undefined {
  initializeProtocolWasmSync();
  const window: unknown = JSON.parse(
    protocolWasm.transfer_receive_window(available, maxFrameBytes),
  );
  if (window === null) return undefined;
  if (
    !Array.isArray(window) ||
    window.length !== 2 ||
    !window.every((value) => Number.isSafeInteger(value))
  )
    throw new Error("invalid transfer receive window");
  return [window[0], window[1]];
}

/** Verify a provider's Ed25519 proof over a natively computed 32-byte digest. */
export function verifyProofDigest(
  proof: string,
  digest: Uint8Array,
  providerKey: string,
): void {
  initializeProtocolWasmSync();
  protocolWasm.verify_proof_digest(proof, digest, providerKey);
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
    providerProtocol().transfer_parse_control(raw),
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
    providerProtocol().transfer_parse_complete(raw),
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
