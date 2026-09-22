/**
 * Coalesced refresh scheduling for live console views.
 *
 * Continuous feed events must not starve refresh: the timer is non-resetting,
 * only one refresh runs per scope at a time, and events arriving during a
 * refresh schedule exactly one trailing refresh. A refresh that cannot run
 * keeps its dirty flag, so a hidden page or disconnected transport resumes
 * with one refresh on recovery even when no further event arrives. The
 * scheduler owns no transport; callers wire its `notify()` to a real watch and
 * its `onRefresh` to the page's own read.
 */

/** Retry backoff after a watch closes unexpectedly, in milliseconds. */
export const LIVE_RETRY_BACKOFF_MS = [
  1_000,
  2_000,
  5_000,
  10_000,
  30_000,
] as const;

/** Coalescing window between a change notification and the refresh read. */
export const LIVE_REFRESH_DELAY_MS = 250;

/** Capped backoff delay for a retry attempt (1-based). */
export function retryBackoffMs(attempt: number): number {
  const index = Math.min(Math.max(attempt, 1), LIVE_RETRY_BACKOFF_MS.length) -
    1;
  return LIVE_RETRY_BACKOFF_MS[index];
}

export type RefreshSchedulerOptions = {
  /** One scope read. The scheduler never overlaps calls to this. */
  onRefresh: () => Promise<void>;
  /** Notified when a refresh throws; the read handles its own page state. */
  onRefreshError?: (error: unknown) => void;
  /** Whether refresh may start; hidden or disconnected scopes suspend. */
  canRefresh?: () => boolean;
  delayMs?: number;
};

/**
 * One-slot refresh scheduler. `notify()` marks dirty and starts the coalescing
 * window only when neither a timer nor a read is already pending, so a stream
 * of events still refreshes on the original deadline.
 */
export class RefreshScheduler {
  #options: RefreshSchedulerOptions;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #running = false;
  #dirty = false;
  #disposed = false;

  constructor(options: RefreshSchedulerOptions) {
    this.#options = options;
  }

  get disposed(): boolean {
    return this.#disposed;
  }

  get running(): boolean {
    return this.#running;
  }

  /** True while a notification is waiting for a refresh that may not start. */
  get dirty(): boolean {
    return this.#dirty;
  }

