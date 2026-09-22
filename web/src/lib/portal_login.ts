import { createPortalFlow } from "@qlever-llc/trellis-svelte";
import { trellisUrl } from "$lib/portal_config";

/**
 * Creates the login portal flow for the current page URL.
 */
export function createLoginPortalFlow(getUrl: () => URL) {
  return createPortalFlow({
    authUrl: trellisUrl,
    getUrl,
  });
}
