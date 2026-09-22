import ts from "typescript";
import {
  resolveInternalNpmDependenciesForBuild,
  resolvePackageBuildVersion,
} from "./release_build_version.ts";

/** Compile an SDK's ordinary TypeScript project and copy its npm metadata. */
export async function buildTypeScriptPackage(
  metadata: {
    name: string;
    description: string;
    exports: Record<string, unknown>;
    dependencies: Record<string, string>;
    peerDependencies?: Record<string, string>;
    peerDependenciesMeta?: Record<string, { optional: boolean }>;
  },
  version: string,
) {
  const config = ts.readConfigFile("tsconfig.npm.json", ts.sys.readFile);
  if (config.error) {
    throw new Error(
      ts.flattenDiagnosticMessageText(config.error.messageText, "\n"),
    );
  }
  const parsed = ts.parseJsonConfigFileContent(
    config.config,
    ts.sys,
    Deno.cwd(),
  );
  const program = ts.createProgram(
    parsed.fileNames,
    parsed.options,
  );
  const diagnostics = [
    ...parsed.errors,
    ...ts.getPreEmitDiagnostics(program),
  ];
  if (diagnostics.length) {
    throw new Error(ts.formatDiagnosticsWithColorAndContext(diagnostics, {
      getCanonicalFileName: (name) => name,
      getCurrentDirectory: () => Deno.cwd(),
      getNewLine: () => "\n",
    }));
  }
  await Deno.remove("npm", { recursive: true }).catch((error) => {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  });
  const emitted = program.emit();
  if (emitted.emitSkipped || emitted.diagnostics.length) {
    throw new Error("TypeScript package emission failed");
  }
  const buildVersion = resolvePackageBuildVersion(version);
  await Deno.writeTextFile(
    "npm/package.json",
    JSON.stringify(
      {
        ...metadata,
        version: buildVersion,
        type: "module",
        license: "Apache-2.0",
        publishConfig: { access: "public" },
        repository: {
          type: "git",
          url: "git+https://github.com/abalmos/trellis.git",
        },
        dependencies: resolveInternalNpmDependenciesForBuild(
          metadata.dependencies,
          buildVersion,
        ),
        peerDependencies: resolveInternalNpmDependenciesForBuild(
          metadata.peerDependencies,
          buildVersion,
        ),
      },
      null,
      2,
    ) + "\n",
  );
  await Deno.copyFile("README.md", "npm/README.md");
}
