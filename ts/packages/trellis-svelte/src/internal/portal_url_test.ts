import assert from "node:assert/strict";
import { test } from "node:test";

import {
  buildDeviceActivationCallbackPath,
  buildDeviceActivationConnectAuthUrlState,
  cleanupDeviceActivationCallbackUrl,
  resolveDeviceActivationUrlState,
} from "./portal_url.ts";

test("buildDeviceActivationCallbackPath adds explicit activation callback marker", () => {
  assert.deepEqual(
    buildDeviceActivationCallbackPath(
      new URL(
        "https://auth.example.com/login/device?flowId=device-flow#confirm",
      ),
      "callback-token",
    ),
    "/login/device?portalCallback=callback-token&deviceFlowId=device-flow#confirm",
  );
});

test("buildDeviceActivationConnectAuthUrlState keeps redirectTo but strips callback binding state", () => {
  const state = buildDeviceActivationConnectAuthUrlState(
    new URL(
      "https://auth.example.com/login/device?flowId=device-flow&portalCallback=callback-token&deviceFlowId=device-flow&authError=approval_denied#confirm",
    ),
  );

  assert.deepEqual(
    state.currentUrl.toString(),
    "https://auth.example.com/login/device#confirm",
  );
  assert.deepEqual(
    state.redirectTo,
    "https://auth.example.com/login/device?flowId=device-flow#confirm",
  );
});

test("cleanupDeviceActivationCallbackUrl preserves the activation flow id", () => {
  assert.deepEqual(
    cleanupDeviceActivationCallbackUrl(
      new URL(
        "https://auth.example.com/login/device?portalCallback=callback-token&deviceFlowId=device-flow&authError=approval_denied#confirm",
      ),
      "device-flow",
    ),
    "/login/device?flowId=device-flow#confirm",
  );
});

test("resolveDeviceActivationUrlState restores flowId from callback URL fallback", () => {
  assert.deepEqual(
    resolveDeviceActivationUrlState(
      new URL(
        "https://auth.example.com/login/device?portalCallback=callback-token&deviceFlowId=device-flow&flowId=auth-flow",
      ),
      null,
    ),
    { flowId: "device-flow", isAuthCallback: true },
  );
});

test("resolveDeviceActivationUrlState restores flowId from callback state", () => {
  assert.deepEqual(
    resolveDeviceActivationUrlState(
      new URL(
        "https://auth.example.com/login/device?portalCallback=callback-token&flowId=auth-flow",
      ),
      { flowId: "device-flow", callbackToken: "callback-token" },
    ),
    { flowId: "device-flow", isAuthCallback: true },
  );
});
