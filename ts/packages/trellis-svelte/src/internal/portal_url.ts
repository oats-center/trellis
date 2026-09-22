import type { DeviceActivationCallbackState } from "./callback_state.ts";
import { isDeviceActivationAuthCallback } from "./callback_state.ts";

export type DeviceActivationConnectAuthUrlState = {
  currentUrl: URL;
  redirectTo: string;
};

export type DeviceActivationUrlState = {
  flowId: string | null;
  isAuthCallback: boolean;
};

const ACTIVATION_CALLBACK_QUERY_PARAM = "portalCallback";
const AUTH_ERROR_QUERY_PARAM = "authError";
const DEVICE_FLOW_ID_QUERY_PARAM = "deviceFlowId";
const FLOW_ID_QUERY_PARAM = "flowId";

export function buildDeviceActivationCallbackPath(
  currentUrl: URL,
  callbackToken: string,
): string {
  const callbackUrl = new URL(currentUrl.pathname, currentUrl.origin);
  callbackUrl.searchParams.set(ACTIVATION_CALLBACK_QUERY_PARAM, callbackToken);
  const flowId = currentUrl.searchParams.get(FLOW_ID_QUERY_PARAM);
  if (flowId) {
    callbackUrl.searchParams.set(DEVICE_FLOW_ID_QUERY_PARAM, flowId);
  }
  callbackUrl.hash = currentUrl.hash;
  return `${callbackUrl.pathname}${callbackUrl.search}${callbackUrl.hash}`;
}

export function buildDeviceActivationConnectAuthUrlState(
  currentUrl: URL,
): DeviceActivationConnectAuthUrlState {
  const redirectUrl = new URL(currentUrl);
  redirectUrl.searchParams.delete(ACTIVATION_CALLBACK_QUERY_PARAM);
  redirectUrl.searchParams.delete(AUTH_ERROR_QUERY_PARAM);
  redirectUrl.searchParams.delete(DEVICE_FLOW_ID_QUERY_PARAM);

  const authCurrentUrl = new URL(redirectUrl);
  authCurrentUrl.searchParams.delete(DEVICE_FLOW_ID_QUERY_PARAM);
  authCurrentUrl.searchParams.delete(FLOW_ID_QUERY_PARAM);

  return {
    currentUrl: authCurrentUrl,
    redirectTo: redirectUrl.toString(),
  };
}

export function cleanupDeviceActivationCallbackUrl(
  currentUrl: URL,
  flowId: string | null,
): string | null {
  if (
    !currentUrl.searchParams.has(FLOW_ID_QUERY_PARAM) &&
    !currentUrl.searchParams.has(AUTH_ERROR_QUERY_PARAM) &&
    !currentUrl.searchParams.has(DEVICE_FLOW_ID_QUERY_PARAM) &&
    !currentUrl.searchParams.has(ACTIVATION_CALLBACK_QUERY_PARAM)
  ) {
    return null;
  }

  const nextUrl = new URL(currentUrl);
  nextUrl.searchParams.delete(FLOW_ID_QUERY_PARAM);
  nextUrl.searchParams.delete(AUTH_ERROR_QUERY_PARAM);
  nextUrl.searchParams.delete(DEVICE_FLOW_ID_QUERY_PARAM);
  nextUrl.searchParams.delete(ACTIVATION_CALLBACK_QUERY_PARAM);
  if (flowId) {
    nextUrl.searchParams.set(FLOW_ID_QUERY_PARAM, flowId);
  }

  return `${nextUrl.pathname}${nextUrl.search}${nextUrl.hash}`;
}

export function resolveDeviceActivationUrlState(
  currentUrl: URL,
  preservedState: DeviceActivationCallbackState | null,
): DeviceActivationUrlState {
  const callbackFlowId = currentUrl.searchParams.get(
    DEVICE_FLOW_ID_QUERY_PARAM,
  );
  const isAuthCallback =
    isDeviceActivationAuthCallback(currentUrl, preservedState) ||
    (currentUrl.searchParams.has(ACTIVATION_CALLBACK_QUERY_PARAM) &&
      !!callbackFlowId);

  return {
    flowId: isAuthCallback
      ? preservedState?.flowId ?? callbackFlowId
      : currentUrl.searchParams.get(FLOW_ID_QUERY_PARAM),
    isAuthCallback,
  };
}
