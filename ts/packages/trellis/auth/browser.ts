/**
 * @module
 * Browser OAuth credential custody. Durable DPoP keys are non-extractable;
 * ephemeral runtime/NATS keys are owned by the connection lifecycle.
 */

export {
  type BrowserOAuthGrant,
  type BrowserOAuthScope,
  BrowserSessionStore,
} from "./browser/storage.ts";
export {
  BrowserOAuthClient,
  type BrowserOAuthAdmission,
  type BrowserOAuthMetadata,
  type BrowserCapabilityRequest,
} from "./browser/oauth.ts";
export {
  classifyBrowserAuthError,
  isRecoverableBrowserAuthError,
} from "./browser_recovery.ts";
export type {
  BrowserAuthRecoveryClassification,
  BrowserAuthRecoveryKind,
} from "./browser_recovery.ts";
export { decodeTrellisHttpError, TrellisHttpError } from "./http_error.ts";
export {
  base64urlDecode,
  base64urlEncode,
  sha256,
  toArrayBuffer,
  utf8,
} from "./utils.ts";
