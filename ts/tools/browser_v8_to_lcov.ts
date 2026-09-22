import { fromFileUrl, isAbsolute, join, relative, resolve } from "@std/path";
import v8ToIstanbul from "v8-to-istanbul";

const tsRoot = fromFileUrl(new URL("../", import.meta.url));
const repoRoot = resolve(tsRoot, "..");

type V8Range = {
  readonly startOffset: number;
  readonly endOffset: number;
  readonly count: number;
};

type V8FunctionCoverage = {
  readonly functionName: string;
  readonly ranges: V8Range[];
  readonly isBlockCoverage: boolean;
};

type V8ScriptCoverage = {
  readonly url: string;
  readonly functions: V8FunctionCoverage[];
};

type Location = {
  readonly line: number;
  readonly column: number;
};

type RangeLocation = {
  readonly start: Location;
  readonly end: Location;
};

type FunctionLocation = {
  readonly name: string;
  readonly decl: RangeLocation;
  readonly loc: RangeLocation;
};

type BranchLocation = {
  readonly loc: RangeLocation;
  readonly locations: readonly RangeLocation[];
};

export type IstanbulFileCoverage = {
  readonly path: string;
  readonly statementMap: Record<string, RangeLocation>;
  readonly s: Record<string, number>;
  readonly fnMap: Record<string, FunctionLocation>;
  readonly f: Record<string, number>;
  readonly branchMap: Record<string, BranchLocation>;
  readonly b: Record<string, readonly number[]>;
};

type IstanbulCoverage = Record<string, IstanbulFileCoverage>;

type Options = {
  readonly inputDir: string;
  readonly output: string;
  readonly buildDir: string;
  readonly appendTo: string | undefined;
  readonly help: boolean;
};

type SourceMap = {
  readonly version: number;
  readonly file?: string;
  readonly names?: readonly string[];
  readonly sources: readonly string[];
  readonly sourcesContent?: readonly string[];
  readonly mappings: string;
  readonly sourceRoot?: string;
};

export async function convertBrowserV8Coverage(
  options: Options,
): Promise<string> {
  const coverage: IstanbulCoverage = {};
  for (const input of await coverageFiles(options.inputDir)) {
    for (const script of readV8Coverage(await Deno.readTextFile(input))) {
      const scriptPath = await localScriptPath(script.url, options.buildDir);
      if (scriptPath === undefined) continue;

      const converter = v8ToIstanbul(
        scriptPath,
        0,
        await scriptSources(scriptPath),
        (path) => excludedBrowserCoveragePath(path, options.buildDir),
      );
      await converter.load();
      converter.applyCoverage(script.functions);
      const converted: unknown = converter.toIstanbul();
      mergeCoverage(coverage, readIstanbulCoverage(converted));
    }
  }
  return coverageToLcov(coverage);
}

async function scriptSources(
  scriptPath: string,
): Promise<{ source: string; sourceMap?: { sourcemap: SourceMap } }> {
  const source = await Deno.readTextFile(scriptPath);
  const sourceMap = await readSourceMap(`${scriptPath}.map`);
  return sourceMap === undefined
    ? { source }
    : { source, sourceMap: { sourcemap: sourceMap } };
}

async function readSourceMap(path: string): Promise<SourceMap | undefined> {
  try {
    return rewriteSourceMapSources(
      readSourceMapJson(await Deno.readTextFile(path)),
    );
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return undefined;
    throw error;
  }
}

function readSourceMapJson(raw: string): SourceMap {
  const parsed: unknown = JSON.parse(raw);
  if (
    !isRecord(parsed) || typeof parsed.version !== "number" ||
    !Array.isArray(parsed.sources) || typeof parsed.mappings !== "string"
  ) {
    throw new Error("browser source map has an invalid shape");
  }
  if (!parsed.sources.every((source) => typeof source === "string")) {
    throw new Error("browser source map sources must be strings");
  }
  if (
    parsed.sourcesContent !== undefined &&
    (!Array.isArray(parsed.sourcesContent) ||
      !parsed.sourcesContent.every((source) => typeof source === "string"))
  ) {
    throw new Error("browser source map sourcesContent must be strings");
  }
  if (
    parsed.names !== undefined &&
    (!Array.isArray(parsed.names) ||
      !parsed.names.every((name) => typeof name === "string"))
  ) {
    throw new Error("browser source map names must be strings");
  }
  return {
    version: parsed.version,
    file: typeof parsed.file === "string" ? parsed.file : undefined,
    names: parsed.names,
    sources: parsed.sources,
    sourcesContent: parsed.sourcesContent,
    mappings: parsed.mappings,
    sourceRoot: typeof parsed.sourceRoot === "string"
      ? parsed.sourceRoot
      : undefined,
  };
}

