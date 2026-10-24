import { fileURLToPath } from "node:url";
import { Application } from "typedoc";

const application = await Application.bootstrapWithPlugins({
  name: "Trellis TypeScript API",
  entryPointStrategy: "packages",
  entryPoints: [fileURLToPath(new URL("../../ts/packages/*", import.meta.url))],
  packageOptions: {
    compilerOptions: {
      target: "ES2022",
      module: "ESNext",
      moduleResolution: "Bundler",
      types: ["node"],
      lib: ["ESNext", "DOM", "DOM.Iterable", "DOM.AsyncIterable"],
      allowImportingTsExtensions: true,
      noEmit: true,
      strict: true,
      verbatimModuleSyntax: true,
    },
  },
});
const project = await application.convert();
if (!project)
  throw new Error("TypeScript API documentation compilation failed");
await application.generateDocs(
  project,
  fileURLToPath(new URL("../static/api/typescript", import.meta.url)),
);
