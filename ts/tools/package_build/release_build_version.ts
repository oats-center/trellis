const SEMVER_RE =
  /^(?<base>\d+\.\d+\.\d+)(?<suffix>-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

const INTERNAL_NPM_SCOPE = "@qlever-llc/";

type ParsedReleaseVersion = {
  version: string;
  baseVersion: string;
  isPrerelease: boolean;
};

function parseReleaseVersion(version: string): ParsedReleaseVersion {
  const trimmed = version.trim();
  const match = trimmed.match(SEMVER_RE);
  if (!match?.groups) {
    throw new Error(
      `Invalid release version '${version}'. Expected semver like 0.8.0 or 0.8.0-rc.1.`,
    );
  }
  const groups = match.groups as { base: string; suffix?: string };
  return {
    version: trimmed,
    baseVersion: groups.base,
    isPrerelease: groups.suffix !== undefined,
  };
}

function validateVersionBase(
  actualVersion: string,
  expectedBaseVersion: string,
  label: string,
): void {
  const actualBaseVersion = parseReleaseVersion(actualVersion).baseVersion;
  if (actualBaseVersion !== expectedBaseVersion) {
    throw new Error(
      `${label} uses ${actualVersion}, but release tag requires base version ${expectedBaseVersion}.`,
    );
  }
}

function replaceDependencyVersionSpec(
  packageName: string,
  currentSpec: string,
  releaseVersion: ParsedReleaseVersion,
  expectedBaseVersion: string,
): string {
  const match = currentSpec.match(
    /^(?<prefix>[~^<>= ]*)(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)(?<suffix>.*)$/,
  );
  if (!match?.groups) {
    return currentSpec;
  }
  const groups = match.groups as {
    prefix: string;
    version: string;
    suffix: string;
  };
  validateVersionBase(
    groups.version,
    expectedBaseVersion,
    `${packageName} dependency`,
  );
  if (releaseVersion.isPrerelease) {
    return releaseVersion.version;
  }
  return `${groups.prefix}${releaseVersion.version}${groups.suffix}`;
}

export function resolvePackageBuildVersion(checkedInVersion: string): string {
  const releaseVersion = Deno.env.get("TRELLIS_RELEASE_VERSION")?.trim();
  if (!releaseVersion) {
    return checkedInVersion;
  }

  const parsedReleaseVersion = parseReleaseVersion(releaseVersion);
  const expectedBaseVersion =
    Deno.env.get("TRELLIS_RELEASE_BASE_VERSION")?.trim() ||
    parsedReleaseVersion.baseVersion;
  validateVersionBase(checkedInVersion, expectedBaseVersion, "package version");
  return releaseVersion;
}

export function resolveInternalNpmDependenciesForBuild(
  dependencies: Record<string, string> | undefined,
  buildVersion?: string,
): Record<string, string> | undefined {
  const releaseVersion = Deno.env.get("TRELLIS_RELEASE_VERSION")?.trim() ||
    buildVersion?.trim();
  if (!releaseVersion || !dependencies) {
    return dependencies;
  }

  const parsedReleaseVersion = parseReleaseVersion(releaseVersion);
  const expectedBaseVersion =
    Deno.env.get("TRELLIS_RELEASE_BASE_VERSION")?.trim() ||
    parsedReleaseVersion.baseVersion;
  return Object.fromEntries(
    Object.entries(dependencies).map(([packageName, spec]) => {
      if (!packageName.startsWith(INTERNAL_NPM_SCOPE)) {
        return [packageName, spec];
      }
      return [
        packageName,
        replaceDependencyVersionSpec(
          packageName,
          spec,
          parsedReleaseVersion,
          expectedBaseVersion,
        ),
      ];
    }),
  );
}
