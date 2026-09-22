export function formatDate(value: string | null | undefined): string {
  if (!value) return "-";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "Unexpected error";
}

export function shortKey(value: string | null | undefined): string {
  if (!value) return "-";
  if (value.length <= 18) return value;
  return `${value.slice(0, 8)}...${value.slice(-8)}`;
}

export function labelForKind(value: string): string {
  switch (value) {
    case "auth.connect":
      return "Connect";
    case "auth.disconnect":
      return "Disconnect";
    case "auth.session_revoked":
      return "Session revoked";
    case "auth.connection_kicked":
      return "Connection kicked";
    default:
      return value;
  }
}

export function toneForKind(value: string): string {
  switch (value) {
    case "auth.connect":
      return "badge-success";
    case "auth.disconnect":
      return "badge-info";
    case "auth.session_revoked":
      return "badge-warning";
    case "auth.connection_kicked":
      return "badge-error";
    default:
      return "badge-outline";
  }
}
