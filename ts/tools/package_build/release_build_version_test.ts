import { deepStrictEqual, throws } from "node:assert/strict";
import { test } from "node:test";
import {
  resolveInternalNpmDependenciesForBuild,
  resolvePackageBuildVersion,
} from "./release_build_version.ts";

test("resolvePackageBuildVersion uses prepared release version", () => {
  withReleaseEnv("0.8.2-rc.1", "0.8.2", () => {
    deepStrictEqual(resolvePackageBuildVersion("0.8.2"), "0.8.2-rc.1");
  });
});

test("resolveInternalNpmDependenciesForBuild uses exact internal prerelease package specs", () => {
  withReleaseEnv("0.8.2-rc.1", "0.8.2", () => {
    deepStrictEqual(
      resolveInternalNpmDependenciesForBuild({
        "@oatscenter/result": "^0.8.2",
        "@oatscenter/trellis": "~0.8.2",
        typebox: "^1.0.15",
      }),
      {
        "@oatscenter/result": "0.8.2-rc.1",
        "@oatscenter/trellis": "0.8.2-rc.1",
        typebox: "^1.0.15",
      },
    );
  });
});

test("resolveInternalNpmDependenciesForBuild preserves internal stable package spec ranges", () => {
  withReleaseEnv("0.8.2", "0.8.2", () => {
    deepStrictEqual(
      resolveInternalNpmDependenciesForBuild({
        "@oatscenter/result": "^0.8.2",
        "@oatscenter/trellis": "~0.8.2",
        typebox: "^1.0.15",
      }),
      {
        "@oatscenter/result": "^0.8.2",
        "@oatscenter/trellis": "~0.8.2",
        typebox: "^1.0.15",
      },
    );
  });
});

test("resolveInternalNpmDependenciesForBuild uses exact prepared prerelease package specs", () => {
  withoutReleaseEnv(() => {
    deepStrictEqual(
      resolveInternalNpmDependenciesForBuild(
        {
          "@oatscenter/result": "^0.8.2",
          typebox: "^1.0.15",
        },
        "0.8.2-rc.1",
      ),
      {
        "@oatscenter/result": "0.8.2-rc.1",
        typebox: "^1.0.15",
      },
    );
  });
});

test("resolveInternalNpmDependenciesForBuild rejects mismatched base versions", () => {
  withReleaseEnv("0.9.0-rc.1", "0.9.0", () => {
    throws(
      () =>
        resolveInternalNpmDependenciesForBuild({
          "@oatscenter/trellis": "^0.8.2",
        }),
      {
        name: "Error",
        message: /@oatscenter\/trellis dependency uses 0\.8\.2/,
      },
    );
  });
});

function withReleaseEnv(
  version: string,
  baseVersion: string,
  run: () => void,
): void {
  const previousVersion = process.env["TRELLIS_RELEASE_VERSION"];
  const previousBase = process.env["TRELLIS_RELEASE_BASE_VERSION"];
  try {
    process.env["TRELLIS_RELEASE_VERSION"] = version;
    process.env["TRELLIS_RELEASE_BASE_VERSION"] = baseVersion;
    run();
  } finally {
    restoreEnv("TRELLIS_RELEASE_VERSION", previousVersion);
    restoreEnv("TRELLIS_RELEASE_BASE_VERSION", previousBase);
  }
}

function withoutReleaseEnv(run: () => void): void {
  const previousVersion = process.env["TRELLIS_RELEASE_VERSION"];
  const previousBase = process.env["TRELLIS_RELEASE_BASE_VERSION"];
  try {
    delete process.env["TRELLIS_RELEASE_VERSION"];
    delete process.env["TRELLIS_RELEASE_BASE_VERSION"];
    run();
  } finally {
    restoreEnv("TRELLIS_RELEASE_VERSION", previousVersion);
    restoreEnv("TRELLIS_RELEASE_BASE_VERSION", previousBase);
  }
}

function restoreEnv(name: string, value: string | undefined): void {
  if (value === undefined) {
    delete process.env[name];
  } else {
    process.env[name] = value;
  }
}
