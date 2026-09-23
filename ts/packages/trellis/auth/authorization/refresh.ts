import { Value } from "typebox/value";
import { ulid } from "ulid";

import { decodeTrellisHttpError, TrellisHttpError } from "../http_error.ts";
import { recordCatalogCounter } from "../../telemetry/metrics.ts";
import type { TrellisAuth } from "../session_auth.ts";
import type { AuthorizationContextCache } from "./client_context.ts";
import type {
  AuthorizationContextRefreshResponse,
  AuthorizationContextRefreshResult,
  AuthorizationRuntimeBinding,
  VerifiedAuthorizationContext,
} from "./types.ts";
import { AuthorizationContextRefreshResponseSchema as ResponseSchema } from "./types.ts";

/** HTTP refresh failure with terminal-state classification. */
export class AuthorizationContextRefreshError extends TrellisHttpError {
  readonly terminal: boolean;
  readonly loginInvalid: boolean;

  constructor(status: number, code: string) {
    super(status, code);
    this.name = "AuthorizationContextRefreshError";
    this.terminal = [
      "session_not_found",
      "session_expired",
      "session_revoked",
      "identity_not_found",
      "identity_inactive",
      "user_not_found",
      "user_inactive",
      "login_not_found",
      "participant_not_found",
      "participant_changed",
      "contract_changed",
      "authority_not_found",
      "authority_rejected",
      "authority_revoked",
      "authority_expired",
      "context_owner_mismatch",
      "deployment_inactive",
      "instance_inactive",
      "device_inactive",
      "activation_required",
      "delegation_expired",
      "context_refresh_mismatch",
      "invalid_proof",
    ].includes(code);
    this.loginInvalid = [
      "session_not_found",
      "session_expired",
      "session_revoked",
      "identity_not_found",
      "identity_inactive",
      "user_not_found",
      "user_inactive",
      "login_not_found",
    ].includes(code);
  }
}

/** Refresh a context after proving possession of its bound session key. */
export async function refreshAuthorizationContextWithMetadata(args: {
  trellisUrl: string;
  sessionId: string;
  auth: TrellisAuth;
  sessionKey?: string;
  cache: AuthorizationContextCache;
  fetch?: typeof globalThis.fetch;
  shouldInstall?: () => boolean;
  requiredTransport?: "native" | "websocket";
  prepareInstall?: (
    response: AuthorizationContextRefreshResponse,
  ) => Promise<(verified: VerifiedAuthorizationContext) => void>;
  prepareOnly?: boolean;
}): Promise<AuthorizationContextRefreshResult> {
  const fetch = args.fetch ?? globalThis.fetch;
  const currentDigest = args.cache.storedContextDigest();
  let runtime: AuthorizationRuntimeBinding | undefined;
  try {
    runtime = args.cache.runtimeBinding();
  } catch {
    runtime = undefined;
  }
  if (runtime?.loginSessionId && runtime.loginSessionId !== args.sessionId) {
    throw new Error("authorization recovery session mismatch");
  }
  const requestStartedAt = args.cache.nowMilliseconds();
  const unsignedRequest = {
    requestId: ulid(),
    issuedAt: Math.trunc(args.auth.currentIat() * 1_000),
    loginSessionId: args.sessionId,
    connectionId: runtime?.connectionId ?? ulid(),
    sessionKey: args.sessionKey ?? args.auth.sessionKey,
    currentContextDigest: currentDigest ?? null,
  };
  const proof = await args.auth.signSessionProof({
    purpose: "authorizationContextRefresh",
    origin: new URL(args.trellisUrl).origin,
    sessionPublicKey: args.auth.sessionKey,
    unsignedRequest,
  });
  let outcome = "error";
  let response: Response;
  try {
    response = await fetch(
      new URL("/auth/context/refresh", args.trellisUrl),
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ ...unsignedRequest, proof }),
      },
    );
    if (!response.ok) {
      const error = await decodeTrellisHttpError(response);
      const classified = new AuthorizationContextRefreshError(
        error.status,
        error.code,
      );
      outcome = classified.terminal
        ? "terminal"
        : error.code === "resource_pending"
        ? "pending"
        : response.status === 503
        ? "unavailable"
        : "error";
      throw classified;
    }
    outcome = "ok";
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      outcome = "cancelled";
    }
    throw error;
  } finally {
    recordCatalogCounter("trellis.auth.refresh.attempts", 1, {
      "trellis.participant.kind": "user",
      "trellis.outcome": outcome,
    });
  }
  const next = Value.Parse(
    ResponseSchema,
    await response.json(),
  ) as AuthorizationContextRefreshResponse;
  if (
    args.requiredTransport &&
    !next.transports[args.requiredTransport]?.natsServers.length
  ) {
    throw new Error(
      `authorization refresh has no ${args.requiredTransport} NATS endpoints`,
    );
  }
  const serverClockOffsetMs = next.serverNow - Math.trunc(
    (requestStartedAt + args.cache.nowMilliseconds()) / 2,
  );
  args.cache.setServerClockOffsetMs(serverClockOffsetMs);
  args.auth.setServerClockOffsetMs(serverClockOffsetMs);
  if (args.shouldInstall?.() === false) {
    throw new Error("authorization context refresh stopped");
  }
  const nextRuntime = runtimeBindingFromResponse(next);
  const context = await args.cache[args.prepareOnly ? "prepare" : "install"](
    next.authorizationContext,
    {
      bootstrapJwt: next.routing.bootstrapJwt,
      bootstrapJwtExpiresAt: next.routing.bootstrapJwtExpiresAt,
    },
    Math.floor(next.serverNow / 1_000),
    args.shouldInstall,
    nextRuntime,
    async (verified) => {
      const installAdditional = await args.prepareInstall?.(next);
      return installAdditional ? () => installAdditional(verified) : undefined;
    },
  );
  return { context, response: next };
}

