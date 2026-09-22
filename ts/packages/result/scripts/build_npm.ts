import { buildTypeScriptPackage } from "../../../tools/package_build/build_typescript_package.ts";
import config from "../deno.json" with { type: "json" };

await buildTypeScriptPackage({
  name: config.name,
  description:
    "Class-based Result and AsyncResult types for Trellis TypeScript applications.",
  exports: { ".": { types: "./mod.d.ts", import: "./mod.js" } },
  dependencies: { typebox: "^1.1.33", ulid: "^3.0.2" },
}, config.version);
