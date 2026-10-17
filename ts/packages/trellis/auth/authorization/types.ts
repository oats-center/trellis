import { type Static, Type } from "typebox";

import type {
  AuthorizationContextVerificationPolicy,
  PermissionAtom,
  VerifiedAuthorizationContextTokenProjection,
} from "../protocol_wasm.ts";

/** Pinned root and connected NATS KV registry binding. */
export const AuthorizationRegistryBindingSchema = Type.Object({
  contextBucket: Type.String({ minLength: 1 }),
});

/** Wire binding for Trellis' internal authorization registry. */
export type AuthorizationRegistryBinding = Static<
  typeof AuthorizationRegistryBindingSchema
>;

/** Wire schema for one published authorization context bundle. */
export const AuthorizationContextBundleSchema = Type.Object({
  context: Type.Unknown(),
  issuer: Type.Object({
    keyId: Type.String({ minLength: 1 }),
    publicKey: Type.String({ minLength: 1 }),
    state: Type.Union([
      Type.Literal("active"),
      Type.Literal("retired"),
      Type.Literal("revoked"),
    ]),
  }),
  authorizationRegistry: AuthorizationRegistryBindingSchema,
  policy: Type.Object({
    allowedClockSkewSeconds: Type.Integer({ minimum: 0 }),
    maximumContextLifetimeSeconds: Type.Integer({ minimum: 1 }),
    // Canonical signed-context JSON size in UTF-8 bytes.
    maximumContextBytes: Type.Integer({ minimum: 1 }),
    maximumPermissions: Type.Integer({ minimum: 1 }),
    refreshLeadSeconds: Type.Integer({ minimum: 1 }),
    refreshJitterSeconds: Type.Integer({ minimum: 0 }),
  }),
});

/** Published signed context and the trust information needed to verify it. */
export type AuthorizationContextBundle = Static<
  typeof AuthorizationContextBundleSchema
>;

/** Route-selection JWT installed atomically with a context. */
export type AuthorizationRoutingMaterial = {
  bootstrapJwt: string;
  bootstrapJwtExpiresAt: number;
};

/** Transport endpoints retained for reconnect recovery. */
export type AuthorizationRuntimeTransports = {
  native?: { natsServers: string[] };
  websocket?: { natsServers: string[] };
};

/** Transport metadata retained for reconnect recovery. */
export type AuthorizationRuntimeBinding = {
  connectionId: string;
  loginSessionId: string | null;
  participantId: string;
  inboxPrefix: string;
  transports: AuthorizationRuntimeTransports;
};

/** Trusted context projection returned by the Rust/WASM verifier. */
export type VerifiedAuthorizationContext =
  VerifiedAuthorizationContextTokenProjection;

/** Installed trust material that a provider-side verifier may reuse. */
export type AuthorizationContextVerificationMaterial = {
  issuer: AuthorizationContextBundle["issuer"];
  context: unknown;
  contextDigest: string;
  policy: AuthorizationContextVerificationPolicy;
  verified: VerifiedAuthorizationContext;
};

/** Presented request proof data supplied by a provider transport adapter. */
export type AuthorizationProviderRequest = {
  contextDigest: string;
  sessionKey: string;
  subject: string;
  reply: string | null;
  payload: Uint8Array;
  iat: number;
  requestId: string;
  proof: string;
  requiredPermissions: PermissionAtom[];
  requiredCapabilities: string[];
};

/** Presented event proof data supplied by a provider event adapter. */
export type AuthorizationProviderEvent = {
  contextDigest: string;
  sessionKey: string;
  descriptorIdentity: string;
  subject: string;
  payload: Uint8Array;
  eventId: string;
  eventTime: string;
  proof: string;
  requiredCapabilities: string[];
};
