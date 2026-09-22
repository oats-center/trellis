import { assertEquals } from "@std/assert";

import {
  clearPreservedDeviceActivationCallbackState,
  getPreservedDeviceActivationCallbackState,
  isDeviceActivationAuthCallback,
  preserveDeviceActivationCallbackState,
} from "./callback_state.ts";

function createStorage(): Pick<Storage, "getItem" | "setItem" | "removeItem"> {
  const values = new Map<string, string>();

  return {
    getItem(key) {
      return values.get(key) ?? null;
    },
    setItem(key, value) {
      values.set(key, value);
    },
    removeItem(key) {
      values.delete(key);
    },
  };
}

Deno.test("device activation callback state round-trips through storage", () => {
  const storage = createStorage();
  clearPreservedDeviceActivationCallbackState(storage);

  preserveDeviceActivationCallbackState(storage, {
    flowId: "device-flow",
    callbackToken: "callback-token",
  });

  assertEquals(getPreservedDeviceActivationCallbackState(storage), {
    flowId: "device-flow",
    callbackToken: "callback-token",
  });

  clearPreservedDeviceActivationCallbackState(storage);
  assertEquals(getPreservedDeviceActivationCallbackState(storage), null);
});

Deno.test("device activation auth callback only matches explicit callback markers", () => {
  assertEquals(
    isDeviceActivationAuthCallback(
      new URL(
        "https://auth.example.com/login/device?flowId=device-flow",
      ),
      { flowId: "device-flow", callbackToken: "callback-token" },
    ),
    false,
  );

  assertEquals(
    isDeviceActivationAuthCallback(
      new URL(
        "https://auth.example.com/login/device?portalCallback=callback-token&flowId=auth-flow",
      ),
      { flowId: "device-flow", callbackToken: "callback-token" },
    ),
    true,
  );
});
