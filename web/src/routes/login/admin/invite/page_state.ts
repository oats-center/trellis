import {
  accountFlowIdFromUrl,
  type AccountFlowState,
  type ActiveAccountFlowState,
  flowKindLabel,
  formatAccountFlowError,
  loadAccountFlowState,
  parseAccountFlowState,
} from "../../account_flow_state.ts";

export {
  accountFlowIdFromUrl,
  flowKindLabel,
  formatAccountFlowError,
  loadAccountFlowState,
  parseAccountFlowState,
};
export type { AccountFlowState, ActiveAccountFlowState };

/** Whether this portal route is intended for the loaded active flow. */
export function isExpectedInviteFlow(state: ActiveAccountFlowState): boolean {
  void state;
  return false;
}
