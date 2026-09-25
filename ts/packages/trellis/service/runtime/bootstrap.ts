import type { NatsConnection } from "@nats-io/nats-core";
import { Type } from "typebox";
import { Value } from "typebox/value";
import { ulid } from "ulid";
import {
  base64urlDecode,
  base64urlEncode,
  estimateMidpointClockOffsetMs,
  sha256,
  type TrellisAuth as SessionAuth,
} from "../../auth.ts";
import {
  type AuthorizationContextBundle,
  AuthorizationContextRefreshError,
} from "../../auth/authorization_context.ts";
import { AuthorizationContextRefreshResponseSchema } from "../../auth/authorization/types.ts";
import {
  decodeTrellisHttpError,
  isRetriableAuthorizationCode,
  TrellisHttpError,
} from "../../auth/http_error.ts";
import { ContractResourceBindingsSchema } from "../../participant.ts";
import type { RuntimeApi } from "../../participant_runtime/api.ts";
import { participantEvidence } from "../../participant_runtime/participant.ts";
import { TransportError } from "../../errors/index.ts";
import type { LoggerLike } from "../../globals.ts";
import { loadDefaultRuntimeTransport } from "../../runtime_transport.ts";
import { initTelemetry } from "../../telemetry/init.ts";
import { recordCatalogCounter } from "../../telemetry/metrics.ts";
import type { TrellisServiceRuntimeDeps } from "./runtime.ts";
import type {
  GeneratedServiceParticipant,
  ResourceBindings,
} from "./service.ts";

type ServiceBootstrapConnectInfo = {
  connectionId: string;
  participantId: string;
  participantDigest: string;
  contractId: string;
  contractDigest: string;
  transports: {
    native?: { natsServers: string[] };
    websocket?: { natsServers: string[] };
  };
  jwt: string;
  jwtExpiresAt: number;
  authorizationContext: AuthorizationContextBundle;
};

/** Validated bootstrap material used to create a service runtime session. */
export type ServiceBootstrapResponse = {
  status: "ready";
  serverNow: number;
  serverClockOffsetMs: number;
  connectInfo: ServiceBootstrapConnectInfo;
  binding: {
    contractId: string;
    digest: string;
    resources: ResourceBindings;
    apiBindings: Readonly<Record<string, unknown>>;
  };
};

const DEFAULT_BOOTSTRAP_UNAVAILABLE_INITIAL_RETRY_MS = 1_000;
const MAX_BOOTSTRAP_UNAVAILABLE_RETRY_MS = 30_000;