/** Refresh a context and return only its verified projection. */
export async function refreshAuthorizationContext(args: {
  trellisUrl: string;
  sessionId: string;
  auth: TrellisAuth;
  sessionKey?: string;
  cache: AuthorizationContextCache;
  fetch?: typeof globalThis.fetch;
  shouldInstall?: () => boolean;
}): Promise<VerifiedAuthorizationContext> {
  const result = await refreshAuthorizationContextWithMetadata(args);
  return result.context;
}

/** Start proactive refresh using the context's distributed refresh time. */
export function startAuthorizationContextRefresh(args: {
  trellisUrl: string;
  sessionId: string;
  auth: TrellisAuth;
  sessionKey?: string;
  cache: AuthorizationContextCache;
  fetch?: typeof globalThis.fetch;
  refresh?: (
    shouldInstall: () => boolean,
  ) => Promise<VerifiedAuthorizationContext>;
  onTerminalFailure?: (error: unknown) => void | Promise<void>;
  onTransientFailure?: (error: unknown) => void | Promise<void>;
  onRefresh?: (context: VerifiedAuthorizationContext) => void | Promise<void>;
}): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let failures = 0;
  let running = false;
  let wakePending = false;
  const schedule = (delayMs: number) => {
    if (!stopped) timer = setTimeout(run, delayMs);
  };
  const run = async () => {
    if (running) {
      wakePending = true;
      return;
    }
    running = true;
    const clearGuard = args.cache.clearGuard();
    try {
      let before: string | undefined;
      try {
        before = args.cache.storedContextDigest();
      } catch {
        before = undefined;
      }
      const context = args.refresh
        ? await args.refresh(() => !stopped)
        : (await refreshAuthorizationContextWithMetadata({
          ...args,
          shouldInstall: () => !stopped,
          prepareOnly: true,
        })).context;
      if (stopped) return;
      failures = 0;
      await args.onRefresh?.(context);
      schedule(
        refreshDelay(
          args.cache,
          before === context.contextDigest ? 5_000 : 1_000,
        ),
      );
    } catch (error) {
      if (stopped) return;
      if (error instanceof AuthorizationContextRefreshError && error.terminal) {
        if (!(await args.cache.clearIfCurrent(clearGuard))) {
          failures = 0;
          schedule(refreshDelay(args.cache, 1_000));
          return;
        }
        await args.onTerminalFailure?.(error);
        return;
      }
      failures += 1;
      let current: VerifiedAuthorizationContext | undefined;
      try {
        current = args.cache.current();
      } catch {
        current = undefined;
      }
      await args.onTransientFailure?.(error);
      const beforeExpiry = current
        ? Math.max(
          1_000,
          (current.context.expiresAt - args.cache.correctedNowSeconds()) *
            1_000,
        )
        : Number.POSITIVE_INFINITY;
      schedule(Math.min(beforeExpiry, 5_000 * 2 ** Math.min(failures - 1, 3)));
    } finally {
      running = false;
      if (wakePending && !stopped) {
        wakePending = false;
        if (timer !== undefined) clearTimeout(timer);
        schedule(0);
      }
    }
  };
  schedule(refreshDelay(args.cache));
  let unregisterRefreshRequest: () => void;
  try {
    unregisterRefreshRequest = args.cache.registerRefreshRequest(() => {
      if (running) {
        wakePending = true;
        return;
      }
      if (timer !== undefined) clearTimeout(timer);
      schedule(0);
    });
  } catch (error) {
    stopped = true;
    if (timer !== undefined) clearTimeout(timer);
    throw error;
  }
  return () => {
    stopped = true;
    unregisterRefreshRequest();
    if (timer !== undefined) clearTimeout(timer);
  };
}

function refreshDelay(
  cache: AuthorizationContextCache,
  minimumMs = 1_000,
): number {
  try {
    return Math.max(
      minimumMs,
      (cache.routingRefreshAt() - cache.correctedNowSeconds()) * 1_000,
    );
  } catch {
    return minimumMs;
  }
}

function runtimeBindingFromResponse(
  response: AuthorizationContextRefreshResponse,
): AuthorizationRuntimeBinding {
  if (
    !response.transports.native &&
    !response.transports.websocket
  ) {
    throw new Error("authorization refresh returned no NATS transport");
  }
  return {
    connectionId: response.runtime.connectionId,
    loginSessionId: response.runtime.loginSessionId,
    participantId: response.runtime.participantId,
    inboxPrefix: response.runtime.inboxPrefix,
    transports: {
      ...(response.transports.native === undefined ? {} : {
        native: {
          natsServers: [...response.transports.native.natsServers],
        },
      }),
      ...(response.transports.websocket === undefined ? {} : {
        websocket: {
          natsServers: [...response.transports.websocket.natsServers],
        },
      }),
    },
  };
}
