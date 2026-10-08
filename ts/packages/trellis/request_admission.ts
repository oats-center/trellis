import { z } from "zod";
import {
  recordCatalogCounter,
  recordCatalogUpDown,
} from "./telemetry/metrics.ts";

/** Bound for verification that may only authorize a busy refusal. @internal */
export const REFUSAL_REQUEST_LIMIT = 4;

/** Limits shared by all provider endpoints and transport generations in a service. */
export const RequestLimitsSchema = z.object({
  /** Concurrent ordinary dispatches, including authorization, through reply handoff. */
  requests: z.number().int().positive().max(1_000_000).default(32),
  /** Retained inbound message bytes; excludes decoded heap and transport buffers. */
  bytes: z.number().int().positive().max(0xffff_ffff).default(16 * 1024 * 1024),
  /** Reserved concurrency for built-in Operation get/cancel and Live controls. */
  controls: z.number().int().positive().max(1_000_000).default(8),
  /** Reserved retained bytes for built-in Operation get/cancel and Live controls. */
  controlBytes: z.number().int().positive().max(0xffff_ffff).default(
    1024 * 1024,
  ),
});

/** Configurable local message-dispatch limits; omitted fields use framework defaults. */
export type RequestLimits = z.input<typeof RequestLimitsSchema>;

/** @internal One non-waiting intake owner; verification never invokes user code. */
export class RequestAdmission {
  readonly #pools: Record<"request" | "control" | "verification", {
    requests: number;
    bytes: number;
    maxRequests: number;
    maxBytes: number;
  }>;

  /** Create the logical service's shared capacity owner. */
  constructor(limits?: RequestLimits) {
    const value = RequestLimitsSchema.parse(limits ?? {});
    this.#pools = {
      request: {
        requests: 0,
        bytes: 0,
        maxRequests: value.requests,
        maxBytes: value.bytes,
      },
      control: {
        requests: 0,
        bytes: 0,
        maxRequests: value.controls,
        maxBytes: value.controlBytes,
      },
      verification: {
        requests: 0,
        bytes: 0,
        maxRequests: REFUSAL_REQUEST_LIMIT,
        maxBytes: 1024 * 1024,
      },
    };
  }

  /** Reserve immediately, or authenticate a refusal within separate bounded capacity. */
  admit(
    bytes: number,
    control: boolean,
  ): { release: () => void; refused: boolean } | undefined {
    const kind = control ? "control" : "request";
    let selected: "request" | "control" | "verification" = kind;
    let pool = this.#pools[kind];
    const reason = pool.requests >= pool.maxRequests
      ? "requests"
      : pool.bytes + bytes > pool.maxBytes
      ? "bytes"
      : undefined;
    if (reason) {
      pool = this.#pools.verification;
      selected = "verification";
      const dropped = pool.requests >= pool.maxRequests
        ? "verification_requests"
        : pool.bytes + bytes > pool.maxBytes
        ? "verification_bytes"
        : undefined;
      recordCatalogCounter("trellis.service.admission.rejections", 1, {
        "trellis.kind": kind,
        "trellis.reason": dropped ?? reason,
      });
      if (dropped) return undefined;
    }
    pool.requests++;
    pool.bytes += bytes;
    const attributes = { "trellis.kind": selected };
    recordCatalogUpDown("trellis.service.admission.inflight", 1, attributes);
    recordCatalogUpDown("trellis.service.admission.bytes", bytes, attributes);
    let released = false;
    return {
      refused: reason !== undefined,
      release: () => {
        if (released) return;
        released = true;
        pool.requests--;
        pool.bytes -= bytes;
        recordCatalogUpDown(
          "trellis.service.admission.inflight",
          -1,
          attributes,
        );
        recordCatalogUpDown(
          "trellis.service.admission.bytes",
          -bytes,
          attributes,
        );
      },
    };
  }
}
