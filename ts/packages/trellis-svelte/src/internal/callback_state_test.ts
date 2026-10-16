import assert from "node:assert/strict";
import { test } from "node:test";

import {
  clearPreservedDeviceActivationCallbackState,
  getPreservedDeviceActivationCallbackState,
  isDeviceActivationAuthCallback,
  preserveDeviceActivationCallbackState,
} from "./callback_state.ts";

test("device activation callback state round-trips through Node Web Storage", () => {
  const storage = sessionStorage;
  clearPreservedDeviceActivationCallbackState(storage);

  preserveDeviceActivationCallbackState(storage, {
    flowId: "device-flow",
    callbackToken: "callback-token",
  });

  assert.deepEqual(getPreservedDeviceActivationCallbackState(storage), {
    flowId: "device-flow",
    callbackToken: "callback-token",
  });

  clearPreservedDeviceActivationCallbackState(storage);
  assert.deepEqual(getPreservedDeviceActivationCallbackState(storage), null);
});

test("device activation auth callback only matches explicit callback markers", () => {
  assert.deepEqual(
    isDeviceActivationAuthCallback(
      new URL("https://auth.example.com/login/device?flowId=device-flow"),
      { flowId: "device-flow", callbackToken: "callback-token" },
    ),
    false,
  );

  assert.deepEqual(
    isDeviceActivationAuthCallback(
      new URL(
        "https://auth.example.com/login/device?portalCallback=callback-token&flowId=auth-flow",
      ),
      { flowId: "device-flow", callbackToken: "callback-token" },
    ),
    true,
  );
});
