const CANONICAL_LOOPBACK_HOST = "localhost";
const LOOPBACK_HOSTS = new Set([
  "127.0.0.1",
  "::1",
  "[::1]",
  CANONICAL_LOOPBACK_HOST,
]);

type RuntimeAppConfig = {
  authUrl?: string;
};

function readRuntimeConfig(): RuntimeAppConfig {
  const config = (globalThis as typeof globalThis & {
    __TRELLIS_RUNTIME_CONFIG__?: RuntimeAppConfig;
  }).__TRELLIS_RUNTIME_CONFIG__;
  return config ?? {};
}

const runtimeConfig = readRuntimeConfig();
const viteEnv = (import.meta as ImportMeta & {
  env?: Record<string, string | undefined>;
}).env ?? {};

export const APP_CONFIG = {
  authUrl: runtimeConfig.authUrl ?? viteEnv["VITE_TRELLIS_AUTH_URL"] ??
    window.location.origin,
};

function normalizeConfiguredUrl(value: string): string {
  const url = new URL(value);
  url.hash = "";
  if (isLoopbackHost(url.hostname)) {
    url.hostname = CANONICAL_LOOPBACK_HOST;
  }
  return url.toString().replace(/\/$/, "");
}

function toUrl(location: URL | Location): URL {
  return location instanceof URL
    ? new URL(location.toString())
    : new URL(location.href);
}

function isLoopbackHost(hostname: string): boolean {
  return LOOPBACK_HOSTS.has(hostname);
}

export function getCanonicalLoopbackUrl(location: URL | Location): URL {
  const url = toUrl(location);

  if (isLoopbackHost(url.hostname)) {
    url.hostname = CANONICAL_LOOPBACK_HOST;
  }

  return url;
}

export function getCanonicalLoopbackRedirectUrl(
  location: URL | Location = globalThis.location,
): string | null {
  const current = toUrl(location);
  const canonical = getCanonicalLoopbackUrl(current);

  return canonical.toString() === current.toString()
    ? null
    : canonical.toString();
}

export function buildAppLoginUrl(
  redirectTo: string,
  location: URL | Location = globalThis.location,
  authError?: string,
  loginPath = "/login",
): string {
  const url = new URL(loginPath, getCanonicalLoopbackUrl(location).origin);
  url.searchParams.set("redirectTo", redirectTo);
  if (authError) {
    url.searchParams.set("authError", authError);
  }
  return url.toString();
}
