import type { ClientAuthOptions } from "@oatscenter/trellis";

type RuntimeAppConfig = {
  authUrl: string;
  clientId: string;
  callbackUri: string;
  loopbackDevelopment: boolean;
};

function readRuntimeConfig(): RuntimeAppConfig {
  const config = (
    globalThis as typeof globalThis & {
      __TRELLIS_RUNTIME_CONFIG__?: RuntimeAppConfig;
    }
  ).__TRELLIS_RUNTIME_CONFIG__;
  if (
    !config ||
    typeof config.authUrl !== "string" ||
    typeof config.clientId !== "string" ||
    !config.clientId ||
    typeof config.callbackUri !== "string" ||
    typeof config.loopbackDevelopment !== "boolean"
  )
    throw new Error("Console OAuth configuration is missing or invalid");
  const callback = new URL(config.callbackUri);
  if (
    callback.origin !== location.origin ||
    callback.pathname !== "/login" ||
    callback.search ||
    callback.hash ||
    callback.username ||
    callback.password
  )
    throw new Error(
      "Console OAuth callback does not match this application origin",
    );
  return config;
}

/**
 * Returns whether the Console is served with Trellis-generated certificates,
 * which every user is told may not be suitable for production.
 */
export function usesGeneratedTls(): boolean {
  return (
    (
      globalThis as typeof globalThis & {
        __TRELLIS_RUNTIME_CONFIG__?: { generatedTls?: unknown };
      }
    ).__TRELLIS_RUNTIME_CONFIG__?.generatedTls === true
  );
}

export const APP_CONFIG: RuntimeAppConfig = {
  get authUrl() {
    return readRuntimeConfig().authUrl;
  },
  get clientId() {
    return readRuntimeConfig().clientId;
  },
  get callbackUri() {
    return readRuntimeConfig().callbackUri;
  },
  get loopbackDevelopment() {
    return readRuntimeConfig().loopbackDevelopment;
  },
};

/** The Console's OAuth identity; its generated request already asks for administration optionally. */
export function consoleAuthOptions(): ClientAuthOptions {
  return {
    clientId: APP_CONFIG.clientId,
    redirectTo: APP_CONFIG.callbackUri,
    loopbackDevelopment: APP_CONFIG.loopbackDevelopment,
  };
}

function toUrl(location: URL | Location): URL {
  return location instanceof URL
    ? new URL(location.toString())
    : new URL(location.href);
}

export function buildAppLoginUrl(
  redirectTo: string,
  location: URL | Location = globalThis.location,
  authError?: string,
  loginPath = "/login",
): string {
  const url = new URL(loginPath, toUrl(location).origin);
  url.searchParams.set("redirectTo", redirectTo);
  if (authError) {
    url.searchParams.set("authError", authError);
  }
  return url.toString();
}
