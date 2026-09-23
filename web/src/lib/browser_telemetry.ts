import {
  initBrowserTelemetry,
  recordCatalogCounter,
  recordCatalogDuration,
} from "@oatscenter/trellis/telemetry/browser";

/** Built-in web app identity for the browser navigation metrics. */
export type BrowserApp = "console" | "portal";

type BrowserTelemetryConfig = {
  enabled: boolean;
  path?: string;
  traceRatio?: number;
};

/**
 * One navigation's telemetry token.
 *
 * Timestamps are captured when each milestone actually occurs; a superseded
 * navigation cannot complete a later one.
 */
export type NavigationToken = {
  readonly id: number;
  readonly app: BrowserApp;
  readonly startedAt: number;
  shellAt?: number;
  completed?: "ok" | "error";
};

let installed = false;
let nextTokenId = 0;
let current: NavigationToken | undefined;
let providerEnabled = false;
let initializationDone = false;
/** Measurements captured before initialization resolved. */
const pendingMeasurements: Array<() => void> = [];
let errorListenersInstalled = false;
/** One in-flight visibility flush; repeated cycles never overlap. */
let flushInFlight: Promise<void> | undefined;

/** Reads the approved opt-in browser configuration; disabled by default. */
function browserConfig(): BrowserTelemetryConfig {
  const runtimeConfig = (globalThis as typeof globalThis & {
    __TRELLIS_RUNTIME_CONFIG__?: {
      browserTelemetry?: BrowserTelemetryConfig;
    };
  }).__TRELLIS_RUNTIME_CONFIG__;
  const viteEnv = (import.meta as ImportMeta & {
    env?: Record<string, string | undefined>;
  }).env ?? {};
  const enabled = runtimeConfig?.browserTelemetry?.enabled ??
    viteEnv["VITE_TRELLIS_BROWSER_TELEMETRY"] === "true";
  const path = runtimeConfig?.browserTelemetry?.path ??
    viteEnv["VITE_TRELLIS_BROWSER_OTLP_PATH"];
  const ratio = runtimeConfig?.browserTelemetry?.traceRatio ??
    (viteEnv["VITE_TRELLIS_BROWSER_TRACE_RATIO"]
      ? Number(viteEnv["VITE_TRELLIS_BROWSER_TRACE_RATIO"])
      : undefined);
  return { enabled, path, traceRatio: ratio };
}

/**
 * Installs one built-in app's browser telemetry exactly once per document.
 *
 * Disabled mode records and exports nothing. When enabled, provider
 * initialization runs off the rendering path; measurements captured before it
 * resolves are flushed with their captured durations.
 */
export function installBrowserTelemetry(app: BrowserApp): void {
  if (installed) return;
  installed = true;
  // performance.now() is relative to the document's navigation start.
  beginNavigation(app, 0);
  const config = browserConfig();
  if (!config.enabled || config.path === undefined) {
    initializationDone = true;
    return;
  }
  void (async () => {
    try {
      const handle = await initBrowserTelemetry({
        app,
        endpoint: config.path!,
        serviceName: `trellis-browser-${app}`,
        traceRatio: config.traceRatio,
      });
      providerEnabled = handle.enabled;
      if (providerEnabled) {
        installErrorListeners(app);
        installVisibilityFlush(handle);
        for (const flush of pendingMeasurements.splice(0)) flush();
      }
    } catch {
      // Provider failure never breaks rendering and never records.
    } finally {
      initializationDone = true;
    }
  })();
}

/**
 * Starts one navigation's token at its real start boundary.
 *
 * The initial document navigation and later client-side navigations both call
 * this; the router's before/start hook is the boundary, not its after hook.
 */
export function beginNavigation(
  app: BrowserApp,
  startedAt = performance.now(),
): NavigationToken {
  const token: NavigationToken = {
    id: nextTokenId++,
    app,
    startedAt,
  };
  current = token;
  return token;
}

/** Returns the token for the navigation currently in progress. */
export function currentNavigationToken(): NavigationToken | undefined {
  return current;
}

/** Records the shell milestone when a navigation settles. */
export function settleNavigation(token: NavigationToken): void {
  if (token !== current || token.shellAt !== undefined) return;
  token.shellAt = performance.now();
  recordMilestone(token, "shell", "ok");
}

/**
 * Records the ready milestone once the app reached its usable surface.
 *
 * The exact captured token is required so stale async work cannot complete a
 * later navigation.
 */
export function completeNavigation(
  token: NavigationToken | undefined,
  outcome: "ok" | "error" = "ok",
): void {
  if (
    token === undefined || token !== current || token.completed !== undefined
  ) {
    return;
  }
  token.completed = outcome;
  recordMilestone(token, "ready", outcome);
}

/** Records one navigation milestone from its captured timestamp. */
function recordMilestone(
  token: NavigationToken,
  phase: "shell" | "ready",
  outcome: "ok" | "error",
): void {
  const observedAt = phase === "shell"
    ? token.shellAt ?? performance.now()
    : performance.now();
  const elapsedMs = observedAt - token.startedAt;
  const emit = (): void => {
    if (token !== current || !providerEnabled) return;
    recordCatalogDuration(
      "trellis.browser.navigation.duration",
      Math.max(0, elapsedMs),
      {
        "trellis.app": token.app,
        "trellis.phase": phase,
        "trellis.outcome": outcome,
      },
    );
  };
  if (providerEnabled) {
    emit();
  } else if (!initializationDone) {
    // Captured before initialization resolved; flush with the captured
    // duration once the enabled handle exists.
    pendingMeasurements.push(emit);
  }
}

/** Installs bounded unhandled-error counters once per document. */
function installErrorListeners(app: BrowserApp): void {
  if (errorListenersInstalled) return;
  errorListenersInstalled = true;
  window.addEventListener("error", () => {
    recordCatalogCounter("trellis.browser.errors", 1, {
      "trellis.app": app,
      "trellis.kind": "unhandled",
    });
  });
  window.addEventListener("unhandledrejection", () => {
    recordCatalogCounter("trellis.browser.errors", 1, {
      "trellis.app": app,
      "trellis.kind": "unhandled",
    });
  });
}

/** Best-effort bounded flush at most once per visibility transition. */
function installVisibilityFlush(handle: { forceFlush(): Promise<void> }): void {
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "hidden" || flushInFlight !== undefined) {
      return;
    }
    // One in-flight flush; repeated cycles never accumulate overlapping work.
    flushInFlight = handle.forceFlush()
      .catch(() => {})
      .finally(() => {
        flushInFlight = undefined;
      });
  });
}

/** Whether built-in browser telemetry is enabled by deployment configuration. */
export function browserTelemetryEnabled(): boolean {
  const config = browserConfig();
  return config.enabled && config.path !== undefined;
}
