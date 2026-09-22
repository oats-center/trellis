const dateTimeFormatter = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

const relativeTimeFormatter = new Intl.RelativeTimeFormat(undefined, {
  numeric: "auto",
});

const byteFormatter = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 1,
});

/** Formats an ISO date/time for operator-facing UI. */
export function formatDateTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;

  return dateTimeFormatter.format(date);
}

/** Formats an ISO date/time with a short relative age when possible. */
export function formatDateTimeWithAge(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;

  const relative = formatRelativeAge(value);
  return relative
    ? `${formatDateTime(value)} (${relative})`
    : formatDateTime(value);
}

/** Formats an ISO date/time as a short relative age. */
export function formatRelativeAge(value: string): string | null {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return null;

  const diffMs = date.getTime() - Date.now();
  const absMs = Math.abs(diffMs);
  const minuteMs = 60_000;
  const hourMs = 60 * minuteMs;
  const dayMs = 24 * hourMs;

  if (absMs < hourMs) {
    return relativeTimeFormatter.format(
      Math.round(diffMs / minuteMs),
      "minute",
    );
  }

  if (absMs < dayMs) {
    return relativeTimeFormatter.format(Math.round(diffMs / hourMs), "hour");
  }

  if (absMs < 14 * dayMs) {
    return relativeTimeFormatter.format(Math.round(diffMs / dayMs), "day");
  }

  return null;
}

/** Formats bytes using human-readable binary units. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;

  const units = ["KB", "MB", "GB", "TB"] as const;
  let value = bytes / 1024;
  let unitIndex = 0;

  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }

  return `${byteFormatter.format(value)} ${units[unitIndex]}`;
}

/** Formats a bounded transfer ratio as an integer percentage. */
export function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return "0%";
  return `${Math.round(Math.max(0, Math.min(100, value)))}%`;
}
