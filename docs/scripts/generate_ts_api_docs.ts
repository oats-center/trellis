const repoRoot = new URL("../../", import.meta.url);
const tsRoot = new URL("ts/", repoRoot);
const output = new URL("docs/static/api/typescript", repoRoot);
const outputParent = new URL("./", output);
const workspaceConfigUrl = new URL("deno.json", tsRoot);

type PackageExports = string | Record<string, string>;

const npmOnlyPublicEntrypointsByPackage: Record<string, string[]> = {
  "packages/trellis": ["device.ts"],
};

interface TsWorkspaceConfig {
  workspace: string[];
}

interface TsPackageConfig {
  name: string;
  exports: PackageExports;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return isRecord(value) &&
    Object.values(value).every((entry) => typeof entry === "string");
}

function isTsWorkspaceConfig(value: unknown): value is TsWorkspaceConfig {
  return isRecord(value) && Array.isArray(value.workspace) &&
    value.workspace.every((entry) => typeof entry === "string");
}

function isPackageExports(value: unknown): value is PackageExports {
  return typeof value === "string" || isStringRecord(value);
}

function isTsPackageConfig(value: unknown): value is TsPackageConfig {
  if (!isRecord(value) || typeof value.name !== "string") {
    return false;
  }

  return isPackageExports(value.exports);
}

function exportPaths(exports: PackageExports) {
  return typeof exports === "string" ? [exports] : Object.values(exports);
}

async function existingNpmOnlyEntrypoints(
  packageRoot: string,
): Promise<string[]> {
  const entrypoints = npmOnlyPublicEntrypointsByPackage[packageRoot] ?? [];
  const existing = await Promise.all(
    entrypoints.map(async (entrypoint) => {
      try {
        const stat = await Deno.stat(
          new URL(`${packageRoot}/${entrypoint}`, tsRoot),
        );
        return stat.isFile ? `${packageRoot}/${entrypoint}` : null;
      } catch (error) {
        if (error instanceof Deno.errors.NotFound) return null;
        throw error;
      }
    }),
  );

  return existing.filter((entrypoint) => entrypoint !== null);
}

const workspaceConfig: unknown = JSON.parse(
  await Deno.readTextFile(workspaceConfigUrl),
);

if (!isTsWorkspaceConfig(workspaceConfig)) {
  throw new Error("Expected ts/deno.json to contain a string workspace list");
}

const packageWorkspaces = workspaceConfig.workspace.filter((workspace) =>
  workspace.startsWith("./packages/")
);

const packageEntryPoints = await Promise.all(
  packageWorkspaces.map(async (workspace) => {
    const packageConfigUrl = new URL(`${workspace}/deno.json`, tsRoot);
    const packageConfig: unknown = JSON.parse(
      await Deno.readTextFile(packageConfigUrl),
    );

    if (!isTsPackageConfig(packageConfig)) {
      throw new Error(
        `Expected ${workspace}/deno.json to contain a package name and string exports`,
      );
    }

    const packageRoot = workspace.replace(/^\.\//, "");
    const denoEntrypoints = exportPaths(packageConfig.exports).map((
      exportPath,
    ) => `${packageRoot}/${exportPath.replace(/^\.\//, "")}`);
    const npmOnlyEntrypoints = await existingNpmOnlyEntrypoints(packageRoot);
    return [...denoEntrypoints, ...npmOnlyEntrypoints];
  }),
);

const entrypoints = [...new Set(packageEntryPoints.flat())];

await Deno.mkdir(outputParent, { recursive: true });
await Deno.remove(output, { recursive: true }).catch((error) => {
  if (!(error instanceof Deno.errors.NotFound)) {
    throw error;
  }
});

const command = new Deno.Command(Deno.execPath(), {
  cwd: tsRoot,
  args: [
    "doc",
    "--html",
    "--quiet",
    "--name=Trellis TypeScript API",
    "--output=../docs/static/api/typescript",
    ...entrypoints,
  ],
});

const result = await command.spawn().status;
if (!result.success) {
  Deno.exit(result.code);
}

console.log(
  `Generated TypeScript API docs for ${entrypoints.length} package entrypoints from ${packageWorkspaces.length} packages`,
);
