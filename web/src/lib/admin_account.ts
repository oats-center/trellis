/** Carries the signed Console intent through first-administrator setup. */
export function buildAdminAccountLoginUrl(
  loginUrl: string,
  adminAccountToken: string,
  location: URL | Location = globalThis.location,
): string | null {
  const resolvedLoginUrl = new URL(loginUrl, location.origin);
  const intent = resolvedLoginUrl.searchParams.get("intent");
  if (!intent) return null;
  const setupUrl = new URL("/login/admin/bootstrap", resolvedLoginUrl);
  setupUrl.searchParams.set("flowId", adminAccountToken);
  setupUrl.searchParams.set("intent", intent);
  return setupUrl.toString();
}

/** Remove and return the one-time administrator token before login starts. */
export function consumeAdminAccountToken(url: URL): string | null {
  const token = url.searchParams.get("adminAccountToken");
  if (token) url.searchParams.delete("adminAccountToken");
  return token;
}
