/**
 * @module
 * Browser-based authentication utilities for session-key based authentication.
 * Uses WebCrypto API and IndexedDB for secure key storage.
 */

export {
  type ApprovalDecision,
  createPortalBinding,
  fetchPortalFlowState,
  getOrCreatePortalBinding,
  type PortalBinding,
  portalFlowIdFromUrl,
  type PortalFlowState,
  type PortalFlowState as BrowserPortalFlowState,
  portalProviderLoginUrl,
  portalRedirectLocation,
  submitPortalApproval,
} from "./browser/portal.ts";
export { BrowserSessionStore } from "./browser/storage.ts";
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
