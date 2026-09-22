export type CatalogIssueLike = {
  issueId: string;
  kind: string;
  contractId?: string;
  message: string;
  deploymentIds?: string[];
};

export type ContractDependencyBlockDetail = {
  alias?: string;
  surfaceKind?: string;
  surfaceName?: string;
  providerContractId?: string;
};

function surfaceKindLabel(kind: string): string {
  switch (kind.toLowerCase()) {
    case "rpc":
      return "RPC";
    case "event":
      return "Event";
    case "operation":
      return "Operation";
    case "feed":
    case "feeds":
      return "Feed";
    default:
      return kind;
  }
}

/** Returns true when a catalog issue blocks a contract on a missing or inactive dependency. */
export function isContractDependencyBlock(issue: CatalogIssueLike): boolean {
  return issue.kind === "invalid-active-contract-uses";
}

/** Returns true when a catalog issue requires an authority migration review. */
export function isAuthorityMigrationIssue(issue: CatalogIssueLike): boolean {
  return issue.kind === "incompatible-active-contract";
}

/** Parses dependency block messages into operator-facing surface details. */
export function parseContractDependencyBlock(
  message: string,
): ContractDependencyBlockDetail {
  const missingSurface = message.match(
    /Dependency '([^']+)' references missing (rpc|event|operation|feed|feeds) '([^']+)' on '([^']+)'/i,
  );
  if (missingSurface) {
    return {
      alias: missingSurface[1],
      surfaceKind: surfaceKindLabel(missingSurface[2]),
      surfaceName: missingSurface[3],
      providerContractId: missingSurface[4],
    };
  }

  const inactiveContract = message.match(
    /Dependency(?: '[^']+')? references (?:inactive|unknown) contract '([^']+)'/i,
  );
  if (inactiveContract) return { providerContractId: inactiveContract[1] };

  return {};
}

/** Formats the required dependency surface that is blocking a contract. */
export function contractDependencyRequiredThing(
  issue: CatalogIssueLike,
): string {
  const detail = parseContractDependencyBlock(issue.message);
  return detail.surfaceKind && detail.surfaceName
    ? `${detail.surfaceKind} ${detail.surfaceName}`
    : "a required surface";
}

/** Returns the provider contract named by a dependency block issue. */
export function contractDependencyProviderContract(
  issue: CatalogIssueLike,
): string {
  return parseContractDependencyBlock(issue.message).providerContractId ??
    "a provider contract";
}

/** Formats a compact label for a dependency block issue. */
export function contractDependencyBlockLabel(issue: CatalogIssueLike): string {
  const detail = parseContractDependencyBlock(issue.message);
  if (detail.surfaceKind && detail.surfaceName) {
    return `${detail.surfaceKind} ${detail.surfaceName}`;
  }
  if (detail.providerContractId) return detail.providerContractId;
  return issue.contractId ?? issue.issueId;
}
