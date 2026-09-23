import { assertEquals } from "@std/assert";

import { AuthError } from "@oatscenter/trellis";

import {
  isDeviceActivationReviewerRequiredFailure,
  mapDeviceActivationFailure,
} from "./activation_view.ts";

Deno.test("mapDeviceActivationFailure uses exact terminal auth codes", () => {
  assertEquals(
    mapDeviceActivationFailure("flow_123", {
      reason: "device_activation_flow_not_found",
    }),
    {
      mode: "invalid_flow",
      flowId: "flow_123",
      reason: "This activation link is no longer valid.",
    },
  );
  assertEquals(
    mapDeviceActivationFailure("flow_123", {
      reason: "device_activation_flow_expired",
    }),
    {
      mode: "expired",
      flowId: "flow_123",
      reason:
        "The activation request expired. Start again from the auth service.",
    },
  );
  assertEquals(
    mapDeviceActivationFailure("flow_123", {
      message: "Auth failed: device_activation_flow_expired",
    }),
    null,
  );
});

Deno.test("mapDeviceActivationFailure gives a helpful generic invalid request message", () => {
  assertEquals(
    mapDeviceActivationFailure(
      "flow_123",
      new AuthError({ reason: "invalid_request" }),
    ),
    {
      mode: "invalid_flow",
      flowId: "flow_123",
      reason:
        "Trellis rejected this activation request. Start again from the device.",
    },
  );
});

Deno.test("reviewer fallback is reserved for permission outcomes", () => {
  assertEquals(
    isDeviceActivationReviewerRequiredFailure(
      new AuthError({ reason: "not_authorized" }),
    ),
    true,
  );
  assertEquals(
    isDeviceActivationReviewerRequiredFailure(
      new AuthError({ reason: "forbidden" }),
    ),
    true,
  );
  assertEquals(
    isDeviceActivationReviewerRequiredFailure(
      new AuthError({ reason: "invalid_request" }),
    ),
    false,
  );
  assertEquals(
    isDeviceActivationReviewerRequiredFailure(
      new Error("transport failed"),
    ),
    false,
  );
  assertEquals(isDeviceActivationReviewerRequiredFailure(undefined), false);
});
