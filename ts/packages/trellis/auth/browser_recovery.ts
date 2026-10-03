import { machineErrorCode } from "../errors/index.ts";

/** Stable browser-auth recovery categories for app-owned recovery flows. */
export type BrowserAuthRecoveryKind =
  | "recoverable_stale_session"
  | "recoverable_expired_transaction"
  | "consent_required"
  | "recoverable_auth_required"
  | "policy_denied"
  | "insufficient_capabilities"
  | "runtime_unavailable"
  | "unknown";

/** Classification result for a browser-auth related failure. */
export type BrowserAuthRecoveryClassification = {
  kind: BrowserAuthRecoveryKind;
  recoverable: boolean;
  reason?: string;
  code?: string;
};

/** Classifies an exact auth machine error code. */
export function classifyBrowserAuthError(
  error: unknown,
): BrowserAuthRecoveryClassification {
  const code = machineErrorCode(error);
  const result = (kind: BrowserAuthRecoveryKind, recoverable: boolean) => ({
    kind,
    recoverable,
    ...(code ? { code, reason: code } : {}),
  });
  switch (code) {
    case "approval_denied":
    case "invalid_credentials":
    case "inactive_account":
    case "authority_rejected":
    case "authority_revoked":
    case "authority_expired":
    case "authority_not_found":
    case "contract_not_active":
    case "participant_changed":
    case "context_owner_mismatch":
      return result("policy_denied", false);
    case "consent_required":
      return result("consent_required", false);
    case "insufficient_capabilities":
      return result("insufficient_capabilities", false);
    case "not_ready":
    case "runtime_unavailable":
    case "authorization_pending":
      return result("runtime_unavailable", false);
    case "transaction_expired":
    case "transaction_not_found":
      return result("recoverable_expired_transaction", true);
    case "session_not_found":
    case "session_expired":
    case "user_not_found":
    case "session_revoked":
    case "user_inactive":
    case "identity_not_found":
    case "identity_inactive":
    case "login_not_found":
      return result("recoverable_stale_session", true);
    case "auth_required":
      return result("recoverable_auth_required", true);
    default:
      return result("unknown", false);
  }
}

/** Returns whether a browser auth error can silently restart sign-in. */
export function isRecoverableBrowserAuthError(error: unknown): boolean {
  return classifyBrowserAuthError(error).recoverable;
}
