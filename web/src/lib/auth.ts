import { resolve } from "$app/paths";
import { APP_CONFIG, buildAppLoginUrl } from "./config.ts";

/** Converts auth callback reason codes into console-facing messages. */
export function formatConsoleAuthError(error: string): string {
  switch (error) {
    case "approval_denied":
      return "App access was denied.";
    case "flow_expired":
      return "This sign-in request expired. Start sign-in again.";
    default:
      return error;
  }
}

function toUrl(location: URL | Location): URL {
  return location instanceof URL
    ? new URL(location.toString())
    : new URL(location.href);
}

export function resolveConsolePath(
  path: string,
  location: URL | Location = globalThis.location,
): string {
  const url = new URL(path, toUrl(location));
  const appBase = resolve("/").replace(/\/$/, "");
  const currentUrl = toUrl(location);
  if (url.origin !== currentUrl.origin) return url.toString();
  if (appBase && url.pathname === appBase) {
    return `${appBase}/${url.search}${url.hash}`;
  }
  if (appBase && url.pathname.startsWith(`${appBase}/`)) {
    return `${appBase}${
      url.pathname.slice(appBase.length)
    }${url.search}${url.hash}`;
  }
  return `${appBase}${url.pathname}${url.search}${url.hash}`;
}

export function getConsoleRedirectTarget(
  location: URL | Location = globalThis.location,
  fallback = "/profile",
): string {
  const currentUrl = toUrl(location);
  const redirectTo = currentUrl.searchParams.get("redirectTo");
  if (!redirectTo) return resolveConsolePath(fallback, currentUrl);
  const redirectUrl = new URL(redirectTo, currentUrl);
  return redirectUrl.origin === currentUrl.origin
    ? resolveConsolePath(redirectTo, currentUrl)
    : resolveConsolePath(fallback, currentUrl);
}

export function buildConsoleLoginUrl(options: {
  redirectTo: string;
  location?: URL | Location;
  authError?: string;
}): string {
  const location = options.location ?? globalThis.location;
  return buildAppLoginUrl(
    resolveConsolePath(options.redirectTo, location),
    location,
    options.authError,
    resolve("/login"),
  );
}