function rewriteSourceMapSources(sourceMap: SourceMap): SourceMap {
  return {
    ...sourceMap,
    sources: sourceMap.sources.map(rewriteSourcePath),
    sourceRoot: "",
  };
}

function rewriteSourcePath(source: string): string {
  if (source.startsWith("file:") || isAbsolute(source)) return source;
  const stripped = source.replace(/^(\.\.\/)+/, "");
  if (stripped.startsWith("src/")) {
    return resolve(tsRoot, "portals/login", stripped);
  }
  if (
    stripped.startsWith("packages/") || stripped.startsWith("apps/") ||
    stripped.startsWith("services/")
  ) {
    return resolve(tsRoot, stripped);
  }
  if (stripped.startsWith("generated/") || stripped.startsWith("art/")) {
    return resolve(repoRoot, stripped);
  }
  return source;
}

function excludedBrowserCoveragePath(path: string, buildDir: string): boolean {
  const normalized = path.replaceAll("\\", "/");
  return normalized.includes("/node_modules/") ||
    normalized.includes("/generated/client-optimized/") ||
    normalized.endsWith("/generated/root.js") ||
    normalized.endsWith("/generated/root.svelte") ||
    normalized.endsWith("/__vite-browser-external") ||
    isInside(buildDir, path);
}

export function coverageToLcov(coverage: IstanbulCoverage): string {
  const records = Object.keys(coverage).sort().map((path) => {
    const file = coverage[path];
    const lines = new Map<number, number>();
    for (const [id, statement] of Object.entries(file.statementMap)) {
      lines.set(
        statement.start.line,
        Math.max(lines.get(statement.start.line) ?? 0, file.s[id] ?? 0),
      );
    }

    const functions = Object.keys(file.fnMap).sort(compareNumberStrings);
    const branches = Object.keys(file.branchMap).sort(compareNumberStrings);
    const lineEntries = [...lines.entries()].sort(([left], [right]) =>
      left - right
    );
    const output = ["TN:", `SF:${file.path}`];

    for (const id of functions) {
      const fn = file.fnMap[id];
      output.push(`FN:${fn.decl.start.line},${fn.name}`);
    }
    for (const id of functions) {
      output.push(`FNDA:${file.f[id] ?? 0},${file.fnMap[id].name}`);
    }
    output.push(`FNF:${functions.length}`);
    output.push(
      `FNH:${functions.filter((id) => (file.f[id] ?? 0) > 0).length}`,
    );

    for (const [line, count] of lineEntries) output.push(`DA:${line},${count}`);
    output.push(`LF:${lineEntries.length}`);
    output.push(
      `LH:${lineEntries.filter(([, count]) => count > 0).length}`,
    );

    for (const id of branches) {
      const branch = file.branchMap[id];
      const counts = file.b[id] ?? [];
      for (const [index, count] of counts.entries()) {
        output.push(`BRDA:${branch.loc.start.line},${id},${index},${count}`);
      }
    }
    output.push(`BRF:${branches.length}`);
    output.push(
      `BRH:${
        branches.filter((id) => (file.b[id] ?? []).some((count) => count > 0))
          .length
      }`,
    );
    output.push("end_of_record");
    return output.join("\n");
  });
  return records.length === 0 ? "" : `${records.join("\n")}\n`;
}

async function coverageFiles(inputDir: string): Promise<string[]> {
  const files: string[] = [];
  try {
    for await (const entry of Deno.readDir(inputDir)) {
      if (entry.isFile && entry.name.endsWith(".json")) {
        files.push(join(inputDir, entry.name));
      }
    }
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return [];
    throw error;
  }
  return files.sort();
}

function readV8Coverage(raw: string): readonly V8ScriptCoverage[] {
  const parsed: unknown = JSON.parse(raw);
  if (!isRecord(parsed) || !Array.isArray(parsed.result)) {
    throw new Error("browser V8 coverage must contain a result array");
  }
  return parsed.result.map(readScriptCoverage);
}

function readScriptCoverage(value: unknown): V8ScriptCoverage {
  if (
    !isRecord(value) || typeof value.url !== "string" ||
    !Array.isArray(value.functions)
  ) {
    throw new Error("browser V8 script coverage has an invalid shape");
  }
  return {
    url: value.url,
    functions: value.functions.map(readFunctionCoverage),
  };
}

