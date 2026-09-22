import { env } from "$env/dynamic/public";
import { resolveTrellisUrl } from "./trellis_url.ts";

function requirePublicTrellisUrl(): string {
  return resolveTrellisUrl(
    env.PUBLIC_TRELLIS_URL,
    typeof globalThis.location === "undefined"
      ? undefined
      : globalThis.location.origin,
  );
}

export const trellisUrl = requirePublicTrellisUrl();
