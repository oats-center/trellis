/**
 * Protocol-owned Trellis session keys and proof helpers.
 *
 * @module
 */

export {
  type AuthorizationContextBundle,
  AuthorizationContextBundleSchema,
  AuthorizationContextCache,
  AuthorizationContextRefreshError,
  AuthorizationProviderCache,
  type AuthorizationProviderEvent,
  type AuthorizationProviderRequest,
  type AuthorizationRoutingMaterial,
  type AuthorizationRuntimeBinding,
  refreshAuthorizationContext,
  refreshAuthorizationContextWithMetadata,
  startAuthorizationContextRefresh,
  type VerifiedAuthorizationContext,
} from "./authorization_context.ts";
export {
  type BrowserAuthRecoveryClassification,
  type BrowserAuthRecoveryKind,
  classifyBrowserAuthError,
  isRecoverableBrowserAuthError,
} from "./browser_recovery.ts";
export {
  buildDeviceActivationPayload,
  deriveDeviceConfirmationCode,
  deriveDeviceIdentity,
  waitForDeviceActivation,
} from "./device_activation.ts";
export {
  fetchPortalFlowState,
  portalFlowIdFromUrl,
  portalProviderLoginUrl,
  portalRedirectLocation,
  submitPortalApproval,
} from "./browser/portal.ts";
export type {
  DeviceUserAuthoritiesResolveOutput,
  DeviceUserAuthoritiesResolveProgress,
} from "../internal_sdk/generated/apis/auth/mod.js";
// Context-bound proof helpers for local signing and signature verification.
export {
  buildEventProofInput,
  buildProofInput,
  createEventProof,
  createProof,
  type EventProofParams,
  type ProofParams,
  verifyEventProof,
  verifyProof,
} from "./proof.ts";
export {
  parseSessionProof,
  SESSION_PROOF_FORMAT_V1,
  type SessionProof,
  type SessionProofInput,
  type SessionProofPolicy,
  type SessionProofPurpose,
  sessionProofRequestDigest,
  sessionProofSigningDigest,
  signSessionProof,
  verifySessionProof,
} from "./session_proof.ts";
export {
  createAuth,
  type NatsConnectOptions,
  type TrellisAuth,
} from "./session_auth.ts";
export { correctedIatSeconds, estimateMidpointClockOffsetMs } from "./time.ts";
export { trellisIdFromOriginId } from "./trellis_id.ts";
export {
  base64urlDecode,
  base64urlEncode,
  canonicalizeJsonValue,
  sha256,
  toArrayBuffer,
  utf8,
} from "./utils.ts";