  /** Marks the scope dirty and schedules one coalesced refresh. */
  notify(): void {
    if (this.#disposed) return;
    this.#dirty = true;
    if (this.#timer !== undefined || this.#running) return;
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      void this.#run();
    }, this.#options.delayMs ?? LIVE_REFRESH_DELAY_MS);
  }

  /**
   * Runs one refresh immediately, bypassing the coalescing window.
   *
   * An immediate request during a running read is not dropped: it retains the
   * trailing dirty flag so the current read is followed by exactly one refresh.
   * A request that satisfies an already scheduled notification cancels that
   * pending timer rather than issuing the same refresh twice. A scope that may
   * not run now keeps its dirty flag so recovery performs the read.
   */
  async refreshNow(): Promise<void> {
    if (this.#disposed) return;
    if (this.#running) {
      this.#dirty = true;
      return;
    }
    if (this.#options.canRefresh?.() === false) {
      this.#dirty = true;
      return;
    }
    if (this.#timer !== undefined) {
      clearTimeout(this.#timer);
      this.#timer = undefined;
    }
    this.#dirty = false;
    this.#running = true;
    try {
      await this.#options.onRefresh();
    } catch (error) {
      this.#options.onRefreshError?.(error);
    } finally {
      this.#running = false;
      if (this.#dirty && !this.#disposed) {
        await this.refreshNow();
      }
    }
  }

  /**
   * Resumes a suspended scope exactly once.
   *
   * Call from a visibility or connection recovery event. A retained dirty flag
   * refreshes even when no further feed event arrived; a clean scope does
   * nothing, so repeated recovery signals cannot cause duplicate reads.
   */
  resume(): void {
    if (this.#disposed || !this.#dirty) return;
    if (this.#options.canRefresh?.() === false) return;
    void this.refreshNow();
  }

  async #run(): Promise<void> {
    if (this.#disposed) return;
    if (this.#options.canRefresh?.() === false) {
      // Keep the dirty flag: a suspended scope refreshes on recovery rather
      // than silently discarding the notification.
      return;
    }
    await this.refreshNow();
  }

  /** Cancels the timer and prevents any later refresh. */
  dispose(): void {
    this.#disposed = true;
    this.#dirty = false;
    if (this.#timer !== undefined) {
      clearTimeout(this.#timer);
      this.#timer = undefined;
    }
  }
}

/** State of a live subscription independent of the general connection badge. */
export type LiveStatus =
  | "idle"
  | "connecting"
  | "live"
  | "reconnecting"
  | "closed";

export type LiveSubscriptionOptions = {
  /** Opens the real watch; resolves once the subscription is usable. */
  subscribe: () => Promise<void>;
  /** Releases the real watch. Must be safe to call once per open. */
  unsubscribe: () => Promise<void> | void;
  /** Called on closure/error with the attempt number and its backoff delay. */
  onStatus?: (
    status: LiveStatus,
    detail?: { attempt: number; retryInMs: number },
  ) => void;
};

/**
 * Owns the lifecycle of one live subscription: connect, retry with capped
 * backoff while mounted, and dispose without post-disposal work.
 *
 * Ownership is explicit rather than inferred from mutable page state: each
 * open is a generation, and only its own generation may publish a status, mark
 * itself live, or schedule the single retry timer.
 */
export class LiveSubscription {
  #options: LiveSubscriptionOptions;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #attempt = 0;
  #status: LiveStatus = "idle";
  #generation = 0;
  #opening = false;
  #disposed = false;

  constructor(options: LiveSubscriptionOptions) {
    this.#options = options;
  }

  get status(): LiveStatus {
    return this.#disposed ? "closed" : this.#status;
  }

  #publish(status: LiveStatus, detail?: {
    attempt: number;
    retryInMs: number;
  }): void {
    this.#status = status;
    this.#options.onStatus?.(status, detail);
  }

  /**
   * Opens the watch and, on unexpected completion, retries with backoff.
   *
   * Single-flight: a repeated call while an open or a retry is pending is
   * ignored, so no caller can create a second concurrent watch.
   */
  async start(): Promise<void> {
    if (this.#disposed || this.#opening || this.#status === "live") return;
    if (this.#timer !== undefined) return;
    const generation = ++this.#generation;
    this.#opening = true;
    this.#publish("connecting");
    try {
      await this.#options.subscribe();
    } catch (error) {
      this.#opening = false;
      if (this.#disposed || generation !== this.#generation) return;
      this.#scheduleRetry(error);
      return;
    }
    this.#opening = false;
    // A late open completion must not restore live after disposal or a newer
    // generation, and must not clobber an already-scheduled retry.
    if (this.#disposed || generation !== this.#generation) return;
    if (this.#timer !== undefined) return;
    this.#attempt = 0;
    this.#publish("live");
  }

  /** Records an unexpected watch closure and schedules a retry. */
  closed(error?: unknown): void {
    if (this.#disposed) return;
    ++this.#generation;
    this.#opening = false;
    this.#scheduleRetry(error);
  }

  #scheduleRetry(_detail?: unknown): void {
    if (this.#disposed || this.#timer !== undefined) return;
    const retryInMs = retryBackoffMs(this.#attempt + 1);
    this.#attempt += 1;
    this.#publish("reconnecting", {
      attempt: this.#attempt,
      retryInMs,
    });
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      void this.start();
    }, retryInMs);
  }

  /** Cancels retries and releases the watch; no work runs afterwards. */
  async dispose(): Promise<void> {
    if (this.#disposed) return;
    this.#disposed = true;
    ++this.#generation;
    if (this.#timer !== undefined) {
      clearTimeout(this.#timer);
      this.#timer = undefined;
    }
    await this.#options.unsubscribe();
    this.#status = "closed";
    this.#options.onStatus?.("closed");
  }
}

/**
 * Runs `tick` on a fixed interval while the scope is allowed to refresh. The
 * timer does not stack: one tick at a time, suspended while hidden or
 * disconnected, resumed immediately on the next tick after it becomes allowed.
 */
export class IntervalTimer {
  #tick: () => void | Promise<void>;
  #intervalMs: number;
  #canRun: () => boolean;
  #timer: ReturnType<typeof setInterval> | undefined;
  #running = false;
  #disposed = false;

  constructor(options: {
    tick: () => void | Promise<void>;
    intervalMs: number;
    canRun?: () => boolean;
  }) {
    this.#tick = options.tick;
    this.#intervalMs = options.intervalMs;
    this.#canRun = options.canRun ?? (() => true);
  }

  start(): void {
    if (this.#disposed || this.#timer !== undefined) return;
    this.#timer = setInterval(() => {
      if (this.#disposed || this.#running || !this.#canRun()) return;
      this.#running = true;
      void Promise.resolve(this.#tick()).finally(() => {
        this.#running = false;
      });
    }, this.#intervalMs);
  }

  /** Runs the tick immediately without disturbing the interval cadence. */
  async refreshNow(): Promise<void> {
    if (this.#disposed || this.#running || !this.#canRun()) return;
    this.#running = true;
    try {
      await this.#tick();
    } finally {
      this.#running = false;
    }
  }

  dispose(): void {
    this.#disposed = true;
    if (this.#timer !== undefined) {
      clearInterval(this.#timer);
      this.#timer = undefined;
    }
  }
}
