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
import { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
const cli = process.env.TRELLIS_TEST_CLI_BIN;
const server = process.env.TRELLIS_TEST_SERVER_BIN;
assert.equal(Boolean(cli), Boolean(server));
await using runtime = await TrellisTestRuntime.start(cli && server ? { trellis: { source: { kind: "path", cli, server } } } : {});
const username = ${JSON.stringify(`package-consumer-${ulid().toLowerCase()}`)};
const created = await runtime.callAdminRpc("authUsersCreate", { username, displayName: username, idempotencyKey: ${JSON.stringify(ulid())} });
const reset = await runtime.callAdminRpc("authUsersPasswordResetCreate", { userId: created.user.principal.principalId, idempotencyKey: ${JSON.stringify(ulid())} });
assert.equal(created.user.username, username);
assert.ok(reset.token);
console.log("Installed staged npm testkit acquired its native host and persisted a user through generated RPCs");
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