function readFunctionCoverage(value: unknown): V8FunctionCoverage {
  if (
    !isRecord(value) || typeof value.functionName !== "string" ||
    typeof value.isBlockCoverage !== "boolean" || !Array.isArray(value.ranges)
  ) {
    throw new Error("browser V8 function coverage has an invalid shape");
  }
  return {
    functionName: value.functionName,
    isBlockCoverage: value.isBlockCoverage,
    ranges: value.ranges.map(readRange),
  };
}

function readRange(value: unknown): V8Range {
  if (
    !isRecord(value) || typeof value.startOffset !== "number" ||
    typeof value.endOffset !== "number" || typeof value.count !== "number"
  ) {
    throw new Error("browser V8 coverage range has an invalid shape");
  }
  return {
    startOffset: value.startOffset,
    endOffset: value.endOffset,
    count: value.count,
  };
}

function readIstanbulCoverage(value: unknown): IstanbulCoverage {
  if (!isRecord(value)) throw new Error("Istanbul coverage must be an object");
  const coverage: IstanbulCoverage = {};
  for (const [path, file] of Object.entries(value)) {
    coverage[path] = readIstanbulFileCoverage(file);
  }
  return coverage;
}

function readIstanbulFileCoverage(value: unknown): IstanbulFileCoverage {
  if (!isRecord(value) || typeof value.path !== "string") {
    throw new Error("Istanbul file coverage has an invalid shape");
  }
  return {
    path: value.path,
    statementMap: readRangeLocationRecord(value.statementMap),
    s: readNumberRecord(value.s),
    fnMap: readFunctionLocationRecord(value.fnMap),
    f: readNumberRecord(value.f),
    branchMap: readBranchLocationRecord(value.branchMap),
    b: readBranchCountRecord(value.b),
  };
}

function readRangeLocationRecord(
  value: unknown,
): Record<string, RangeLocation> {
  if (!isRecord(value)) throw new Error("Istanbul range map must be an object");
  const result: Record<string, RangeLocation> = {};
  for (const [key, range] of Object.entries(value)) {
    result[key] = readRangeLocation(range);
  }
  return result;
}

function readFunctionLocationRecord(
  value: unknown,
): Record<string, FunctionLocation> {
  if (!isRecord(value)) {
    throw new Error("Istanbul function map must be an object");
  }
  const result: Record<string, FunctionLocation> = {};
  for (const [key, fn] of Object.entries(value)) {
    if (!isRecord(fn) || typeof fn.name !== "string") {
      throw new Error("Istanbul function map has an invalid shape");
    }
    result[key] = {
      name: fn.name,
      decl: readRangeLocation(fn.decl),
      loc: readRangeLocation(fn.loc),
    };
  }
  return result;
}

function readBranchLocationRecord(
  value: unknown,
): Record<string, BranchLocation> {
  if (!isRecord(value)) {
    throw new Error("Istanbul branch map must be an object");
  }
  const result: Record<string, BranchLocation> = {};
  for (const [key, branch] of Object.entries(value)) {
    if (!isRecord(branch) || !Array.isArray(branch.locations)) {
      throw new Error("Istanbul branch map has an invalid shape");
    }
    result[key] = {
      loc: readRangeLocation(branch.loc),
      locations: branch.locations.map(readRangeLocation),
    };
  }
  return result;
}

function readRangeLocation(value: unknown): RangeLocation {
  if (!isRecord(value)) throw new Error("Istanbul range has an invalid shape");
  return {
    start: readLocation(value.start),
    end: readLocation(value.end),
  };
}

function readLocation(value: unknown): Location {
  if (
    !isRecord(value) || typeof value.line !== "number" ||
    typeof value.column !== "number"
  ) {
    throw new Error("Istanbul location has an invalid shape");
  }
  return { line: value.line, column: value.column };
}

function readNumberRecord(value: unknown): Record<string, number> {
  if (!isRecord(value)) throw new Error("Istanbul count map must be an object");
  const result: Record<string, number> = {};
  for (const [key, count] of Object.entries(value)) {
    if (typeof count !== "number") {
      throw new Error("Istanbul count map has an invalid shape");
    }
    result[key] = count;
  }
  return result;
}

