import { basename, dirname, join } from "@std/path";
import { compile, compileModule } from "svelte/compiler";
import ts from "typescript";
import {
  resolveInternalNpmDependenciesForBuild,
  resolvePackageBuildVersion,
} from "../../../tools/package_build/release_build_version.ts";

const description =
  "Svelte components and state helpers for Trellis browser applications.";
const repositoryUrl = "git+https://github.com/Qlever-LLC/trellis.git";
const outDir = "./npm";
const jsrDir = "./jsr";

async function emptyDir(path: string): Promise<void> {
  await Deno.remove(path, { recursive: true }).catch((error) => {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  });
  await Deno.mkdir(path, { recursive: true });
}

const sourceFiles = [
  "src/index.ts",
  "src/context.svelte.ts",
  "src/authorization_context.svelte.ts",
  "src/portal_flow.svelte.ts",
  "src/device_activation.svelte.ts",
  "src/device_activation_controller.ts",
  "src/internal/activation_view.ts",
  "src/internal/callback_state.ts",
  "src/internal/portal_url.ts",
  "src/components/TrellisProvider.svelte",
  "src/components/TrellisContextProvider.svelte",
  "src/components/TrellisProvider.types.ts",
];
const declarationSourceFiles = sourceFiles.filter((sourceFile) =>
  sourceFile.endsWith(".ts")
);

const denoConfig = JSON.parse(await Deno.readTextFile("./deno.json"));
const name = denoConfig.name as string;
const version = resolvePackageBuildVersion(denoConfig.version as string);
const jsrRuntimeDependencyVersion =
  Deno.env.get("TRELLIS_SVELTE_JSR_RUNTIME_DEPENDENCY_VERSION")?.trim() ||
  jsrRuntimeDependencyFloorVersion(version);
const dependencies = resolveInternalNpmDependenciesForBuild(
  {
    "@nats-io/nats-core": "^3.3.1",
    "@qlever-llc/result": "^0.12.0",
    "@qlever-llc/trellis": "^0.12.0",
    typebox: "^1.0.15",
    ulid: "^3.0.2",
  },
  version,
);

function jsrRuntimeDependencyFloorVersion(version: string): string {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:[-+].*)?$/.exec(version);
  if (!match) return version;
  const [, major, minor, patch] = match;
  const patchNumber = Number(patch);
  if (patchNumber <= 1) return version;
  // Use a caret range low enough to tolerate one skipped runtime JSR patch.
  return `${major}.${minor}.${patchNumber - 2}`;
}

const peerDependencies = resolveInternalNpmDependenciesForBuild(
  {
    svelte: "^5.0.0",
  },
  version,
);

function rewriteRuntimeImports(code: string): string {
  return code
    .replace(
      /(from\s+["'])(\.{1,2}\/[^"']+?)\.svelte\.ts(["'])/g,
      "$1$2.js$3",
    )
    .replace(
      /(from\s+["'])(\.{1,2}\/[^"']+?)\.ts(["'])/g,
      "$1$2.js$3",
    );
}

function rewriteSvelteComponentImports(code: string): string {
  return code.replace(
    /(from\s+["'])(\.{1,2}\/[^"']+?)\.svelte(["'])/g,
    "$1$2.js$3",
  );
}

function rewriteJsrRuntimeImports(code: string): string {
  return rewriteSvelteComponentImports(rewriteRuntimeImports(code));
}

function rewriteDeclarationImports(
  code: string,
  componentImports: "svelte" | "js",
): string {
  const rewritten = rewriteRuntimeImports(code);
  return componentImports === "js"
    ? rewriteSvelteComponentImports(rewritten)
    : rewritten;
}

function transpileTypeScript(code: string): string {
  return ts.transpileModule(code, {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ES2022,
      verbatimModuleSyntax: true,
    },
  }).outputText;
}

