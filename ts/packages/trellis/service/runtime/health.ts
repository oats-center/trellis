/**
 * Health check types and utility functions for Trellis services.
 *
 * This module provides a standardized way to implement health checks
 * for services, with support for individual check results and
 * aggregated health status (healthy, degraded, unhealthy).
 *
 * @module
 */

import type { JsonValue } from "../../participant.ts";
import { ulid } from "ulid";
import {
  type HealthHeartbeatSample,
  HealthHeartbeatSampleCodec,
} from "../../internal_sdk/generated/types/_internal/p0.js";

type MaybePromise<T> = T | Promise<T>;

/**
 * Result of a single health check.
 */
export type HealthCheckResult = {
  /** Name of the health check */
  name: string;
  /** Status of the check: "ok" if passed, "failed" if not */
  status: "ok" | "failed";
  /** Error message if the check failed with an error */
  error?: string;
  /** Optional short human-readable summary */
  summary?: string;
  /** Optional structured metadata for the check */
  info?: Record<string, JsonValue>;
  /** Time in milliseconds the check took to execute */
  latencyMs: number;
};

export type ServiceHealthInfo = {
  version?: string;
  info?: Record<string, JsonValue>;
};

export type ServiceHealthCheck = {
  status: "ok" | "failed";
  summary?: string;
  info?: Record<string, JsonValue>;
  error?: string;
};

export type ServiceHealthCheckFn = () => MaybePromise<ServiceHealthCheck>;
export type ServiceHealthInfoFn = () => MaybePromise<
  ServiceHealthInfo | undefined
>;

function summarizeHealthStatus(
  results: readonly Pick<HealthCheckResult, "status">[],
): HealthResponse["status"] {
  const allOk = results.every((r) => r.status === "ok");
  const anyOk = results.some((r) => r.status === "ok");
  return allOk ? "healthy" : anyOk ? "degraded" : "unhealthy";
}

function summarizeHealthChecks(
  results: readonly HealthCheckResult[],
): string | undefined {
  const failedCount = results.filter((r) => r.status === "failed").length;
  if (failedCount === 0) {
    return undefined;
  }

  return `${failedCount} check${failedCount === 1 ? "" : "s"} failing`;
}

function detectRuntime(): {
  runtime: HealthHeartbeatSample["participant"]["runtime"];
  runtimeVersion?: string;
} {
  const maybeDeno = Reflect.get(globalThis, "Deno") as
    | { version?: { deno?: string } }
    | undefined;
  if (maybeDeno?.version?.deno) {
    return { runtime: "deno", runtimeVersion: maybeDeno.version.deno };
  }

  const maybeProcess = Reflect.get(globalThis, "process") as
    | { version?: string }
    | undefined;
  if (typeof maybeProcess?.version === "string") {
    return { runtime: "node", runtimeVersion: maybeProcess.version };
  }

  return { runtime: "unknown" };
}

/**
 * Aggregated health response for a service.
 */
export type HealthResponse = {
  /** Overall status: "healthy" if all pass, "unhealthy" if all fail, "degraded" if mixed */
  status: "healthy" | "unhealthy" | "degraded";
  /** Name of the service */
  service: string;
  /** ISO timestamp of when the health check was performed */
  timestamp: string;
  /** Individual health check results */
  checks: HealthCheckResult[];
};

export async function runServiceHealthCheck(
  name: string,
  check: ServiceHealthCheckFn,
): Promise<HealthCheckResult> {
  const start = performance.now();
  try {
    const result = await check();
    const latencyMs = performance.now() - start;
    return {
      name,
      status: result.status,
      error: result.error,
      summary: result.summary,
      info: result.info,
      latencyMs,
    };
  } catch (error) {
    const latencyMs = performance.now() - start;
    const message = error instanceof Error ? error.message : String(error);
    return {
      name,
      status: "failed",
      error: message,
      summary: message,
      latencyMs,
    };
  }
}

