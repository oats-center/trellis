/**
 * Plain-language labels for Events and Jobs. Wire values (event names, API
 * IDs, states, serialized errors) become short words a person can act on.
 */

type Tone = "healthy" | "degraded" | "unhealthy" | "offline";

/** "Connections.Opened" → "Connections opened"; "StatusChanged" → "Status changed". */
export function sentenceName(value: string): string {
  const words = value
    .split(/[._\-\s]+/)
    .flatMap((part) => part.replace(/([a-z0-9])([A-Z])/g, "$1 $2").split(" "))
    .filter(Boolean)
    .map((word) => word.toLowerCase());
  const deduped = words.filter((word, index) => word !== words[index - 1]);
  const text = deduped.join(" ");
  return text ? text.charAt(0).toUpperCase() + text.slice(1) : value;
}

/** "trellis.health@v1" → "Health"; "acme.billing-api@v2" → "Billing api". */
export function apiLabel(apiId: string | undefined): string {
  if (!apiId) return "Unknown";
  const name = apiId.split("@")[0] ?? apiId;
  return sentenceName(name.split(".").at(-1) ?? name);
}

/** Plain status of a service's event delivery, with the reason it matters. */
export function deliveryStatus(status: string): {
  label: string;
  tone: Tone;
  attention: boolean;
} {
  switch (status) {
    case "current":
      return { label: "Up to date", tone: "healthy", attention: false };
    case "processing":
      return { label: "Working", tone: "healthy", attention: false };
    case "behind":
      return { label: "Behind", tone: "degraded", attention: true };
    case "saturated":
      return { label: "Overloaded", tone: "degraded", attention: true };
    case "failing":
      return { label: "Retrying", tone: "unhealthy", attention: true };
    case "inactive":
      return { label: "Not receiving", tone: "unhealthy", attention: true };
    case "missing":
      return { label: "Not set up", tone: "unhealthy", attention: true };
    case "orphaned":
      return { label: "Left over", tone: "offline", attention: true };
    case "unmanaged":
      return { label: "Outside Trellis", tone: "offline", attention: false };
    default:
      return { label: sentenceName(status), tone: "offline", attention: true };
  }
}

/** Why an event could not be trusted, or "Verified". */
export function eventCheckLabel(
  verification: string,
  resolution: string,
): { label: string; ok: boolean } {
  if (resolution === "malformed") return { label: "Unreadable", ok: false };
  if (resolution === "unresolved") {
    return { label: "Unknown event type", ok: false };
  }
  const labels: Record<string, string> = {
    verified: "Verified",
    "missing-proof": "Not signed",
    "missing-key": "Unknown sender",
    "untrusted-key": "Untrusted sender",
    "invalid-signature": "Bad signature",
    "missing-session": "Sender not signed in",
    "subject-denied": "Not allowed to publish",
    "outside-session-window": "Sent outside its session",
    "auth-unavailable": "Couldn't check",
  };
  return {
    label: labels[verification] ?? sentenceName(verification),
    ok: verification === "verified",
  };
}

/** Plain job state; running jobs can be waiting or slow. */
export function jobStateLabel(job: {
  state: string;
  waitingOn?: readonly unknown[];
  runtimeBand?: string;
}): { label: string; tone: Tone } {
  if (job.state === "active" && (job.waitingOn?.length ?? 0) > 0) {
    return { label: "Waiting", tone: "degraded" };
  }
  if (job.state === "active" && job.runtimeBand === "slow") {
    return { label: "Slow", tone: "degraded" };
  }
  const labels: Record<string, { label: string; tone: Tone }> = {
    active: { label: "Running", tone: "healthy" },
    pending: { label: "Queued", tone: "degraded" },
    retry: { label: "Retrying", tone: "degraded" },
    completed: { label: "Finished", tone: "healthy" },
    failed: { label: "Failed", tone: "unhealthy" },
    dead: { label: "Failed for good", tone: "unhealthy" },
    stale: { label: "Stuck", tone: "unhealthy" },
    cancelled: { label: "Cancelled", tone: "offline" },
    dismissed: { label: "Dismissed", tone: "offline" },
    expired: { label: "Expired", tone: "unhealthy" },
    skipped: { label: "Skipped", tone: "offline" },
  };
  return (
    labels[job.state] ?? { label: sentenceName(job.state), tone: "offline" }
  );
}

/**
 * The human part of a stored error. Job and delivery errors are often a
 * serialized Trellis error whose useful text is the root cause.
 */
export function errorText(raw: string | undefined): string | undefined {
  const text = rawErrorText(raw);
  return text === undefined ? undefined : (KNOWN_MESSAGES[text] ?? text);
}

function rawErrorText(raw: string | undefined): string | undefined {
  if (!raw) return undefined;
  const trimmed = raw.trim();
  if (!trimmed.startsWith("{")) return trimmed;
  try {
    const parsed = JSON.parse(trimmed) as Record<string, unknown>;
    const context =
      parsed.context && typeof parsed.context === "object"
        ? (parsed.context as Record<string, unknown>)
        : {};
    for (const candidate of [
      context.causeMessage,
      context.message,
      parsed.message,
    ]) {
      if (typeof candidate === "string" && candidate.length > 0)
        return candidate;
    }
    if (typeof parsed.type === "string") return sentenceName(parsed.type);
  } catch {
    // Not JSON after all; show it as written.
  }
  return trimmed;
}

/** A duration as people say it: "350ms", "2.5s", "33s", "4m 12s", "3h 5m", "2d 4h". */
export function friendlyDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "—";
  if (ms < 1_000) return `${Math.round(ms)}ms`;
  if (ms < 10_000) return `${(ms / 1_000).toFixed(1).replace(/\.0$/, "")}s`;
  const seconds = Math.round(ms / 1_000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60)
    return seconds % 60 ? `${minutes}m ${seconds % 60}s` : `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48)
    return minutes % 60 ? `${hours}h ${minutes % 60}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  return hours % 24 ? `${days}d ${hours % 24}h` : `${days}d`;
}

/** System messages that mean something simpler to the person reading them. */
const KNOWN_MESSAGES: Record<string, string> = {
  "broker max deliveries exceeded": "Gave up after the most tries allowed",
  "Retry job": "Asked to try again",
};
