import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  realpath,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const repository = fileURLToPath(new URL("../../../", import.meta.url));
const isolated = await mkdtemp(join(tmpdir(), "trellis-orders-consumer-"));
const project = join(isolated, "orders");
const run = promisify(execFile);
try {
  const files = await run(
    "git",
    ["ls-files", "-z", "--", "docs/examples/orders"],
    { cwd: repository },
  );
  for (const file of files.stdout.split("\0").filter(Boolean)) {
    await cp(
      join(repository, file),
      join(project, file.slice("docs/examples/orders/".length)),
      { recursive: true },
    );
  }
  await cp(
    join(repository, "docs/examples/orders/trellis"),
    join(project, "trellis"),
    { recursive: true },
  );
  const manifest = JSON.parse(
    await readFile(join(project, "package.json"), "utf8"),
  );
  for (const name of ["result", "trellis", "trellis-testkit"]) {
    const destination = join(project, ".sdk", name);
    await cp(join(repository, "ts/packages", name, "npm"), destination, {
      recursive: true,
    });
    manifest.dependencies[`@oatscenter/${name}`] = `file:./.sdk/${name}`;
  }
  await writeFile(join(project, "package.json"), JSON.stringify(manifest));
  const bin = join(isolated, "bin");
  await mkdir(bin);
  await symlink(process.execPath, join(bin, "node"));
  // Package scripts may run nested `npm` commands; hosted runners keep npm
  // beside node rather than in /usr/bin.
  await symlink(
    process.env.npm_execpath ??
      (await realpath(join(dirname(process.execPath), "npm"))),
    join(bin, "npm"),
  );
  const native = join(isolated, "native");
  await mkdir(native);
  for (const [name, variable] of [
    ["trellis", "TRELLIS_TEST_CLI_BIN"],
    ["trellis-server", "TRELLIS_TEST_SERVER_BIN"],
  ])
    await cp(
      process.env[variable] ?? join(repository, "target/debug", name),
      join(native, name),
    );
  const env = {
    ...process.env,
    PATH: `${bin}:/usr/bin:/bin`,
    NODE_PATH: "",
    TRELLIS_TEST_CLI_BIN: join(native, "trellis"),
    TRELLIS_TEST_SERVER_BIN: join(native, "trellis-server"),
    TRELLIS_CACHE: join(isolated, "empty-api-cache"),
  };
  await assert.rejects(run("sh", ["-c", "command -v trellis"], { env }));
  for (const args of [
    [
      "install",
      "--ignore-scripts",
      "--install-links",
      "--no-audit",
      "--no-fund",
    ],
    ["run", "check"],
    ["test"],
  ]) {
    const result = await run(
      process.execPath,
      [
        process.env.npm_execpath ??
          (await realpath(join(dirname(process.execPath), "npm"))),
        ...args,
      ],
      {
        cwd: project,
        env,
        maxBuffer: 4 * 1024 * 1024,
      },
    );
    process.stdout.write(result.stdout);
    process.stderr.write(result.stderr);
  }
} finally {
  await rm(isolated, { recursive: true, force: true });
}
