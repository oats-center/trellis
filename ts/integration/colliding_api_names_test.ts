/**
 * Two packages named `x`, published from `ghcr.io/acme-lab` and
 * `ghcr.io/acme/lab`, each serve an API `samples@v1`. Their names, short IDs
 * and punctuation-free full IDs are all the same, so a consumer of both gets
 * generated names with hash suffixes; it still builds, and each call reaches
 * the service that serves its own API.
 */
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { withTrellisRuntime } from "./_support/runtime.ts";

const run = promisify(execFile);
const repo = fileURLToPath(new URL("../../", import.meta.url));
const PROVIDERS = [
  { directory: "one", workspace: "ghcr.io/acme-lab", model: "One" },
  { directory: "two", workspace: "ghcr.io/acme/lab", model: "Two" },
] as const;

/** The flat method name the SDK derives from a generated action name. */
function methodName(name: string): string {
  const pascal = name
    .split(/[^A-Za-z0-9]+/u)
    .filter((part) => part.length > 0)
    .map((part) => part[0]!.toUpperCase() + part.slice(1))
    .join("");
  return pascal[0]!.toLowerCase() + pascal.slice(1);
}

test("a consumer of two APIs whose generated names collide calls each one", async () => {
  const binary = process.env.TRELLIS_TEST_CLI_BIN;
  assert.ok(binary, "supply a producer-built CLI");
  const root = await mkdtemp(join(tmpdir(), "colliding-apis-"));
  try {
    // The generated packages import the SDK the way checked-in ones do.
    await symlink(join(repo, "node_modules"), join(root, "node_modules"));
    for (const { directory, workspace, model } of PROVIDERS) {
      await mkdir(join(root, directory));
      await writeFile(
        join(root, directory, "trellis.toml"),
        `[package]\nname = "x"\nworkspace = "${workspace}"\nversion = "1.0.0"\n\n[sources]\nsamples = "samples.trellis"\n\n[generate.typescript]\noutput = "generated"\n`,
      );
      await writeFile(
        join(root, directory, "samples.trellis"),
        `model ${model}Sample { id: string; }\napi samples@v1 {\n  title "Samples";\n  description "Lab samples.";\n  rpc Get { input ${model}Sample; output ${model}Sample; }\n  capabilities { capability samples.read { title "Read samples"; description "Read samples."; grants "Sees samples."; allows { rpc Get; } } }\n}\nservice Lab { implements samples; }\n`,
      );
    }
    await mkdir(join(root, "consumer"));
    await writeFile(
      join(root, "consumer", "trellis.toml"),
      `[package]\nname = "consumer"\nversion = "1.0.0"\n\n[sources]\ncaller = "caller.trellis"\n\n[dependencies]\none = { package = "x", path = "../one" }\ntwo = { package = "x", path = "../two" }\n\n[generate.typescript]\noutput = "generated"\n`,
    );
    await writeFile(
      join(root, "consumer", "caller.trellis"),
      `import { samples as first } from one;\nimport { samples as second } from two;\nagent Caller {\n  use first { required capability samples.read; }\n  use second { required capability samples.read; }\n}\n`,
    );
    const env = { ...process.env, TRELLIS_CACHE: join(root, "cache") };
    for (const directory of ["one", "two", "consumer"])
      await run(binary, ["install", "--root", join(root, directory)], { env });
    const generated = (directory: string) =>
      import(pathToFileURL(join(root, directory, "generated/index.js")).href);
    const [one, two, consumer] = await Promise.all(
      ["one", "two", "consumer"].map(generated),
    );
    const caller = consumer.apps.Caller.request;
    const names: Record<string, string> = caller.actionNames;
    const method = (workspace: string) => {
      const name = names[`${workspace}/x.samples@v1:rpc:Get`];
      assert.ok(name, `no generated name for ${workspace}`);
      return methodName(name);
    };
    // Neither the API name nor a full ID tells the two apart in a name.
    assert.notEqual(method("ghcr.io/acme-lab"), method("ghcr.io/acme/lab"));
    assert.match(method("ghcr.io/acme-lab"), /^ghcrIoAcmeLabXSamplesV1/u);

    await withTrellisRuntime(async (runtime) => {
      const stops: Array<() => Promise<void>> = [];
      try {
        for (const [index, provider] of [one, two].entries()) {
          const participant = provider.participants.Lab.participant;
          const identity = await runtime.registerService({
            name: `lab-${PROVIDERS[index]!.directory}`,
            contract: participant,
          });
          const service = await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant,
            seed: identity.seed,
          }).orThrow();
          stops.push(async () => {
            await service.stop();
            await service.wait();
          });
          // Its generated package is built at test time, so its handler is
          // found by the name the generator chose.
          const handle: unknown = Reflect.get(service, "handleGet");
          if (typeof handle !== "function")
            throw new Error("the service has no handleGet");
          await Reflect.apply(handle, service, [
            ({ input }: { input: { id: string } }) =>
              Result.ok({ id: `${PROVIDERS[index]!.workspace}:${input.id}` }),
          ]);
        }
        const client = await runtime.connectClient({
          name: "colliding-caller",
          contract: caller,
        });
        try {
          // The generated packages are built at test time, so the client's
          // methods are looked up by the names the generator chose.
          const call = async (workspace: string) => {
            const invoke: unknown = Reflect.get(client, method(workspace));
            if (typeof invoke !== "function")
              throw new Error(`the client has no ${method(workspace)}`);
            const pending = Reflect.apply(invoke, client, [{ id: "s1" }]);
            return await pending.orThrow();
          };
          assert.deepEqual(await call("ghcr.io/acme-lab"), {
            id: "ghcr.io/acme-lab:s1",
          });
          assert.deepEqual(await call("ghcr.io/acme/lab"), {
            id: "ghcr.io/acme/lab:s1",
          });
        } finally {
          await client.connection.close();
        }
      } finally {
        await Promise.all(stops.map((stop) => stop()));
      }
    });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
