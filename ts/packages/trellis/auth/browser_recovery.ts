import { machineErrorCode } from "../errors/index.ts";

/** Stable browser-auth recovery categories for app-owned recovery flows. */
export type BrowserAuthRecoveryKind =
  | "recoverable_stale_session"
  | "recoverable_expired_flow"
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
    case "user_inactive":
    case "inactive_account":
      return result("policy_denied", false);
    case "insufficient_capabilities":
      return result("insufficient_capabilities", false);
    case "not_ready":
    case "runtime_unavailable":
      return result("runtime_unavailable", false);
    case "flow_expired":
    case "flow_not_found":
      return result("recoverable_expired_flow", true);
    case "session_not_found":
    case "session_expired":
    case "user_not_found":
    case "contract_not_active":
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
