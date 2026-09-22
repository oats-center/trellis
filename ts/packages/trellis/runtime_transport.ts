import { wsconnect } from "@nats-io/nats-core";
import type { Authenticator, NatsConnection } from "@nats-io/nats-core";

type RuntimeTransportEndpoints = {
  natsServers: string[];
};

type RuntimeTransports = {
  native?: RuntimeTransportEndpoints;
  websocket?: RuntimeTransportEndpoints;
};

export const DEFAULT_RUNTIME_MAX_RECONNECT_ATTEMPTS = -1;
export const DEFAULT_SERVICE_RUNTIME_WAIT_ON_FIRST_CONNECT = true;

export type RuntimeTransportConnectOptions = {
  servers: string | string[];
  token?: string;
  authenticator?: Authenticator | Authenticator[];
  inboxPrefix?: string;
  maxReconnectAttempts?: number;
  waitOnFirstConnect?: boolean;
} & Record<string, unknown>;

export type RuntimeTransport = {
  connect(options: RuntimeTransportConnectOptions): Promise<NatsConnection>;
};

type NativeGlobalThis = typeof globalThis & {
  document?: unknown;
  window?: unknown;
};

export function selectRuntimeTransportServers(
  transports: RuntimeTransports,
): string[] {
  if (isBrowserRuntime()) {
    if (transports.websocket?.natsServers?.length) {
      return transports.websocket.natsServers;
    }
    if (transports.native?.natsServers?.length) {
      return transports.native.natsServers;
    }
  } else {
    if (transports.native?.natsServers?.length) {
      return transports.native.natsServers;
    }
    if (transports.websocket?.natsServers?.length) {
      return transports.websocket.natsServers;
    }
  }

  throw new Error("No supported NATS transport endpoints available");
}

function isBrowserRuntime(): boolean {
  const browserGlobal = globalThis as NativeGlobalThis;
  return typeof browserGlobal.window !== "undefined" &&
    typeof browserGlobal.document !== "undefined";
}

function usesWebSocketTransport(servers: string | string[]): boolean {
  const values = Array.isArray(servers) ? servers : [servers];
  return values.some((server) =>
    server.startsWith("ws://") || server.startsWith("wss://")
  );
}

/** Loads WebSocket transport in browsers and native Node transport in Node/Deno. */
export async function loadDefaultRuntimeTransport(): Promise<RuntimeTransport> {
  if (isBrowserRuntime()) {
    return {
      connect: wsconnect,
    };
  }

  return {
    connect: async (options) => {
      if (usesWebSocketTransport(options.servers)) {
        return await wsconnect(options);
      }
      const { connect } = await import("@nats-io/transport-node");
      return await connect(options);
    },
  };
}
