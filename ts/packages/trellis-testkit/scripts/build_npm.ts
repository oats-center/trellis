import { cp } from "node:fs/promises";
import { buildTypeScriptPackage } from "../../../tools/package_build/build_typescript_package.ts";
import config from "../package.json" with { type: "json" };

await buildTypeScriptPackage(
  {
    name: config.name,
    description:
      "Node integration test host using ordinary Trellis runtime binaries and public SDK clients.",
    exports: { ".": { types: "./index.d.ts", import: "./index.js" } },
    dependencies: config.dependencies,
    engines: config.engines,
  },
  config.version,
);

await cp("trellis", "npm/trellis", { recursive: true });
