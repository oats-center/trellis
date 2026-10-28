// Builds docs/static/llms-full.txt from the structured guides, so the complete
// single-file variant never drifts from them. `--check` fails when the
// committed file is stale instead of writing it.
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const staticRoot = fileURLToPath(new URL("../static", import.meta.url));
const parts = [
  "llms-core.txt",
  "llms-typescript.txt",
  "llms-rust.txt",
  "llms-svelte.txt",
];

/** Demotes every Markdown heading outside fenced code by one level. */
function demote(text: string): string {
  let fenced = false;
  return text
    .split("\n")
    .map((line) => {
      if (line.startsWith("```")) fenced = !fenced;
      return !fenced && /^#{1,5} /.test(line) ? `#${line}` : line;
    })
    .join("\n");
}

const body = parts
  .map((part) => demote(readFileSync(join(staticRoot, part), "utf8").trimEnd()))
  .join("\n\n");
const full = `# Trellis: Complete Guide

Every Trellis LLM guide in one file: the language-neutral guide, then the
TypeScript, Rust, and Svelte guides. Generated from ${parts.join(", ")} by
\`node docs/scripts/build_llms_full.ts\`; edit those files instead.

${body}
`;

const target = join(staticRoot, "llms-full.txt");
if (process.argv.includes("--check")) {
  if (readFileSync(target, "utf8") !== full) {
    console.error(
      "docs/static/llms-full.txt is stale; run node docs/scripts/build_llms_full.ts",
    );
    process.exit(1);
  }
} else {
  writeFileSync(target, full);
}
