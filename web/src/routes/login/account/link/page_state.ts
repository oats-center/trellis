import {
  accountFlowIdFromUrl,
  accountFlowProviderLoginUrl,
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
  parseAccountFlowOAuthCompletion,
  parseAccountFlowState,
  unavailableProviders,
} from "../../account_flow_state.ts";

export {
  accountFlowIdFromUrl,
  accountFlowProviderLoginUrl,
  completeAccountFlowLocalPassword,
  defaultProfileValue,
  flowKindLabel,
  formatAccountFlowError,
  hasLocalProvider,
  loadAccountFlowState,
  parseAccountFlowOAuthCompletion,
  parseAccountFlowState,
  unavailableProviders,
};
export type {
  AccountFlowState,
  ActiveAccountFlowState,
  LocalPasswordInput,
  LocalPasswordSuccess,
};

/** Whether this portal route is intended for the loaded active flow. */
export function isExpectedLinkFlow(state: ActiveAccountFlowState): boolean {
  return state.kind === "identity_link";
}