export async function runAllServiceHealthChecks(
  service: string,
  checks: Record<string, ServiceHealthCheckFn>,
): Promise<HealthResponse> {
  const results = await Promise.all(
    Object.entries(checks).map(([name, fn]) => runServiceHealthCheck(name, fn)),
  );

  return {
    status: summarizeHealthStatus(results),
    service,
    timestamp: new Date().toISOString(),
    checks: results,
  };
}

export function createHealthHeartbeatSample(args: {
  serviceName: string;
  kind?: HealthHeartbeatSample["participant"]["kind"];
  instanceId: string;
  contractId: string;
  contractDigest: string;
  startedAt: string;
  publishIntervalMs: number;
  checks: HealthCheckResult[];
  info?: ServiceHealthInfo;
}): HealthHeartbeatSample {
  const runtime = detectRuntime();
  const summary = summarizeHealthChecks(args.checks);

  return HealthHeartbeatSampleCodec.decode({
    sample: {
      id: ulid(),
      time: new Date().toISOString(),
    },
    participant: {
      name: args.serviceName,
      kind: args.kind ?? "service",
      instanceId: args.instanceId,
      contractId: args.contractId,
      contractDigest: args.contractDigest,
      startedAt: args.startedAt,
      publishIntervalMs: String(args.publishIntervalMs),
      runtime: runtime.runtime,
      ...(runtime.runtimeVersion
        ? { runtimeVersion: runtime.runtimeVersion }
        : {}),
      ...(args.info?.version ? { version: args.info.version } : {}),
      ...(args.info?.info ? { info: args.info.info } : {}),
    },
    reportedStatus: summarizeHealthStatus(args.checks),
    ...(summary ? { summary } : {}),
    checks: args.checks,
  });
}

/** Public health enrichment surface exposed by connected participants. */
export type ServiceHealth = Readonly<{
  setInfo(info: ServiceHealthInfo | ServiceHealthInfoFn): void;
  add(name: string, check: ServiceHealthCheckFn): () => void;
}>;

/** Internal health runtime used by automatic heartbeat publishers. */
export class ServiceHealthRuntime implements ServiceHealth {
  readonly serviceName: string;
  readonly kind: HealthHeartbeatSample["participant"]["kind"];
  readonly instanceId: string;
  readonly contractId: string;
  readonly contractDigest: string;
  readonly startedAt: string;
  readonly publishIntervalMs: number;

  #checks = new Map<string, ServiceHealthCheckFn>();
  #info?: ServiceHealthInfoFn;

  constructor(args: {
    serviceName: string;
    kind?: HealthHeartbeatSample["participant"]["kind"];
    instanceId?: string;
    contractId: string;
    contractDigest: string;
    publishIntervalMs: number;
  }) {
    this.serviceName = args.serviceName;
    this.kind = args.kind ?? "service";
    this.instanceId = args.instanceId ?? ulid();
    this.contractId = args.contractId;
    this.contractDigest = args.contractDigest;
    this.startedAt = new Date().toISOString();
    this.publishIntervalMs = args.publishIntervalMs;
  }

  setInfo(info: ServiceHealthInfo | ServiceHealthInfoFn): void {
    if (typeof info === "function") {
      this.#info = info;
      return;
    }

    this.#info = () => info;
  }

  add(name: string, check: ServiceHealthCheckFn): () => void {
    this.#checks.set(name, check);
    return () => {
      this.#checks.delete(name);
    };
  }

  async checks(): Promise<HealthCheckResult[]> {
    return await Promise.all(
      Array.from(this.#checks.entries()).map(([name, check]) =>
        runServiceHealthCheck(name, check)
      ),
    );
  }

  async response(): Promise<HealthResponse> {
    const checks = Object.fromEntries(this.#checks.entries());
    return await runAllServiceHealthChecks(this.serviceName, checks);
  }

  async sample(): Promise<HealthHeartbeatSample> {
    const checks = await this.checks();
    const info = this.#info ? await this.#info() : undefined;
    return createHealthHeartbeatSample({
      serviceName: this.serviceName,
      kind: this.kind,
      instanceId: this.instanceId,
      contractId: this.contractId,
      contractDigest: this.contractDigest,
      startedAt: this.startedAt,
      publishIntervalMs: this.publishIntervalMs,
      checks,
      info,
    });
  }
}
