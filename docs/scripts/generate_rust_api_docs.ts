// Builds Rustdoc for the public Trellis Rust crates into docs/static/api/rust
// so the API reference site serves emitted Rustdoc instead of pointing at an
// unbuilt external target.
const repoRoot = new URL("../../", import.meta.url);
const output = new URL("docs/static/api/rust/", repoRoot);
const outputParent = new URL("./", output);

await Deno.mkdir(outputParent, { recursive: true });
await Deno.remove(output, { recursive: true }).catch((error) => {
  if (!(error instanceof Deno.errors.NotFound)) {
    throw error;
  }
});

const command = new Deno.Command("cargo", {
  cwd: repoRoot,
  args: [
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
});

const result = await command.spawn().status;
if (!result.success) {
  Deno.exit(result.code);
}

async function copyTree(from: URL, to: URL) {
  await Deno.mkdir(to, { recursive: true });
  for await (const entry of Deno.readDir(from)) {
    const suffix = entry.isDirectory ? "/" : "";
    const source = new URL(entry.name + suffix, from);
    const target = new URL(entry.name + suffix, to);
    if (entry.isDirectory) {
      await copyTree(source, target);
    } else {
      await Deno.copyFile(source, target);
    }
  }
}

await copyTree(new URL("target/doc/", repoRoot), output);

console.log(
  "Generated Rustdoc for trellis-rs, trellis-protocol, and trellis-testkit",
);
