/**
 * One truthful telemetry owner per live observation endpoint.
 *
 * The endpoint record owns exactly one of these; it derives both the seven
 * `trellis.live.*` families and the narrow Live-only projections from the
 * same local state. Instruments are never recorded from a detached closer, a
 * UI status, or handle garbage collection.
 *
 * Boundary rules: a session moves one unit between phases and is removed at
 * actual local cleanup completion; an end is recorded exactly once at the
 * local terminal commit; a pending normal END is not yet a consumer terminal;
 * retained cleanup stays `closing` and additionally counts once in
 * `trellis.live.cleanup.pending`.
 */

import {
  recordCatalogCounter,
  recordCatalogDuration,
  recordCatalogUpDown,
} from "../telemetry/metrics.ts";
import type { LiveEnd } from "./types.ts";

/** Live observation session kind as it appears in metric dimensions. */
export type LiveTelemetryKind = "standalone" | "operation";

/** Endpoint side dimension for every live family. */
export type LiveTelemetrySide = "consumer" | "provider";

/** Fixed rejection categories for `trellis.live.rejections`. */
export type LiveTelemetryRejection =
  | "invalid_signature"
  | "foreign_identity"
  | "invalid_protocol"
  | "over_capacity";

/** Frame class dimension for `trellis.live.frames`. */
export type LiveTelemetryFrameClass = "data" | "control";

/** Frame direction dimension for `trellis.live.frames`. */
export type LiveTelemetryDirection = "send" | "receive";

type LiveTelemetryPhase =
  | "prepared"
  | "activating"
  | "active"
  | "draining"
  | "closing";

/** Local state owner for one endpoint's live telemetry. */
export class LiveTelemetryOwner {
  readonly #kind: "standalone" | "operation";
  readonly #side: LiveTelemetrySide;
  #phase: LiveTelemetryPhase | undefined;
  #handshakeStarted: number | undefined;
  #handshakeRecorded = false;
  #ended = false;
  #removed = false;
  #cleanupPending = false;

  constructor(kind: LiveTelemetryKind, side: LiveTelemetrySide) {
    this.#kind = kind === "standalone" ? "standalone" : "operation";
    this.#side = side;
  }

  #base(): Record<string, string> {
    return { "trellis.kind": this.#kind, "trellis.side": this.#side };
  }

  #transition(next: LiveTelemetryPhase): void {
    if (this.#phase === next) return;
    if (this.#phase !== undefined) {
      recordCatalogUpDown("trellis.live.sessions", -1, {
        ...this.#base(),
        "trellis.phase": this.#phase,
      });
    }
    this.#phase = next;
    recordCatalogUpDown("trellis.live.sessions", 1, {
      ...this.#base(),
      "trellis.phase": next,
    });
  }

  #recordHandshake(): void {
    if (this.#handshakeRecorded || this.#handshakeStarted === undefined) return;
    this.#handshakeRecorded = true;
    recordCatalogDuration(
      "trellis.live.handshake.duration",
      performance.now() - this.#handshakeStarted,
      this.#base(),
    );
  }

  /** The record now owns a prepared session. */
  prepared(): void {
    this.#transition("prepared");
  }

  /** The first activation control was initiated. */
  activating(): void {
    if (this.#handshakeStarted === undefined) {
      this.#handshakeStarted = performance.now();
    }
    this.#transition("activating");
  }

  /** The local side committed ACTIVE. */
  active(): void {
    this.#recordHandshake();
    this.#transition("active");
  }

  /** A verified normal end was admitted and the queue is draining. */
  draining(): void {
    this.#transition("draining");
  }

  /** The endpoint entered its close exchange. */
  closing(): void {
    this.#transition("closing");
  }

  /**
   * Commit the one local terminal outcome.
   *
   * A prepared failure/cancel/expiry records `trellis.live.ends` without a
   * Live active/end pair.
   */
  end(end: LiveEnd): void {
    if (this.#ended) return;
    this.#ended = true;
    this.#recordHandshake();
    if (this.#phase !== "closing") this.#transition("closing");
    recordCatalogCounter("trellis.live.ends", 1, {
      ...this.#base(),
      "trellis.reason": end.reason,
    });
  }

  /** Retained cleanup exceeded the shared grace. */
  cleanupExceededGrace(): void {
    if (this.#cleanupPending || this.#removed) return;
    this.#cleanupPending = true;
    recordCatalogUpDown("trellis.live.cleanup.pending", 1, this.#base());
  }

  /** Actual owned cleanup settled. */
  cleanupFinished(): void {
    if (this.#cleanupPending) {
      this.#cleanupPending = false;
      recordCatalogUpDown("trellis.live.cleanup.pending", -1, this.#base());
    }
    if (this.#removed) return;
    this.#removed = true;
    if (this.#phase !== undefined) {
      recordCatalogUpDown("trellis.live.sessions", -1, {
        ...this.#base(),
        "trellis.phase": this.#phase,
      });
      this.#phase = undefined;
    }
  }

  /** Count one admitted outgoing or verified incoming live frame. */
  frame(
    class_: LiveTelemetryFrameClass,
    direction: LiveTelemetryDirection,
  ): void {
    recordCatalogCounter("trellis.live.frames", 1, {
      ...this.#base(),
      "trellis.class": class_,
      "trellis.direction": direction,
    });
  }

  /** Count one rejected message by fixed category. */
  rejection(reason: LiveTelemetryRejection): void {
    recordCatalogCounter("trellis.live.rejections", 1, {
      ...this.#base(),
      "trellis.reason": reason,
    });
  }

  /** Transfer retained serialized payload accounting. */
  buffered(delta: number): void {
    if (delta === 0) return;
    recordCatalogUpDown("trellis.live.buffered.bytes", delta, this.#base());
  }
}