function getErrorCauseMessage(error: unknown): string {
  if (error && typeof error === "object") {
    const context = (error as { context?: Record<string, unknown> }).context;
    if (
      typeof context?.causeMessage === "string" &&
      context.causeMessage.length > 0
    ) {
      return context.causeMessage;
    }
  }

  return error instanceof Error ? error.message : String(error);
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function bootstrapUnavailableRetryDelayMs(attempt: number): number {
  const exponent = Math.min(attempt, 10);
  return Math.min(
    DEFAULT_BOOTSTRAP_UNAVAILABLE_INITIAL_RETRY_MS * 2 ** exponent,
    MAX_BOOTSTRAP_UNAVAILABLE_RETRY_MS,
  );
}

class ServiceBootstrapEndpointUnavailableError extends Error {
  constructor(cause: unknown) {
    super("Service bootstrap endpoint is unavailable.", { cause });
    this.name = "ServiceBootstrapEndpointUnavailableError";
  }
}

/** Loads the default transport and telemetry dependencies for service connection. */
export async function loadDefaultServiceRuntimeDeps(): Promise<
  TrellisServiceRuntimeDeps
> {
  const transport = await loadDefaultRuntimeTransport();
  return {
    initTelemetry,
    connect: (
      { servers, token, authenticator, inboxPrefix, ...extraOptions },
    ) =>
      transport.connect({
        servers,
        ...extraOptions,
        ...(token ? { token } : {}),
        ...(authenticator ? { authenticator: authenticator as never } : {}),
        ...(inboxPrefix ? { inboxPrefix } : {}),
      }),
  };
}

const ServiceBootstrapReadySchema = Type.Object({
  ...AuthorizationContextRefreshResponseSchema.properties,
  authorization: Type.Object({
    participantId: Type.String({ minLength: 1 }),
    participantDigest: Type.String({ minLength: 1 }),
    resourceRuntime: ContractResourceBindingsSchema,
  }),
});

async function fetchServiceBootstrapInfoOnce(args: {
  bootstrapUrl: URL;
  contractId: string;
  contractDigest: string;
  contract: GeneratedServiceParticipant<RuntimeApi, RuntimeApi | undefined>;
  identityAuth: SessionAuth;
  sessionAuth: SessionAuth;
  connectionId: string;
  name?: string;
  /** Whether this issued request is a credential-bound refresh. */
  refreshAttempt: boolean;
}): Promise<{
  response: Response;
  payload: unknown;
  requestStartedAtMs: number;
  responseReceivedAtMs: number;
}> {
  const requestStartedAtMs = Date.now();
  const requestId = ulid();
  const issuedAt = args.identityAuth.currentIat() * 1_000;
  const provisionedIdentityKeyId = base64urlEncode(
    await sha256(base64urlDecode(args.identityAuth.sessionKey)),
  );
  const unsigned = {
    identityKeyId: provisionedIdentityKeyId,
    sessionKey: args.sessionAuth.sessionKey,
    connectionId: args.connectionId,
    requestId,
    iat: issuedAt,
    ...participantEvidence(args.contract),
    ...(args.name === undefined ? {} : { name: args.name }),
  };
  const body = JSON.stringify({
    ...unsigned,
    proof: await args.identityAuth.signSessionProof({
      purpose: "serviceBootstrap",
      origin: args.bootstrapUrl.origin,
      unsignedRequest: unsigned,
    }),
  });
  let outcome = "error";
  let response: Response;
  try {
    response = await fetch(args.bootstrapUrl, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body,
    });
    if (!response.ok) {
      const error = await decodeTrellisHttpError(response);
      outcome = new AuthorizationContextRefreshError(
          error.status,
          error.code,
        ).terminal
        ? "terminal"
        : isRetriableAuthorizationCode(error.code)
        ? "pending"
        : error.status === 503
        ? "unavailable"
        : "error";
      throw error;
    }
    outcome = "ok";
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === "AbortError") {
      outcome = "cancelled";
      throw cause;
    }
    if (!(cause instanceof TrellisHttpError)) {
      outcome = "unavailable";
      throw new ServiceBootstrapEndpointUnavailableError(cause);
    }
    throw cause;
  } finally {
    if (args.refreshAttempt) {
      try {
        recordCatalogCounter("trellis.auth.refresh.attempts", 1, {
          "trellis.participant.kind": "service",
          "trellis.outcome": outcome,
        });
      } catch {
        // Optional telemetry must never replace the bootstrap result.
      }
    }
  }
  const responseReceivedAtMs = Date.now();

  let payload: unknown;
  try {
    payload = await response.json();
  } catch {
    payload = undefined;
  }
  return {
    response,
    payload,
    requestStartedAtMs,
    responseReceivedAtMs,
  };
}

