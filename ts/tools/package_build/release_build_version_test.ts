import { assertEquals, assertThrows } from "@std/assert";
import {
  resolveInternalNpmDependenciesForBuild,
  resolvePackageBuildVersion,
} from "./release_build_version.ts";

Deno.test("resolvePackageBuildVersion uses checked-in version without release env", () => {
  const previousVersion = Deno.env.get("TRELLIS_RELEASE_VERSION");
  const previousBase = Deno.env.get("TRELLIS_RELEASE_BASE_VERSION");
  try {
    Deno.env.delete("TRELLIS_RELEASE_VERSION");
    Deno.env.delete("TRELLIS_RELEASE_BASE_VERSION");
    assertEquals(resolvePackageBuildVersion("0.8.2"), "0.8.2");
  } finally {
    restoreEnv("TRELLIS_RELEASE_VERSION", previousVersion);
    restoreEnv("TRELLIS_RELEASE_BASE_VERSION", previousBase);
  }
});

Deno.test("resolvePackageBuildVersion uses prepared release version", () => {
  withReleaseEnv("0.8.2-rc.1", "0.8.2", () => {
    assertEquals(resolvePackageBuildVersion("0.8.2"), "0.8.2-rc.1");
  });
});

Deno.test("resolveInternalNpmDependenciesForBuild uses exact internal prerelease package specs", () => {
  withReleaseEnv("0.8.2-rc.1", "0.8.2", () => {
    assertEquals(
      resolveInternalNpmDependenciesForBuild({
        "@qlever-llc/result": "^0.8.2",
        "@qlever-llc/trellis": "~0.8.2",
        typebox: "^1.0.15",
      }),
      {
        "@qlever-llc/result": "0.8.2-rc.1",
        "@qlever-llc/trellis": "0.8.2-rc.1",
        typebox: "^1.0.15",
      },
    );
  });
});

Deno.test("resolveInternalNpmDependenciesForBuild preserves internal stable package spec ranges", () => {
  withReleaseEnv("0.8.2", "0.8.2", () => {
    assertEquals(
      resolveInternalNpmDependenciesForBuild({
        "@qlever-llc/result": "^0.8.2",
        "@qlever-llc/trellis": "~0.8.2",
        typebox: "^1.0.15",
      }),
      {
        "@qlever-llc/result": "^0.8.2",
        "@qlever-llc/trellis": "~0.8.2",
        typebox: "^1.0.15",
      },
    );
  });
});

Deno.test("resolveInternalNpmDependenciesForBuild uses exact prepared prerelease package specs", () => {
  withoutReleaseEnv(() => {
    assertEquals(
      resolveInternalNpmDependenciesForBuild({
        "@qlever-llc/result": "^0.8.2",
        typebox: "^1.0.15",
      }, "0.8.2-rc.1"),
      {
        "@qlever-llc/result": "0.8.2-rc.1",
        typebox: "^1.0.15",
      },
    );
  });
});

Deno.test("resolveInternalNpmDependenciesForBuild rejects mismatched base versions", () => {
  withReleaseEnv("0.9.0-rc.1", "0.9.0", () => {
    assertThrows(
      () =>
        resolveInternalNpmDependenciesForBuild({
          "@qlever-llc/trellis": "^0.8.2",
        }),
      Error,
      "@qlever-llc/trellis dependency uses 0.8.2",
    );
  });
});

function withReleaseEnv(
  version: string,
  baseVersion: string,
  run: () => void,
): void {
  const previousVersion = Deno.env.get("TRELLIS_RELEASE_VERSION");
  const previousBase = Deno.env.get("TRELLIS_RELEASE_BASE_VERSION");
  try {
    Deno.env.set("TRELLIS_RELEASE_VERSION", version);
    Deno.env.set("TRELLIS_RELEASE_BASE_VERSION", baseVersion);
    run();
  } finally {
    restoreEnv("TRELLIS_RELEASE_VERSION", previousVersion);
    restoreEnv("TRELLIS_RELEASE_BASE_VERSION", previousBase);
  }
}

function withoutReleaseEnv(run: () => void): void {
  const previousVersion = Deno.env.get("TRELLIS_RELEASE_VERSION");
  const previousBase = Deno.env.get("TRELLIS_RELEASE_BASE_VERSION");
  try {
    Deno.env.delete("TRELLIS_RELEASE_VERSION");
    Deno.env.delete("TRELLIS_RELEASE_BASE_VERSION");
    run();
  } finally {
    restoreEnv("TRELLIS_RELEASE_VERSION", previousVersion);
    restoreEnv("TRELLIS_RELEASE_BASE_VERSION", previousBase);
  }
}

function restoreEnv(name: string, value: string | undefined): void {
  if (value === undefined) {
    Deno.env.delete(name);
  } else {
    Deno.env.set(name, value);
  }
}
