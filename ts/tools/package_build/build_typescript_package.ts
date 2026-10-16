import { copyFile, rm, writeFile } from "node:fs/promises";
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
    engines?: Record<string, string>;
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
    process.cwd(),
  );
  const program = ts.createProgram(parsed.fileNames, parsed.options);
  const diagnostics = [...parsed.errors, ...ts.getPreEmitDiagnostics(program)];
  if (diagnostics.length) {
    throw new Error(
      ts.formatDiagnosticsWithColorAndContext(diagnostics, {
        getCanonicalFileName: (name) => name,
        getCurrentDirectory: () => process.cwd(),
        getNewLine: () => "\n",
      }),
    );
  }
  await rm("npm", { recursive: true, force: true });
  const emitted = program.emit();
  if (emitted.emitSkipped || emitted.diagnostics.length) {
    throw new Error("TypeScript package emission failed");
  }
  const buildVersion = resolvePackageBuildVersion(version);
  await writeFile(
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
          url: "git+https://github.com/OATS-Center/trellis.git",
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
  await copyFile("README.md", "npm/README.md");
}
