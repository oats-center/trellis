import { TrellisClient } from "@oats-center/trellis";
import {
  createDeviceActivationController,
  type DeviceActivationAuth,
  type DeviceActivationOperationRef,
} from "@oats-center/trellis-svelte";
import { participant as portalParticipant } from "../../../ts/packages/trellis/internal_sdk/generated/participants/portal/mod.js";
import { trellisUrl } from "./portal_config.ts";

type PortalAuthState = DeviceActivationAuth;

function createPortalAuthState(
  onCallback: (flowId: string) => void,
): PortalAuthState {
  return {
    async init() {},
    async handleCallback(callbackUrl) {
      const url = new URL(callbackUrl);
      const flowId = url.searchParams.get("flowId");
      const deviceFlowId = url.searchParams.get("deviceFlowId");
      if (!flowId || flowId === deviceFlowId) return null;

      onCallback(flowId);
      return null;
    },
    async signIn(options) {
      const redirectTo = new URL(
        options?.redirectTo ?? "/login",
        window.location.href,
      ).toString();
      const currentUrl = new URL(window.location.href);
      currentUrl.searchParams.delete("flowId");
      currentUrl.searchParams.delete("deviceFlowId");
      await TrellisClient.connect({
        trellisUrl,
        participant: portalParticipant,
        auth: { currentUrl, redirectTo, context: options?.context },
        onAuthRequired: ({ loginUrl }) => {
          window.location.href = loginUrl;
          throw new Error("Browser authentication redirect started");
        },
      }).orThrow();
    },
  };
}

/**
 * Creates the device activation controller used by the portal route.
 */
export function createPortalDeviceActivationController() {
  let callbackFlowId: string | undefined;
  const authState = createPortalAuthState((flowId) => {
    callbackFlowId = flowId;
  });

  return createDeviceActivationController({
    authState,
    createClient: async (authUrlState) => {
      const trellis = await TrellisClient.connect({
        trellisUrl,
        auth: {
          currentUrl: authUrlState.currentUrl,
          redirectTo: authUrlState.redirectTo,
          flowId: callbackFlowId,
        },
        onAuthRequired: () => ({ status: "handled" }),
        participant: portalParticipant,
      }).orThrow();
      callbackFlowId = undefined;

      return {
        async activateDevice(input): Promise<DeviceActivationOperationRef> {
          return await trellis.deviceUserAuthoritiesResolve(input)
            .start()
            .orThrow();
        },
      };
    },
    sessionStorage: typeof window === "undefined"
      ? undefined
      : window.sessionStorage,
  });
}