function readBranchCountRecord(
  value: unknown,
): Record<string, readonly number[]> {
  if (!isRecord(value)) {
    throw new Error("Istanbul branch count map must be an object");
  }
  const result: Record<string, readonly number[]> = {};
  for (const [key, counts] of Object.entries(value)) {
    if (
      !Array.isArray(counts) ||
      !counts.every((count) => typeof count === "number")
    ) {
      throw new Error("Istanbul branch count map has an invalid shape");
    }
    result[key] = counts;
  }
  return result;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

async function localScriptPath(
  scriptUrl: string,
  buildDir: string,
): Promise<string | undefined> {
  if (!scriptUrl.startsWith("http://") && !scriptUrl.startsWith("https://")) {
    return undefined;
  }
  const path = decodeURIComponent(new URL(scriptUrl).pathname);
  const candidate = resolve(buildDir, `.${path}`);
  if (!isInside(buildDir, candidate)) return undefined;
  try {
    return (await Deno.stat(candidate)).isFile ? candidate : undefined;
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return undefined;
    throw error;
  }
}

function isInside(root: string, path: string): boolean {
  const rel = relative(resolve(root), resolve(path));
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

function mergeCoverage(
  target: IstanbulCoverage,
  source: IstanbulCoverage,
): void {
  for (const [path, file] of Object.entries(source)) {
    const existing = target[path];
    if (existing === undefined) {
      target[path] = file;
      continue;
    }
    for (const [id, count] of Object.entries(file.s)) {
      existing.s[id] = (existing.s[id] ?? 0) + count;
    }
    for (const [id, count] of Object.entries(file.f)) {
      existing.f[id] = (existing.f[id] ?? 0) + count;
    }
    for (const [id, counts] of Object.entries(file.b)) {
      existing.b[id] = counts.map((count, index) =>
        (existing.b[id]?.[index] ?? 0) + count
      );
    }
  }
}

function compareNumberStrings(left: string, right: string): number {
  return Number(left) - Number(right);
}

function parseArgs(args: readonly string[]): Options {
  let inputDir = resolve(tsRoot, "coverage/browser");
  let output = resolve(tsRoot, "coverage/browser/lcov.info");
  let buildDir = resolve(tsRoot, "portals/login/build");
  let appendTo: string | undefined;
  let help = false;

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--help" || arg === "-h") help = true;
    else if (arg === "--input-dir") {
      inputDir = resolvePath(readFlagValue(args, index, arg));
      index += 1;
    } else if (arg.startsWith("--input-dir=")) {
      inputDir = resolvePath(readInlineFlagValue(arg, "--input-dir"));
    } else if (arg === "--output") {
      output = resolvePath(readFlagValue(args, index, arg));
      index += 1;
    } else if (arg.startsWith("--output=")) {
      output = resolvePath(readInlineFlagValue(arg, "--output"));
    } else if (arg === "--build-dir") {
      buildDir = resolvePath(readFlagValue(args, index, arg));
      index += 1;
    } else if (arg.startsWith("--build-dir=")) {
      buildDir = resolvePath(readInlineFlagValue(arg, "--build-dir"));
    } else if (arg === "--append-to") {
      appendTo = resolvePath(readFlagValue(args, index, arg));
      index += 1;
    } else if (arg.startsWith("--append-to=")) {
      appendTo = resolvePath(readInlineFlagValue(arg, "--append-to"));
    } else throw new Error(`unknown argument: ${arg}`);
  }

  return { inputDir, output, buildDir, appendTo, help };
}

function resolvePath(path: string): string {
  return isAbsolute(path) ? path : resolve(tsRoot, path);
}

function readFlagValue(
  args: readonly string[],
  index: number,
  flag: string,
): string {
  const value = args[index + 1];
  if (value === undefined || value.startsWith("--")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function readInlineFlagValue(arg: string, flag: string): string {
  const value = arg.slice(flag.length + 1);
  if (value === "") throw new Error(`${flag} requires a value`);
  return value;
}

function helpText(): string {
  return `Convert browser CDP V8 coverage to LCOV.

Usage:
  deno run -A tools/browser_v8_to_lcov.ts [options]

Options:
  --input-dir <dir>  Raw browser V8 JSON directory. Defaults to coverage/browser.
  --output <file>    Browser LCOV output. Defaults to coverage/browser/lcov.info.
  --build-dir <dir>  Static browser build directory. Defaults to portals/login/build.
  --append-to <file> Append browser LCOV to an existing LCOV file.
  --help, -h         Print this help text.`;
}

if (import.meta.main) {
  try {
    const options = parseArgs(Deno.args);
    if (options.help) {
      console.log(helpText());
      Deno.exit(0);
    }
    const lcov = await convertBrowserV8Coverage(options);
    await Deno.mkdir(resolve(options.output, ".."), { recursive: true });
    await Deno.writeTextFile(options.output, lcov);
    if (options.appendTo !== undefined) {
      const existing = await Deno.readTextFile(options.appendTo);
      await Deno.writeTextFile(
        options.appendTo,
        `${existing}${
          existing.endsWith("\n") || existing === "" ? "" : "\n"
        }${lcov}`,
      );
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    Deno.exit(1);
  }
}
