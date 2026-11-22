import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const repository = fileURLToPath(new URL("../../", import.meta.url));

// An ordinary npm consumer of the generated package: it names the package and
// its subpaths, never the generated files or the root barrel.
const service = `
import { ok } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { API } from "runtime-trellis/apis/runtime";
import { participant } from "runtime-trellis/participants/EchoProvider";
const [trellisUrl, seed] = process.argv.slice(2);
const service = await TrellisService.connect({ trellisUrl, participant, seed }).orThrow();
await service.handleEcho(({ input }) => ok({ value: \`\${API.identity} \${input.value}\` }));
console.log("ready");
await service.wait();
`;

test("a service imported by package subpaths answers a real call", async () => {
  const directory = await mkdtemp(join(tmpdir(), "trellis-subpaths-"));
  try {
    await mkdir(join(directory, "node_modules/@oatscenter"), {
      recursive: true,
    });
    await symlink(
      join(repository, "integration/fixtures/runtime/packages/runtime-trellis"),
      join(directory, "node_modules/runtime-trellis"),
      "dir",
    );
    for (const name of ["trellis", "result"])
      await symlink(
        join(repository, "ts/packages", name),
        join(directory, "node_modules/@oatscenter", name),
        "dir",
      );
    await writeFile(
      join(directory, "package.json"),
      JSON.stringify({ type: "module" }),
    );
    await writeFile(join(directory, "service.js"), service);

    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: "subpath-consumer",
        contract: participants.EchoProvider.participant,
      });
      const child = spawn(
        process.execPath,
        ["service.js", runtime.trellisUrl, identity.seed],
        {
          cwd: directory,
          env: {
            ...process.env,
            NODE_EXTRA_CA_CERTS: runtime.caCertificatePath,
          },
          stdio: ["ignore", "pipe", "pipe"],
        },
      );
      const exited = once(child, "exit");
      let output = "";
      child.stderr.on("data", (chunk) => (output += chunk));
      try {
        await new Promise<void>((resolve, reject) => {
          child.stdout.on("data", (chunk) => {
            output += chunk;
            if (output.includes("ready")) resolve();
          });
          void exited.then(() =>
            reject(new Error(`consumer exited early:\n${output}`)),
          );
        });
        const caller = await runtime.connectClient({
          name: "subpath-caller",
          contract: apps.Caller.request,
        });
        try {
          assert.deepEqual(await caller.echo({ value: "hello" }).orThrow(), {
            value: "localhost/runtime-trellis.runtime@v1 hello",
          });
        } finally {
          await caller.connection.close();
        }
      } finally {
        child.kill("SIGTERM");
        await exited;
      }
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