function runtimePathFor(
  sourceFile: string,
  rootDir: string,
  componentOutput: "svelte" | "js",
): string {
  if (sourceFile.endsWith(".svelte.ts")) {
    return join(
      rootDir,
      sourceFile.replace(/^src\//, "dist/").replace(/\.svelte\.ts$/, ".js"),
    );
  }
  if (sourceFile.endsWith(".ts")) {
    return join(
      rootDir,
      sourceFile.replace(/^src\//, "dist/").replace(/\.ts$/, ".js"),
    );
  }
  return join(
    rootDir,
    sourceFile.replace(/^src\//, "dist/").replace(
      /\.svelte$/,
      componentOutput === "js" ? ".js" : ".svelte",
    ),
  );
}

function declarationPathFor(
  sourceFile: string,
  rootDir: string,
  componentOutput: "svelte" | "js",
): string {
  if (sourceFile.endsWith(".svelte.ts")) {
    return join(
      rootDir,
      sourceFile.replace(/^src\//, "dist/").replace(
        /\.svelte\.ts$/,
        ".d.ts",
      ),
    );
  }
  if (sourceFile.endsWith(".ts")) {
    return join(
      rootDir,
      sourceFile.replace(/^src\//, "dist/").replace(/\.ts$/, ".d.ts"),
    );
  }
  return join(
    rootDir,
    sourceFile.replace(/^src\//, "dist/").replace(
      /\.svelte$/,
      componentOutput === "js" ? ".d.ts" : ".svelte.d.ts",
    ),
  );
}

async function writeFile(path: string, contents: string): Promise<void> {
  await Deno.mkdir(dirname(path), { recursive: true });
  await Deno.writeTextFile(path, contents);
}

async function copyFile(source: string, destination: string): Promise<void> {
  await Deno.mkdir(dirname(destination), { recursive: true });
  await Deno.copyFile(source, destination);
}

async function buildRuntimeFile(sourceFile: string): Promise<void> {
  await buildPackageRuntimeFile(sourceFile, outDir, "svelte", false);
}

function withSelfTypesDirective(path: string, code: string): string {
  const dtsFile = basename(path).replace(/\.js$/, ".d.ts");
  return `// @ts-self-types="./${dtsFile}"\n${code}`;
}

async function buildPackageRuntimeFile(
  sourceFile: string,
  rootDir: string,
  componentOutput: "svelte" | "js",
  selfTypes: boolean,
): Promise<void> {
  const source = await Deno.readTextFile(sourceFile);
  const rewritten = componentOutput === "js"
    ? rewriteJsrRuntimeImports(source)
    : rewriteRuntimeImports(source);
  const destination = runtimePathFor(sourceFile, rootDir, componentOutput);

  if (sourceFile.endsWith(".svelte.ts")) {
    const transpiled = transpileTypeScript(rewritten);
    const compiled = compileModule(transpiled, {
      filename: destination,
      generate: "client",
      dev: false,
    });
    const code = compiled.js.code + "\n";
    await writeFile(
      destination,
      selfTypes ? withSelfTypesDirective(destination, code) : code,
    );
    return;
  }

  if (sourceFile.endsWith(".ts")) {
    const code = transpileTypeScript(rewritten);
    await writeFile(
      destination,
      selfTypes ? withSelfTypesDirective(destination, code) : code,
    );
    return;
  }

  if (componentOutput === "js") {
    const compiled = compile(rewritten, {
      filename: sourceFile,
      generate: "client",
      dev: false,
    });
    const code = compiled.js.code + "\n";
    await writeFile(destination, withSelfTypesDirective(destination, code));
    return;
  }

  await writeFile(destination, rewritten);
}

function sourcePathForTypeScriptFile(fileName: string): string | undefined {
  const normalized = fileName.replaceAll("\\", "/");
  const srcIndex = normalized.lastIndexOf("/src/");
  const sourceFile = srcIndex >= 0
    ? normalized.slice(srcIndex + 1)
    : normalized;
  return declarationSourceFiles.includes(sourceFile) ? sourceFile : undefined;
}

function formatDiagnostics(diagnostics: readonly ts.Diagnostic[]): string {
  return ts.formatDiagnosticsWithColorAndContext(diagnostics, {
    getCanonicalFileName: (fileName) => fileName,
    getCurrentDirectory: () => Deno.cwd(),
    getNewLine: () => "\n",
  });
}

async function buildDeclarations(
  rootDir: string,
  componentImports: "svelte" | "js",
): Promise<void> {
  const compilerOptions: ts.CompilerOptions = {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ES2022,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    declaration: true,
    emitDeclarationOnly: true,
    allowImportingTsExtensions: true,
    strict: true,
    skipLibCheck: true,
    lib: [
      "lib.esnext.d.ts",
      "lib.dom.d.ts",
      "lib.dom.iterable.d.ts",
      "lib.dom.asynciterable.d.ts",
    ],
  };
  const program = ts.createProgram(declarationSourceFiles, compilerOptions);
  const emitResult = program.emit(
    undefined,
    (_fileName, data, _writeByteOrderMark, _onError, sourceFiles) => {
      const sourceFile = sourceFiles
        ?.map((source) => sourcePathForTypeScriptFile(source.fileName))
        .find((source): source is string => source !== undefined);
      if (!sourceFile) return;

      const destination = declarationPathFor(
        sourceFile,
        rootDir,
        componentImports,
      );
      const rewritten = rewriteDeclarationImports(data, componentImports);
      Deno.mkdirSync(dirname(destination), { recursive: true });
      Deno.writeTextFileSync(destination, rewritten);
    },
    undefined,
    true,
  );

  if (emitResult.emitSkipped) {
    throw new Error(
      `Failed to emit declarations:\n${
        formatDiagnostics(emitResult.diagnostics)
      }`,
    );
  }

  await writeComponentDeclarations(rootDir, componentImports);
}

async function writeComponentDeclarations(
  rootDir: string,
  componentOutput: "svelte" | "js",
): Promise<void> {
  await writeFile(
    declarationPathFor(
      "src/components/TrellisProvider.svelte",
      rootDir,
      componentOutput,
    ),
    `import type { Component } from "svelte";\nimport type { TrellisProviderProps } from "./TrellisProvider.types.js";\n\n/** Svelte component that connects a Trellis app and provides context to children. */\ndeclare const TrellisProvider: Component<TrellisProviderProps>;\nexport default TrellisProvider;\n`,
  );
  await writeFile(
    declarationPathFor(
      "src/components/TrellisContextProvider.svelte",
      rootDir,
      componentOutput,
    ),
    `import type { Component, Snippet } from "svelte";\nimport type { TrellisAppOwner, TrellisContextClient } from "../context.js";\n\ntype TrellisContextProviderProps = {\n  trellisApp: TrellisAppOwner;\n  trellis: TrellisContextClient;\n  children: Snippet;\n};\n\ndeclare const TrellisContextProvider: Component<TrellisContextProviderProps>;\nexport default TrellisContextProvider;\n`,
  );
}

async function writeJson(path: string, value: unknown): Promise<void> {
  await writeFile(path, JSON.stringify(value, null, 2) + "\n");
}

await emptyDir(outDir);
await emptyDir(jsrDir);

for (const sourceFile of sourceFiles) {
  await copyFile(sourceFile, join(outDir, sourceFile));
  await buildRuntimeFile(sourceFile);
  await buildPackageRuntimeFile(sourceFile, jsrDir, "js", true);
}

await buildDeclarations(outDir, "svelte");
await buildDeclarations(jsrDir, "js");

try {
  await copyFile("README.md", join(outDir, "README.md"));
  await copyFile("README.md", join(jsrDir, "README.md"));
} catch (error) {
  if (!(error instanceof Deno.errors.NotFound)) {
    throw error;
  }
}

await writeJson(
  join(outDir, "package.json"),
  {
    name,
    version,
    type: "module",
    description,
    license: "Apache-2.0",
    homepage: "https://github.com/Qlever-LLC/trellis#readme",
    bugs: {
      url: "https://github.com/Qlever-LLC/trellis/issues",
    },
    repository: {
      type: "git",
      url: repositoryUrl,
    },
    publishConfig: {
      access: "public",
    },
    files: ["dist", "src", "README.md"],
    types: "./dist/index.d.ts",
    exports: {
      ".": {
        types: "./dist/index.d.ts",
        svelte: "./dist/index.js",
        default: "./dist/index.js",
      },
    },
    dependencies,
    peerDependencies,
    svelte: "./dist/index.js",
  },
);

await writeJson(
  join(jsrDir, "deno.json"),
  {
    name,
    version,
    license: "Apache-2.0",
    workspace: [],
    exports: {
      ".": "./dist/index.js",
    },
    publish: {
      exclude: ["!dist/**", "!README.md", "!deno.json"],
    },
    imports: {
      "@qlever-llc/result":
        `jsr:@qlever-llc/result@^${jsrRuntimeDependencyVersion}`,
      "@qlever-llc/trellis":
        `jsr:@qlever-llc/trellis@^${jsrRuntimeDependencyVersion}`,
      "@qlever-llc/trellis/auth":
        `jsr:@qlever-llc/trellis@^${jsrRuntimeDependencyVersion}/auth`,
      "@qlever-llc/trellis/auth/browser":
        `jsr:@qlever-llc/trellis@^${jsrRuntimeDependencyVersion}/auth/browser`,
      "svelte": "npm:svelte@^5.0.0",
      "svelte/internal/client": "npm:svelte@^5.0.0/internal/client",
      "svelte/internal/disclose-version":
        "npm:svelte@^5.0.0/internal/disclose-version",
      "ulid": "npm:ulid@^3.0.2",
    },
  },
);

console.log(`Built ${name}@${version} in npm and jsr`);
