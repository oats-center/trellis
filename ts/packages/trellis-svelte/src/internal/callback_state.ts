export type DeviceActivationCallbackState = {
  flowId: string;
  callbackToken: string;
};

export type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

const PRESERVED_ACTIVATION_FLOW_ID_STORAGE_KEY = "portal.activate.flowId";
const ACTIVATION_CALLBACK_TOKEN_STORAGE_KEY = "portal.activate.callbackToken";
const ACTIVATION_CALLBACK_QUERY_PARAM = "portalCallback";

export function getPreservedDeviceActivationCallbackState(
  storage: StorageLike,
): DeviceActivationCallbackState | null {
  const flowId = storage.getItem(PRESERVED_ACTIVATION_FLOW_ID_STORAGE_KEY);
  const callbackToken = storage.getItem(ACTIVATION_CALLBACK_TOKEN_STORAGE_KEY);

  if (!flowId || !callbackToken) return null;

  return { flowId, callbackToken };
}

export function preserveDeviceActivationCallbackState(
  storage: StorageLike,
  nextState: DeviceActivationCallbackState,
): void {
  storage.setItem(PRESERVED_ACTIVATION_FLOW_ID_STORAGE_KEY, nextState.flowId);
  storage.setItem(
    ACTIVATION_CALLBACK_TOKEN_STORAGE_KEY,
    nextState.callbackToken,
  );
}

export function clearPreservedDeviceActivationCallbackState(
  storage: StorageLike,
): void {
  storage.removeItem(PRESERVED_ACTIVATION_FLOW_ID_STORAGE_KEY);
  storage.removeItem(ACTIVATION_CALLBACK_TOKEN_STORAGE_KEY);
}

export function isDeviceActivationAuthCallback(
  currentUrl: URL,
  preservedState: DeviceActivationCallbackState | null,
): boolean {
  if (!preservedState) return false;
  return currentUrl.searchParams.get(ACTIVATION_CALLBACK_QUERY_PARAM) ===
    preservedState.callbackToken;
}
