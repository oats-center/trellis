import { type apis } from "trellis-web-generated";

export type ParticipantKind = "app" | "agent" | "device" | "service";

type UserPrincipal = {
  type: "user";
  userId: string;
  name: string;
  identity: {
    identityId: string;
    provider: string;
    subject: string;
  };
};

export type SessionRecord = apis.auth.SessionsListOutput["items"][number];

export type ConnectionRecord = apis.auth.ConnectionsListOutput["items"][number];

export type UserGrantRecord = {
  identityGrantId: string;
  contractEvidence: {
    contractDigest: string;
    contractId: string;
  };
  displayName: string;
  description: string;
  participantKind: "app" | "agent";
  capabilities: string[];
  grantedAt: string;
  updatedAt: string;
};

type SessionLike = SessionRecord | ConnectionRecord;

export function formatIdentityProviderSubject(
  identity: UserPrincipal["identity"],
): string {
  return `${identity.provider}:${identity.subject}`;
}

export function formatIdentityProviderLabel(provider: string): string {
  const normalized = provider.trim().toLowerCase();
  switch (normalized) {
    case "local":
      return "Password";
    case "github":
      return "GitHub";
    case "google":
      return "Google";
    case "microsoft":
    case "azuread":
    case "azure-ad":
      return "Microsoft";
    case "oidc":
      return "OIDC";
    case "saml":
      return "SAML";
    default:
      return provider.trim() || "External provider";
  }
}

export function formatShortKey(
  value: string | null | undefined,
  size = 12,
): string {
  if (!value) return "—";
  return value.length <= size ? value : `${value.slice(0, size)}…`;
}

export function participantKindLabel(kind: string): string {
  switch (kind) {
    case "app":
      return "App";
    case "agent":
      return "Agent";
    case "device":
      return "Device";
    case "service":
      return "Service";
  }

  return kind || "Unknown";
}

export function participantKindBadgeClass(kind: string): string {
  switch (kind) {
    case "app":
      return "badge-primary";
    case "agent":
      return "badge-secondary";
    case "device":
      return "badge-accent";
    case "service":
      return "badge-outline";
  }

  return "badge-neutral";
}

function contractLabel(record: SessionLike): string | null {
  const displayName = "contractDisplayName" in record &&
      typeof record.contractDisplayName === "string"
    ? record.contractDisplayName
    : undefined;
  const contractId =
    "contractId" in record && typeof record.contractId === "string"
      ? record.contractId
      : undefined;

  if (displayName && contractId) {
    return `${displayName} (${contractId})`;
  }
  return displayName ?? contractId ?? null;
}

function joinDetails(details: Array<string | null | undefined>): string {
  return details.filter((detail): detail is string =>
    Boolean(detail && detail.length > 0)
  ).join(" • ");
}

export function describeSessionPrincipal(
  record: SessionLike,
): { title: string; details: string } {
  const contract = contractLabel(record);
  // The principal ID is the current authoritative description. `userNkey` is a
  // legacy transport key and is never promoted to the primary label.
  return {
    title: record.principalId,
    details: joinDetails([
      "connectionId" in record && typeof record.connectionId === "string"
        ? record.connectionId
        : null,
      "sessionId" in record && typeof record.sessionId === "string"
        ? record.sessionId
        : null,
      contract,
    ]),
  };
}

export function describeUserGrant(
  grant: UserGrantRecord,
): { title: string; details: string } {
  return {
    title: grant.displayName || grant.contractEvidence.contractId,
    details: joinDetails([
      `${participantKindLabel(grant.participantKind)} grant`,
      grant.contractEvidence.contractId,
    ]),
  };
}
