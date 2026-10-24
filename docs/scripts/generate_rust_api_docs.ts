// Builds Rustdoc for the public Trellis Rust crates into docs/static/api/rust
// so the API reference site serves emitted Rustdoc instead of pointing at an
// unbuilt external target.
import { spawnSync } from "node:child_process";
import { cp, mkdir, rm } from "node:fs/promises";
const repoRoot = new URL("../../", import.meta.url);
const output = new URL("docs/static/api/rust/", repoRoot);
const outputParent = new URL("./", output);

const result = spawnSync(
  "cargo",
  [
    "doc",
    "--no-deps",
    "--manifest-path",
    "Cargo.toml",
    "-p",
    "trellis-rs",
    "-p",
    "trellis-protocol",
    "-p",
    "trellis-testkit",
  ],
  {
    cwd: repoRoot,
    stdio: "inherit",
  },
);

if (result.error) throw result.error;
if (result.status !== 0)
  throw new Error(`Rustdoc build failed (${result.status})`);

await mkdir(outputParent, { recursive: true });
await rm(output, { recursive: true, force: true });
await cp(new URL("target/doc/", repoRoot), output, { recursive: true });

console.log(
  "Generated Rustdoc for trellis-rs, trellis-protocol, and trellis-testkit",
);
