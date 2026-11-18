import { execFile } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { ulid } from "ulid";

const [staged, ...dependencies] = process.argv.slice(2);
if (!staged)
  throw new Error(
    "Usage: verify_testkit_package.ts <staged package> [local SDK packages]",
  );
const directory = await mkdtemp(join(tmpdir(), "trellis-package-consumer-"));
const run = promisify(execFile);
try {
  await writeFile(
    join(directory, "package.json"),
    JSON.stringify({ private: true, type: "module" }),
  );
  await run(
    "npm",
    [
      "install",
      "--ignore-scripts",
      "--install-links",
      "--no-audit",
      "--no-fund",
      ...dependencies.map((path) => resolve(path)),
      resolve(staged),
    ],
    { cwd: directory },
  );
  await writeFile(
    join(directory, "smoke.mjs"),
    `
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
// Every entrypoint of every installed package loads on this Node.
const installed = JSON.parse(await readFile("package.json", "utf8")).dependencies;
for (const name of Object.keys(installed)) {
  const manifest = JSON.parse(await readFile(\`node_modules/\${name}/package.json\`, "utf8"));
  for (const subpath of Object.keys(manifest.exports)) {
    await import(subpath === "." ? name : name + subpath.slice(1));
  }
}
const { TrellisTestRuntime } = await import("@oatscenter/trellis-testkit");
const server = process.env.TRELLIS_TEST_SERVER_BIN;
await using runtime = await TrellisTestRuntime.start(server ? { trellis: { source: { kind: "path", server } } } : {});
const username = ${JSON.stringify(`package-consumer-${ulid().toLowerCase()}`)};
const created = await runtime.callAdminRpc("authUsersCreate", { username, displayName: username, idempotencyKey: ${JSON.stringify(ulid())} });
const reset = await runtime.callAdminRpc("authUsersPasswordResetCreate", { userId: created.user.principalId, idempotencyKey: ${JSON.stringify(ulid())} });
assert.equal(created.user.account?.username, username);
assert.ok(reset.token);
console.log(\`Loaded every entrypoint of \${Object.keys(installed).join(", ")} on Node \${process.version}; the installed testkit acquired its native host and persisted a user through generated RPCs\`);
`,
  );
  const result = await run(process.execPath, ["smoke.mjs"], {
    cwd: directory,
    maxBuffer: 4 * 1024 * 1024,
  });
  process.stdout.write(result.stdout);
  process.stderr.write(result.stderr);
} finally {
  await rm(directory, { recursive: true, force: true });
}
