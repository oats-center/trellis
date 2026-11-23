import { resolve } from "$app/paths";
import { AuthError } from "@oatscenter/trellis";
import { APP_CONFIG, buildAppLoginUrl } from "./config.ts";
import { connectionProblem } from "./connection_problem.ts";

/** This tab's post-OAuth destination, isolated by exact application and issuer identity. */
export function consoleReturnKey(): string {
  return JSON.stringify([
    location.origin,
    APP_CONFIG.authUrl,
    APP_CONFIG.clientId,
    "console-return",
  ]);
}

const SIGN_IN_ATTEMPTS = 3;
const SIGN_IN_WINDOW_MS = 30_000;

function signInAttemptsKey(): string {
  return JSON.stringify([
    location.origin,
    APP_CONFIG.authUrl,
    APP_CONFIG.clientId,
    "console-sign-in-attempts",
  ]);
}

/**
 * Records that this tab is about to leave for sign-in on its own, and says
 * whether it may. A tab leaves at most three times in 30 seconds: a sign-in that
 * keeps coming back unfinished would otherwise reload the page forever, so
 * the caller stops on the sign-in problem page instead.
 */
export function allowAutomaticSignIn(): boolean {
  const now = Date.now();
  let recent: number[] = [];
  try {
    const stored: unknown = JSON.parse(
      sessionStorage.getItem(signInAttemptsKey()) ?? "[]",
    );
    if (Array.isArray(stored))
      recent = stored.filter(
        (at): at is number =>
          typeof at === "number" && at <= now && now - at < SIGN_IN_WINDOW_MS,
      );
  } catch {
    // Unreadable attempts count as none; sign-in itself needs this storage.
  }
  if (recent.length >= SIGN_IN_ATTEMPTS) return false;
  try {
    sessionStorage.setItem(
      signInAttemptsKey(),
      JSON.stringify([...recent, now]),
    );
  } catch {
    // Without session storage the sign-in that follows fails and says so.
  }
  return true;
}

/** Forgets this tab's automatic sign-in attempts when a person starts one. */
export function resetAutomaticSignIn(): void {
  try {
    sessionStorage.removeItem(signInAttemptsKey());
  } catch {
    // Nothing was stored.
  }
}

/** What the sign-in problem page tells a person. */
export type SignInProblem = {
  /** What happened, in one plain sentence. */
  summary: string;
  /** The server's reference for its own failure, for whoever reads its logs. */
  reference?: string;
  /** The exact reason, for support. */
  detail?: string;
};

/** Shown when this tab stopped leaving for sign-in on its own. */
export const SIGN_IN_STOPPED: SignInProblem = {
  summary: "Sign-in kept starting over, so the Console stopped trying.",
  detail: `More than ${SIGN_IN_ATTEMPTS} automatic sign-ins in ${SIGN_IN_WINDOW_MS / 1000} seconds`,
};

/** Says why sign-in stopped, with the server's reference when it gave one. */
export function signInProblem(error: unknown): SignInProblem {
  if (!(error instanceof AuthError))
    return { summary: connectionProblem(error) };
  const context = error.getContext();
  const refused = [
    typeof context.error === "string" ? context.error : undefined,
    typeof context.status === "number" ? `HTTP ${context.status}` : undefined,
  ].filter((part) => part !== undefined);
  const detail = refused.length
    ? `${error.reason} (${refused.join(", ")})`
    : error.reason;
  const reference = /Reference: (\S+)/.exec(error.message)?.[1];
  const summary = (() => {
    switch (error.reason) {
      case "OAuthAccountChanged":
        return "This browser is already signed in to the Console as someone else.";
      case "OAuthAuthorizationDenied":
        return "Access for the Console was not allowed.";
      case "InvalidOAuthCallback":
        return "This sign-in link has expired or belongs to another sign-in.";
      case "OAuthUnavailable":
        return "Trellis couldn't finish signing you in.";
      case "OAuthRecoveryRequired":
      case "InvalidCredential":
        return "Trellis didn't accept this sign-in.";
      default:
        return connectionProblem(error);
    }
  })();
  return { summary, ...(reference ? { reference } : {}), detail };
}

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
    return `${appBase}${url.pathname.slice(
      appBase.length,
    )}${url.search}${url.hash}`;
  }
  return `${appBase}${url.pathname}${url.search}${url.hash}`;
}

export function getConsoleRedirectTarget(
  location: URL | Location = globalThis.location,
  fallback = "/console/profile",
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
    resolve("/console/login"),
  );
}
