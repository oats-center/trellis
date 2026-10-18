import { execFile } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs, promisify } from "node:util";
import { z } from "zod";

const root = fileURLToPath(new URL("../../", import.meta.url));
const exec = promisify(execFile);
const { values: args } = parseArgs({
  options: {
    cli: { type: "string" },
    output: { type: "string" },
    actions: { type: "string", default: "1,32,128" },
  },
});
const cli = resolve(z.string().parse(args.cli));
const output = resolve(z.string().parse(args.output));
const counts = z
  .array(z.coerce.number().int().positive().max(1024))
  .nonempty()
  .parse(args.actions.split(","));
for (const count of counts) {
  const dir = `${output}/contracts-${count}`;
  await mkdir(dir, { recursive: true });
  const actions = Array.from({ length: count }, (_, index) => `Echo${index}`);
  await writeFile(
    `${dir}/contract.trellis`,
    `import { auth } from trellis;
model Value { value: string; }
api scale@v1 {
  title "Contract scale";
  description "Measure authorization and connection cost with a growing complete RPC surface.";
  ${actions.map((name) => `rpc ${name} { input Value; output Value; }`).join("\n  ")}
  capabilities {
    capability workloads {
      title "Run scaled workloads";
      description "Invoke the measured generated RPC surface.";
      consequence "The caller may invoke every scaled Echo action.";
      consent_revision 1;
      allows { ${actions.map((name) => `rpc ${name};`).join(" ")} }
    }
  }
}
service Provider { implements scale; }
agent Caller {
  use scale { required capability workloads; }
  use auth { required capability accountRead; }
}
`,
  );
  await writeFile(
    `${dir}/trellis.toml`,
    `[package]
name = "scaleperf${count}"
version = "0.0.0"
[sources]
scale = "contract.trellis"
[generate.typescript]
output = "packages/scale"
[dependencies]
trellis = { package = "trellis", path = ${JSON.stringify(relative(dir, `${root}/crates/builtin-contracts`))} }
`,
  );
  const started = performance.now();
  await exec(cli, ["update"], { cwd: dir });
  const generated = await exec(cli, ["generate"], { cwd: dir });
  await writeFile(`${dir}/generation.log`, generated.stdout + generated.stderr);
  await writeFile(
    `${dir}/generation.json`,
    JSON.stringify({
      actions: count,
      generationMs: performance.now() - started,
    }),
  );
  await writeFile(
    `${dir}/package.json`,
    JSON.stringify(
      {
        private: true,
        type: "module",
        dependencies: {
          "@oatscenter/trellis": `file:${root}/ts/packages/trellis`,
          "@oatscenter/result": `file:${root}/ts/packages/result`,
        },
      },
      null,
      2,
    ),
  );
  await exec(
    "npm",
    [
      "install",
      "--offline",
      "--ignore-scripts",
      "--package-lock=false",
      "--no-audit",
      "--no-fund",
    ],
    { cwd: dir },
  );
  // Compile a real consumer of this exact generated package, not its text or AST.
  await writeFile(
    `${dir}/worker.ts`,
    `
import { once } from "node:events";
import { writeFile } from "node:fs/promises";
import { text } from "node:stream/consumers";
import { ok } from "@oatscenter/result";
import { connectNativeClient, discoverNativeOAuth, NativeOAuthClient, type ConnectedNativeClient } from "@oatscenter/trellis/native";
import { TrellisService } from "@oatscenter/trellis/service";
import { apps, participants } from "./packages/scale/index.js";
import { loginNativeTestClient } from ${JSON.stringify(`./${relative(dir, `${root}/ts/packages/trellis-testkit/src/admin/native_auth.ts`)}`)};
import { WorkerOptions, summarize, type Sample } from ${JSON.stringify(`./${relative(dir, `${root}/benchmarks/runtime/model.ts`)}`)};
const options = WorkerOptions.parse(JSON.parse(await text(process.stdin)));
if (options.role === "provider") {
  if (!options.serviceSeed) throw new Error("Service seed missing");
  const service = await TrellisService.connect({ trellisUrl: options.trellisUrl, participant: participants.Provider.participant, seed: options.serviceSeed, name: "contract-scale-provider" }).orThrow();
  ${actions.map((name) => `await service.handle${name}(({ input }) => ok(input));`).join("\n  ")}
  const serving = service.wait();
  await writeFile(options.output + "/provider-" + options.providerIndex + ".json", JSON.stringify({ pid: process.pid, ready: true }));
  try { await Promise.race([serving, once(process, "SIGTERM")]); }
  finally { await service.stop(); await serving; }
} else {
  const samples: Sample[] = [];
  for (let index = 0; index < options.samples; index++) {
    for (const resume of [false, true]) {
      const started = performance.now();
      const row: Sample = { scenario: "contract/${count}-actions/" + (resume ? "resume-first-rpc" : "fresh-login-first-rpc"), transport: "trellis", startedUnixMs: Date.now(), durationMs: 0 };
      await writeFile(options.output + "/phase.txt", row.scenario);
      let native: NativeOAuthClient | undefined;
      let client: ConnectedNativeClient<typeof apps.Caller.request> | undefined;
      try {
        const server = (await discoverNativeOAuth(options.trellisUrl)).orThrow();
        const directory = options.output + "/credentials";
        const credentialHandle = options.credentialHandles[index];
        if (!credentialHandle) throw new Error("Missing credential handle");
        native = (await NativeOAuthClient.open({ directory, scope: { issuer: server.issuer, instanceId: server.trust.instanceId, account: server.trust.account, clientId: apps.Caller.request.id, credentialHandle }, trust: server.trust })).orThrow();
        const login = resume
          ? { session: (await native.resume()).orThrow(), servers: server.natsServers }
          : await loginNativeTestClient({ issuer: server.issuer, directory, client: native, clientId: apps.Caller.request.id, request: apps.Caller.request, username: options.username, password: options.password });
        client = await connectNativeClient({ ...login, request: apps.Caller.request, name: "contract-scale-caller" }).orThrow();
        const connected = performance.now();
        const value = "scaled-" + index;
        if ((await client.echo0({ value }).orThrow()).value !== value) throw new Error("Incorrect generated response");
        row.durationMs = performance.now() - started;
        row.connectMs = connected - started;
        row.firstRpcMs = performance.now() - connected;
        if (resume) await client.sessionsLogout({}).orThrow();
      } catch (error) { row.durationMs = performance.now() - started; row.error = String(error); }
      finally { if (client) await client.connection.close(); if (native) await native.close(); }
      samples.push(row);
      await writeFile(options.output + "/samples.json", JSON.stringify({ samples, summary: summarize(samples), windows: [] }, null, 2));
    }
  }
  if (samples.some((row) => row.error)) throw new Error("Contract-scale workload failed; samples retained");
}
`,
  );
  await exec(
    process.execPath,
    [
      `${root}/node_modules/typescript/bin/tsc`,
      "--ignoreConfig",
      "--noEmit",
      "--strict",
      "--target",
      "ESNext",
      "--module",
      "NodeNext",
      "--allowImportingTsExtensions",
      "--types",
      "node",
      "--typeRoots",
      `${root}/node_modules/@types`,
      `${dir}/worker.ts`,
    ],
    { cwd: dir },
  );
}
