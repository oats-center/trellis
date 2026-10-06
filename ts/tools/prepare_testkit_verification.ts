import { copy } from "@std/fs";
import { basename, dirname, join } from "@std/path";
import { z } from "zod";

const [staged] = Deno.args;
if (!staged) {
  throw new Error(
    "Usage: prepare_testkit_verification.ts <staged testkit directory>",
  );
}
const parent = dirname(staged);
const workspaceConfigUrl = new URL("../../deno.json", import.meta.url);
const workspace = z.object({
  importMap: z.string(),
  compilerOptions: z.record(z.string(), z.unknown()),
}).parse(JSON.parse(await Deno.readTextFile(workspaceConfigUrl)));
const importMap = z.object({
  imports: z.record(z.string(), z.string()),
}).parse(
  JSON.parse(
    await Deno.readTextFile(new URL(workspace.importMap, workspaceConfigUrl)),
  ),
);
const root = { compilerOptions: workspace.compilerOptions, ...importMap };
const tables: Record<
  string,
  { version: string; files: Record<string, string> }
> = {};
for (const name of ["trellis", "result", "trellis-testkit"]) {
  const config = z.object({
    version: z.string(),
    exports: z.union([z.string(), z.record(z.string(), z.string())]),
  }).parse(
    JSON.parse(await Deno.readTextFile(`ts/packages/${name}/deno.json`)),
  );
  const exports = typeof config.exports === "string"
    ? { ".": config.exports }
    : config.exports;
  tables[name] = {
    version: config.version,
    files: Object.fromEntries(
      Object.entries(exports).map((
        [key, file],
      ) => [file.replace(/^\.\//, ""), key]),
    ),
  };
}
for (const [name, path] of Object.entries(root.imports)) {
  if (!path.startsWith("./packages/")) continue;
  const [packageName, ...parts] = path.slice("./packages/".length).split("/");
  const table = tables[packageName];
  const exported = table?.files[parts.join("/")];
  if (exported === undefined) {
    delete root.imports[name];
    continue;
  }
  root.imports[name] = `jsr:@oatscenter/${packageName}@${table.version}${
    exported === "." ? "" : exported.slice(1)
  }`;
}
for (const name of ["trellis", "result"]) {
  await copy(`ts/packages/${name}`, join(parent, "verification", name));
}
await Deno.writeTextFile(
  join(parent, "deno.json"),
  JSON.stringify(
    {
      ...root,
      workspace: [
        basename(staged),
        "verification/trellis",
        "verification/result",
      ],
    },
    null,
    2,
  ) + "\n",
);
