import type { NatsConnection } from "@nats-io/nats-core";

// Node and Deno share the native transport loaded by runtime_transport.ts.
export type NatsConnectOpts = {
  servers: string | string[];
  token?: string;
  inboxPrefix?: string;
  authenticator?: unknown;
  maxReconnectAttempts?: number;
  waitOnFirstConnect?: boolean;
} & Record<string, unknown>;

export type NatsConnectFn = (opts: NatsConnectOpts) => Promise<NatsConnection>;

/** Initializes telemetry for a service runtime and resolves when ready. */
export type InitTelemetryFn = (serviceName: string) => void | Promise<void>;

export type TrellisServiceRuntimeDeps = {
  connect: NatsConnectFn;
  initTelemetry?: InitTelemetryFn;
};
