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
    surface: "rpc" | "operation" | "event" | "feed" | "state";
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

function wasmVerificationPolicy(
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