/** Fetches and validates service bootstrap material, retrying pending states. */
export async function fetchServiceBootstrapInfo(args: {
  trellisUrl: string;
  serviceName: string;
  name?: string;
  contractId: string;
  contractDigest: string;
  contract: GeneratedServiceParticipant<RuntimeApi, RuntimeApi | undefined>;
  identityAuth: SessionAuth;
  sessionAuth: SessionAuth;
  log: LoggerLike;
  connectionId?: string;
}): Promise<ServiceBootstrapResponse> {
  const bootstrapUrl = new URL("/bootstrap/service", args.trellisUrl);
  const connectionId = args.connectionId ?? ulid();
  let unavailableAttempt = 0;
  while (true) {
    let settled: Awaited<ReturnType<typeof fetchServiceBootstrapInfoOnce>>;
    try {
      settled = await fetchServiceBootstrapInfoOnce({
        ...args,
        bootstrapUrl,
        connectionId,
        refreshAttempt: args.connectionId !== undefined,
      });
      unavailableAttempt = 0;
    } catch (cause) {
      if (
        cause instanceof TrellisHttpError &&
        isRetriableAuthorizationCode(cause.code)
      ) {
        await delay(bootstrapUnavailableRetryDelayMs(unavailableAttempt));
        unavailableAttempt += 1;
        continue;
      }
      if (!(cause instanceof ServiceBootstrapEndpointUnavailableError)) {
        throw cause;
      }

      const retryDelayMs = bootstrapUnavailableRetryDelayMs(unavailableAttempt);
      unavailableAttempt += 1;
      args.log.warn(
        {
          service: args.serviceName,
          trellisUrl: args.trellisUrl,
          contractId: args.contractId,
          contractDigest: args.contractDigest,
          attempt: unavailableAttempt,
          retryDelayMs,
          causeMessage: getErrorCauseMessage(cause.cause),
        },
        "Service bootstrap endpoint unavailable; retrying",
      );
      await delay(retryDelayMs);
      continue;
    }

    if (settled.payload === undefined) {
      throw new TransportError({
        code: "trellis.bootstrap.invalid_response",
        message: "Service bootstrap returned invalid JSON.",
        hint:
          "Retry the connection. If it keeps happening, check the Trellis deployment.",
        context: {
          trellisUrl: args.trellisUrl,
          contractId: args.contractId,
          contractDigest: args.contractDigest,
        },
      });
    }

    const response = Value.Parse(
      ServiceBootstrapReadySchema,
      settled.payload,
    );
    const serverClockOffsetMs = estimateMidpointClockOffsetMs({
      requestStartedAtMs: settled.requestStartedAtMs,
      responseReceivedAtMs: settled.responseReceivedAtMs,
      serverNowSeconds: response.serverNow / 1_000,
    });
    args.identityAuth.setServerClockOffsetMs(serverClockOffsetMs);
    args.sessionAuth.setServerClockOffsetMs(serverClockOffsetMs);
    const native = response.transports.native;
    if (!native) {
      throw new TransportError({
        code: "trellis.bootstrap.invalid_response",
        message: "Service bootstrap returned no native NATS transport.",
        hint: "Configure native NATS endpoints for the Trellis runtime.",
      });
    }
    return {
      status: "ready",
      serverNow: response.serverNow / 1_000,
      serverClockOffsetMs,
      connectInfo: {
        connectionId: response.runtime.connectionId,
        participantId: response.runtime.participantId,
        participantDigest: response.authorization.participantDigest,
        contractId: args.contractId,
        contractDigest: args.contractDigest,
        transports: { native: { natsServers: native.natsServers } },
        jwt: response.routing.bootstrapJwt,
        jwtExpiresAt: response.routing.bootstrapJwtExpiresAt,
        authorizationContext: response.authorizationContext,
      },
      binding: {
        contractId: args.contractId,
        digest: response.authorization.participantDigest,
        apiBindings: response.apiBindings,
        resources: {
          kv: response.authorization.resourceRuntime.kv ?? {},
          store: response.authorization.resourceRuntime.store ?? {},
          ...(response.authorization.resourceRuntime.jobs === undefined
            ? {}
            : { jobs: response.authorization.resourceRuntime.jobs }),
          ...(response.authorization.resourceRuntime.eventConsumers ===
              undefined
            ? {}
            : {
              eventConsumers:
                response.authorization.resourceRuntime.eventConsumers,
            }),
        },
      },
    };
  }
}

/** Closes a NATS connection created by an unsuccessful bootstrap attempt. */
export async function closeFailedServiceBootstrapConnection(
  nc: NatsConnection,
): Promise<void> {
  const closed = nc.closed().catch(() => undefined);
  if (nc.isClosed()) {
    await closed;
    return;
  }

  try {
    await nc.close();
  } catch {
    // The connection closure is the lifecycle result, including auth revocation.
  }
  await closed;
}
