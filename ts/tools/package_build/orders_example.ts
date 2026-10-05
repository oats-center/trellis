import { assertEquals } from "@std/assert";
import { copy, ensureDir } from "@std/fs";
import { dirname, fromFileUrl, join } from "@std/path";
import { z } from "zod";

const repository = fromFileUrl(new URL("../../../", import.meta.url));
const isolated = await Deno.makeTempDir({ prefix: "trellis-orders-consumer-" });
const project = join(isolated, "orders");
const configSchema = z.object({
  extends: z.string().optional(),
  imports: z.record(z.string(), z.string()),
  links: z.array(z.string()).optional(),
})
  .passthrough();

async function run(
  command: string,
  args: string[],
  env?: Record<string, string>,
) {
  const result = await new Deno.Command(command, {
    args,
    cwd: project,
    env,
    stdout: "piped",
    stderr: "inherit",
  }).output();
  const output = new TextDecoder().decode(result.stdout);
  assertEquals(result.code, 0, `${command} ${args.join(" ")}\n${output}`);
  return output;
}

try {
  const testkit = join(project, "testkit");
  for (
    const [source, destination] of [
      ["docs/examples/orders", project],
      ["ts/packages/trellis-testkit", testkit],
    ]
  ) {
    const files = await new Deno.Command("git", {
      args: ["ls-files", "-z", "--", source],
      cwd: repository,
    }).output();
    assertEquals(files.success, true, `Could not list ${source} sources`);
    for (
      const file of new TextDecoder().decode(files.stdout).split("\0").filter(
        Boolean,
      )
    ) {
      if (
        file.split("/").some((part) =>
          part === ".trellis" || part === "trellis"
        )
      ) continue;
      const target = join(destination, file.slice(source.length + 1));
      await ensureDir(dirname(target));
      await Deno.copyFile(join(repository, file), target);
    }
    // Generated packages are ignored and must be prepared before this harness.
    await copy(
      join(repository, source, "trellis"),
      join(destination, "trellis"),
    );
  }
  const testConfig = configSchema.parse(JSON.parse(
    await Deno.readTextFile(join(testkit, "deno.json")),
  ));
  // Keep the testkit's own dependencies, without the repository workspace.
  delete testConfig.extends;
  for (const [name, specifier] of Object.entries(testConfig.imports)) {
    if (
      name === "@oatscenter/trellis" ||
      name.startsWith("@oatscenter/trellis/")
    ) {
      testConfig.imports[name] = specifier.replace(/^jsr:/, "npm:");
    }
  }
  await Deno.writeTextFile(
    join(testkit, "deno.json"),
    JSON.stringify(testConfig),
  );
  const config = configSchema.parse(
    JSON.parse(await Deno.readTextFile(join(project, "deno.json"))),
  );
  config.links = ["./testkit"];
  config.nodeModulesDir = "auto";
  for (const name of ["result", "trellis"]) {
    await copy(
      join(repository, "ts/packages", name, "npm"),
      join(project, ".sdk", name),
    );
    config.links.push(`./.sdk/${name}`);
  }
  // The example pins the published version range; for a prerelease candidate
  // (e.g. 0.100.0-rc.2) that range no longer matches the local package, so pin
  // the example's @oatscenter/trellis imports to the locally built version.
  const trellisVersion = JSON.parse(
    await Deno.readTextFile(join(project, ".sdk", "trellis", "package.json")),
  ).version as string;
  for (const [name, specifier] of Object.entries(config.imports)) {
    if (
      name === "@oatscenter/trellis" || name.startsWith("@oatscenter/trellis/")
    ) {
      config.imports[name] = specifier.replace(
        /^npm:@oatscenter\/trellis@[^/]+/,
        `npm:@oatscenter/trellis@${trellisVersion}`,
      );
    }
    if (name === "@oatscenter/trellis-testkit") {
      const testkitVersion = JSON.parse(
        await Deno.readTextFile(join(testkit, "deno.json")),
      ).version as string;
      config.imports[name] = specifier.replace(
        /^(jsr:@oatscenter\/trellis-testkit@)[^/]+/,
        `$1${testkitVersion}`,
      );
    }
  }
  await Deno.writeTextFile(join(project, "deno.json"), JSON.stringify(config));

  const bin = join(isolated, "bin");
  await ensureDir(bin);
  const native = join(isolated, "native");
  await ensureDir(native);
  for (
    const [name, variable] of [
      ["trellis", "TRELLIS_TEST_CLI_BIN"],
      ["trellis-server", "TRELLIS_TEST_SERVER_BIN"],
    ]
  ) {
    await Deno.copyFile(
      Deno.env.get(variable) ?? join(repository, "target/debug", name),
      join(native, name),
    );
  }
  await Deno.symlink(Deno.execPath(), join(bin, "deno"));
  const node = await new Deno.Command("node", {
    args: ["-p", "process.execPath"],
  }).output();
  assertEquals(node.success, true, "Could not locate installed Node");
  await Deno.symlink(
    new TextDecoder().decode(node.stdout).trim(),
    join(bin, "node"),
  );
  const env = {
    PATH: `${bin}:/usr/bin:/bin`,
    NODE_PATH: "",
    TRELLIS_TEST_CLI_BIN: join(native, "trellis"),
    TRELLIS_TEST_SERVER_BIN: join(native, "trellis-server"),
    TRELLIS_CACHE: join(isolated, "empty-api-cache"),
  };
  assertEquals(
    (await new Deno.Command("sh", {
      args: ["-c", "command -v trellis"],
      env,
    }).output()).success,
    false,
    "isolated consumer must not have a Trellis CLI on PATH",
  );
  console.log(await run(Deno.execPath(), ["install"], env));
  console.log(await run(Deno.execPath(), ["task", "check"], env));
  console.log(await run(Deno.execPath(), ["task", "test"], env));
  console.log(
    "Orders builds and tests with a managed native distribution, no Trellis CLI on PATH, no API cache, and no parent repository imports.",
  );
} finally {
  await Deno.remove(isolated, { recursive: true });
}
