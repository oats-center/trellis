import {
  displayJson,
  formatTimestamp,
  projectConsoleError,
} from "./console/display_value.ts";

type ErrorLike = {
  name?: unknown;
  message?: unknown;
  reason?: unknown;
  getContext?: () => Record<string, unknown>;
  error?: {
    message?: unknown;
    reason?: unknown;
    remoteError?: {
      message?: unknown;
      reason?: unknown;
      context?: Record<string, unknown>;
      issues?: Array<{ path?: unknown; message?: unknown }>;
    };
    issues?: Array<{ path?: unknown; message?: unknown }>;
    context?: {
      reason?: unknown;
    };
  };
};

function formatAuthReason(reason: unknown): string | null {
  if (typeof reason !== "string") return null;
  switch (reason) {
    case "invalid_request":
      return "The request could not be completed. Check the form and try again.";
    case "insufficient_permissions":
      return "This Console session is missing permission for that action. Sign out and connect the Console again to accept the updated access.";
    case "session_not_found":
    case "session_expired":
      return "Your session has expired. Sign in again.";
    case "invalid_signature":
    case "missing_session_key":
    case "missing_proof":
      return "Your session could not be verified. Sign in again.";
    case "user_not_found":
      return "That user account could not be found.";
    case "username_taken":
      return "That username is already in use.";
    case "user_inactive":
      return "This account is inactive. Contact an administrator.";
    case "forbidden":
      return "You are not allowed to complete this action.";
    case "not_authorized":
      return "You do not have permission for this operation.";
    case "last_admin_required":
      return "At least one active administrator is required.";
    default:
      return null;
  }
}

function formatContextMessage(
  context: Record<string, unknown> | undefined,
): string | null {
  if (!context) return null;

  if (typeof context.message === "string" && context.message.length > 0) {
    return context.message;
  }

  if (
    typeof context.causeMessage === "string" && context.causeMessage.length > 0
  ) {
    return context.causeMessage;
  }

  if (typeof context.reason === "string" && context.reason.length > 0) {
    return formatAuthReason(context.reason) ?? context.reason;
  }

  return null;
}

function formatIssues(
  issues: Array<{ path?: unknown; message?: unknown }>,
): string | null {
  if (issues.length === 0) return null;
  return issues
    .map((issue) => {
      const path = typeof issue.path === "string" ? issue.path : "";
      const message = typeof issue.message === "string"
        ? issue.message
        : "Invalid value";
      return path ? `${path}: ${message}` : message;
    })
    .join("; ");
}

export function formatDate(
  value: string | number | bigint | null | undefined,
): string {
  // Delegates to the shared timestamp rules: zero is valid, decimal strings
  // are Unix milliseconds before ISO parsing, and invalid input is explicit.
  return formatTimestamp(value);
}

export function formatList(values: string[] | null | undefined): string {
  if (!values || values.length === 0) return "-";
  return values.join(", ");
}

/** Converts generated numeric values for bounded visual calculations only. */
export function boundedNumber(value: number | bigint): number {
  if (typeof value === "number") return value;
  const maximum = BigInt(Number.MAX_SAFE_INTEGER);
  if (value > maximum) return Number.MAX_SAFE_INTEGER;
  if (value < -maximum) return -Number.MAX_SAFE_INTEGER;
  return Number(value);
}

export function compactDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "-";
  const rounded = Math.round(ms);
  if (rounded < 1_000) return `${rounded}ms`;

  const milliseconds = rounded % 1_000;
  const totalSeconds = Math.floor(rounded / 1_000);
  const seconds = `${totalSeconds % 60}.${
    String(milliseconds).padStart(3, "0")
  }s`;
  if (totalSeconds < 60) {
    return `${totalSeconds}.${String(milliseconds).padStart(3, "0")}s`;
  }

  const totalMinutes = Math.floor(totalSeconds / 60);
  if (totalMinutes < 60) return `${totalMinutes}m ${seconds}`;

  const totalHours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (totalHours < 48) return `${totalHours}h ${minutes}m ${seconds}`;

  const days = Math.floor(totalHours / 24);
  return `${days}d ${totalHours % 24}h ${minutes}m ${seconds}`;
}

export function jsonBlock(value: unknown): string {
  // Lossless display JSON: exact bigint decimals and tagged bytes. Protocol
  // requests are built by generated codecs, never from this string.
  return displayJson(value);
}

export function jobStateStatus(
  state: string | undefined,
): "healthy" | "degraded" | "unhealthy" | "offline" {
  switch (state) {
    case "completed":
    case "active":
      return "healthy";
    case "failed":
    case "dead":
    case "expired":
    case "stale":
    case "dismissed":
      return "unhealthy";
    case "retry":
    case "pending":
    case "skipped":
      return "degraded";
    default:
      return "offline";
  }
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;

  if (error && typeof error === "object") {
    const candidate = error as ErrorLike;

    const remoteIssues = candidate.error?.remoteError?.issues;
    if (Array.isArray(remoteIssues) && remoteIssues.length > 0) {
      return formatIssues(remoteIssues) ?? "Validation failed";
    }

    const localIssues = candidate.error?.issues;
    if (Array.isArray(localIssues) && localIssues.length > 0) {
      return formatIssues(localIssues) ?? "Validation failed";
    }

    const remoteContextMessage = formatContextMessage(
      candidate.error?.remoteError?.context,
    );
    if (remoteContextMessage) {
      return remoteContextMessage;
    }

    const directContext = typeof candidate.getContext === "function"
      ? candidate.getContext()
      : undefined;
    const directContextMessage = formatContextMessage(directContext);
    if (directContextMessage) {
      return directContextMessage;
    }

    if (typeof candidate.error?.context?.reason === "string") {
      return formatAuthReason(candidate.error.context.reason) ??
        candidate.error.context.reason;
    }

    const nestedContextMessage = formatContextMessage(candidate.error?.context);
    if (nestedContextMessage) {
      return nestedContextMessage;
    }

    // Structured projection covers generated errors and the current `code`
    // field; the legacy `reason` paths above stay for existing callers.
    const projected = projectConsoleError(error);
    const projectedCopy = formatAuthReason(projected.code);
    if (projectedCopy) return projectedCopy;

    const directReasonMessage = formatAuthReason(candidate.reason);
    if (directReasonMessage) {
      return directReasonMessage;
    }

    const nestedReasonMessage = formatAuthReason(candidate.error?.reason);
    if (nestedReasonMessage) {
      return nestedReasonMessage;
    }

    const remoteReasonMessage = formatAuthReason(
      candidate.error?.remoteError?.reason,
    );
    if (remoteReasonMessage) {
      return remoteReasonMessage;
    }

    if (typeof candidate.error?.remoteError?.message === "string") {
      return candidate.error.remoteError.message;
    }

    if (typeof candidate.error?.message === "string") {
      return candidate.error.message;
    }

    if (projected.message !== "Unexpected error") {
      return projected.message;
    }

    if (typeof candidate.message === "string") {
      return candidate.message;
    }
  }

  if (error instanceof Error) return error.message;

  return "Unexpected error";
}
