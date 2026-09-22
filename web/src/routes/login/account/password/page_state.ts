import {
  accountFlowIdFromUrl,
  type AccountFlowState,
  type ActiveAccountFlowState,
  completeAccountFlowLocalPassword,
  defaultProfileValue,
  flowKindLabel,
  formatAccountFlowError,
  hasLocalProvider,
  loadAccountFlowState,
  type LocalPasswordInput,
  type LocalPasswordSuccess,
  parseAccountFlowState,
  unavailableProviders,
} from "../../account_flow_state.ts";

export {
  accountFlowIdFromUrl,
  completeAccountFlowLocalPassword,
  defaultProfileValue,
  flowKindLabel,
  formatAccountFlowError,
  hasLocalProvider,
  loadAccountFlowState,
  parseAccountFlowState,
  unavailableProviders,
};
export type {
  AccountFlowState,
  ActiveAccountFlowState,
  LocalPasswordInput,
  LocalPasswordSuccess,
};

/** Return the configured password-policy minimum, if the flow exposes one. */
export function passwordMinimumLength(
  state: ActiveAccountFlowState,
): number | null {
  const minLength = state.passwordPolicy?.minLength;
  return typeof minLength === "number" && Number.isInteger(minLength) &&
      minLength > 0
    ? minLength
    : null;
}

/** Human-friendly password policy helper text for the password field. */
export function passwordPolicyHint(state: ActiveAccountFlowState): string {
  const minLength = passwordMinimumLength(state);
  return minLength === null
    ? "Use a strong password."
    : `Use at least ${minLength} characters.`;
}

/** Validate the password against flow policy before submitting to the backend. */
export function passwordPolicyError(
  state: ActiveAccountFlowState,
  password: string,
): string | null {
  const minLength = passwordMinimumLength(state);
  if (
    minLength !== null && password.length > 0 && password.length < minLength
  ) {
    return `Password must be at least ${minLength} characters.`;
  }
  return null;
}

/** Whether this portal route is intended for the loaded active flow. */
export function isExpectedPasswordFlow(state: ActiveAccountFlowState): boolean {
  return state.kind === "password_reset";
}

/** Primary heading for password reset flows. */
export function passwordFlowTitle(kind: string): string {
  if (kind === "password_reset") return "Reset your password";
  return "Set local credentials";
}

/** Submit button text for password reset flows. */
export function passwordFlowAction(kind: string): string {
  if (kind === "password_reset") return "Reset password";
  return "Save credentials";
}
