const DEFAULT_TRELLIS_URL = "http://localhost:3000";

function normalizeTrellisUrl(value: string, label: string): string {
  try {
    return new URL(value).toString().replace(/\/$/, "");
  } catch (error) {
    throw new Error(
      `Invalid ${label} ${JSON.stringify(value)}: ${(error as Error).message}`,
    );
  }
}

/**
 * Resolves the Trellis runtime URL used by the built-in portal.
 *
 * The packaged Trellis image must be deployment-agnostic, so an unset public URL
 * uses the browser origin that served the portal. The localhost default only
 * remains for non-browser local tooling.
 */
export function resolveTrellisUrl(
  publicTrellisUrl: string | undefined,
  browserOrigin: string | undefined,
): string {
  const configured = publicTrellisUrl?.trim();
  if (configured) {
    return normalizeTrellisUrl(configured, "PUBLIC_TRELLIS_URL");
  }

  if (browserOrigin) {
    return normalizeTrellisUrl(browserOrigin, "browser origin");
  }

  return DEFAULT_TRELLIS_URL;
}
