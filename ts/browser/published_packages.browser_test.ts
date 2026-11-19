// Services pre-approved from their published Trellis packages: the CLI and the
// Console name a release in a real OCI registry, Auth pulls and checks it, and
// a service running that build connects with nothing left to approve.

import assert from "node:assert/strict";
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { cp, mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { chromium } from "playwright";
import { codecs } from "@oatscenter/trellis/generated";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as extendedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  browserRuntimeOptions,
  launchProfile,
  loginCliAsAdministrator,
  startedServices,
} from "./browser_test_support.ts";
import {
  openConsoleAsRuntimeAdmin,
  waitForConsoleShell,
} from "./console_test_support.ts";

const repo = fileURLToPath(new URL("../../", import.meta.url));
const run = promisify(execFile);
const PROVIDER = participants.Provider.participant;
const EXTENDED = extendedParticipants.Provider.participant;
const KEEPER = participants.GrantKeeper.participant;

/**
 * A CNCF Distribution registry on a free loopback port, storing in `root`.
 * With `htpasswd`, every request must sign in as a user that file lists.
 */
async function startRegistry(
  root: string,
  htpasswd?: string,
): Promise<{
  host: string;
  stop: () => Promise<void>;
}> {
  const binary = process.env.TRELLIS_TEST_REGISTRY_BIN;
  assert.ok(
    binary,
    "supply the pinned registry (scripts/run-browser-tests.sh)",
  );
  const port = await new Promise<number>((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() =>
        resolve(typeof address === "object" && address ? address.port : 0),
      );
    });
  });
  const host = `127.0.0.1:${port}`;
  await mkdir(join(root, "storage"), { recursive: true });
  const config = join(root, "config.yml");
  await writeFile(
    config,
    `version: 0.1\nlog:\n  level: error\nstorage:\n  filesystem:\n    rootdirectory: ${join(root, "storage")}\nhttp:\n  addr: ${host}\n${htpasswd ? `auth:\n  htpasswd:\n    realm: trellis-test\n    path: ${htpasswd}\n` : ""}`,
  );
  const child: ChildProcess = spawn(binary, ["serve", config], {
    stdio: "ignore",
  });
  const deadline = Date.now() + 15_000;
  for (;;) {
    try {
      const { status } = await fetch(`http://${host}/v2/`);
      if (status === 200 || status === 401) break;
    } catch {
      // Not listening yet.
    }
    if (Date.now() > deadline) throw new Error("the registry never started");
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return {
    host,
    stop: async () => {
      if (child.exitCode === null) {
        child.kill("SIGTERM");
        await once(child, "exit");
      }
    },
  };
}

/**
 * Publish the built-in `trellis` package, the runtime fixture as
 * runtime-trellis 0.100.0, and the pinned fixture as 0.100.1 to `prefix` with
 * the producer-built CLI, as a service's own pipeline would. The fixtures
 * depend on `trellis` from that registry, so each release's frozen
 * resolution names it there.
 */
async function publishFixtures(
  workdir: string,
  prefix: string,
  login?: { username: string; password: string },
): Promise<Record<string, string>> {
  const binary = process.env.TRELLIS_TEST_CLI_BIN;
  assert.ok(binary, "supply a producer-built CLI");
  // The publisher's own Docker login, never the machine's; without one it
  // pushes and pulls anonymously.
  const docker = join(workdir, "docker-config");
  await mkdir(docker, { recursive: true });
  if (login)
    await writeFile(
      join(docker, "config.json"),
      JSON.stringify({
        auths: {
          [prefix.split("/")[0]]: {
            auth: Buffer.from(`${login.username}:${login.password}`).toString(
              "base64",
            ),
          },
        },
      }),
    );
  const env = {
    ...process.env,
    TRELLIS_CACHE: join(workdir, "package-cache"),
    DOCKER_CONFIG: docker,
  };
  const builtin = join(workdir, "publish", "trellis");
  await cp(join(repo, "crates/builtin-contracts/src"), join(builtin, "src"), {
    recursive: true,
  });
  const manifest = await readFile(
    join(repo, "crates/builtin-contracts/trellis.toml"),
    "utf8",
  );
  const version = /^version = "([^"]+)"/m.exec(manifest)?.[1];
  assert.ok(version);
  await writeFile(
    join(builtin, "trellis.toml"),
    manifest.replace(/prefix = "[^"]+"/, `prefix = "${prefix}"`),
  );
  const fixture = async (from: string, release: string) => {
    const directory = join(workdir, "publish", `runtime-${release}`);
    await mkdir(directory, { recursive: true });
    await cp(
      join(repo, from, "contract.trellis"),
      join(directory, "contract.trellis"),
    );
    await writeFile(
      join(directory, "trellis.toml"),
      `default-registry = "local"\n\n[package]\nname = "runtime-trellis"\nversion = "${release}"\n\n[sources]\nruntime = "contract.trellis"\n\n[dependencies]\ntrellis = { package = "trellis", version = "=${version}", registry = "local" }\n\n[registries.local]\nprefix = "${prefix}"\n`,
    );
    return directory;
  };
  const digests: Record<string, string> = {};
  for (const directory of [
    builtin,
    await fixture("integration/fixtures/runtime", "0.100.0"),
    await fixture("integration/fixtures/runtime-pinned", "0.100.1"),
  ]) {
    await run(binary, ["update", "--root", directory], { env });
    const published = JSON.parse(
      (
        await run(
          binary,
          ["--format", "json", "publish", "--root", directory],
          {
            env,
          },
        )
      ).stdout,
    );
    digests[`${published.name}@${published.version}`] = published.digest;
  }
  return digests;
}

/** Whether the deployment has any service update, waiting or decided. */
async function updates(runtime: TrellisTestRuntime, deploymentId: string) {
  const { items } = await runtime.callAdminRpc("authServiceUpdatesList", {
    deploymentId: codecs.ulid.decode(deploymentId),
  });
  return items;
}

test("the CLI installs a published package so its service connects at once, and refuses a tampered one", async () => {
  const services = startedServices();
  let stopRegistry = async () => {};
  try {
    await withTrellisRuntime(async (runtime) => {
      const registry = await startRegistry(join(runtime.workdir, "registry"));
      stopRegistry = registry.stop;
      const prefix = `${registry.host}/packages`;
      const digests = await publishFixtures(runtime.workdir, prefix);

      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      let cli: Awaited<ReturnType<typeof loginCliAsAdministrator>>;
      try {
        cli = await loginCliAsAdministrator(runtime, await browser.newPage());
      } finally {
        await browser.close();
      }
      const binary = process.env.TRELLIS_TEST_CLI_BIN!;
      const failing = async (...args: string[]) => {
        const error = await run(binary, ["--format", "json", ...args], {
          env: { ...process.env, XDG_CONFIG_HOME: cli.directory },
          timeout: 15_000,
        }).then(
          () => assert.fail(`${args.join(" ")} succeeded`),
          (error: { stderr: string }) => error.stderr,
        );
        return error;
      };

      // Without a registry Trellis has nowhere to pull from.
      assert.match(
        await failing(
          "svc",
          "early",
          "create",
          "--package",
          "runtime-trellis@0.100.0",
          "--participant",
          PROVIDER.identity,
        ),
        /no registry/,
      );
      await cli.run("registries", "set", "local", "--prefix", prefix);
      assert.deepEqual(
        (await cli.run("registries", "list")).registries.map(
          (item: { name: string; prefix: string }) => [item.name, item.prefix],
        ),
        [["local", prefix]],
      );

      // The fixture declares many services; --participant picks one.
      const created = await cli.run(
        "svc",
        "from-package",
        "create",
        "--package",
        "runtime-trellis@0.100.0",
        "--participant",
        PROVIDER.identity,
      );
      const deploymentId: string = created.deployment.deploymentId;
      assert.equal(created.deployment.participantId, PROVIDER.identity);
      assert.equal(created.deployment.implementationDigest, PROVIDER.digest);
      const first = await cli.run("svc", "from-package", "provision");
      const running = services.add(
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: PROVIDER,
          seed: first.instanceSeed,
          signal: services.signal,
        }).orThrow(),
      );
      // Its build is the installed one: it connects, and submits nothing.
      await (await running).stop();
      assert.deepEqual(await updates(runtime, deploymentId), []);

      // Installing the next release before rolling it out lets the new build
      // connect at once, though it asks for storage the old one never had.
      const applied = await cli.run(
        "svc",
        "from-package",
        "apply",
        "--package",
        "runtime-trellis@0.100.1",
      );
      assert.equal(applied.deployment.implementationDigest, EXTENDED.digest);
      const second = await cli.run("svc", "from-package", "provision", "--new");
      await services.add(
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: EXTENDED,
          seed: second.instanceSeed,
          signal: services.signal,
        }).orThrow(),
      );
      assert.deepEqual(await updates(runtime, deploymentId), []);

      // A release pinned by its manifest digest installs that exact release,
      // here another service of the same package.
      const pinned = await cli.run(
        "svc",
        "pinned",
        "create",
        "--package",
        `runtime-trellis@${digests["runtime-trellis@0.100.0"]}`,
        "--participant",
        KEEPER.identity,
      );
      assert.equal(pinned.deployment.implementationDigest, KEEPER.digest);
      // A digest that names another package's release is not this package.
      assert.match(
        await failing(
          "svc",
          "mismatched",
          "create",
          "--package",
          `runtime-trellis@${Object.entries(digests).find(([release]) => release.startsWith("trellis@"))![1]}`,
          "--participant",
          KEEPER.identity,
        ),
        /couldn't pull/,
      );

      // Change the stored source layer of 0.100.0 under the registry; the
      // registry serves it unchanged, and Auth refuses what doesn't match.
      const manifest = await (
        await fetch(
          `http://${registry.host}/v2/packages/runtime-trellis/manifests/0.100.0`,
          { headers: { accept: "application/vnd.oci.image.manifest.v1+json" } },
        )
      ).json();
      const layer: string = manifest.layers[0].digest.slice("sha256:".length);
      const stored = join(
        runtime.workdir,
        "registry/storage/docker/registry/v2/blobs/sha256",
        layer.slice(0, 2),
        layer,
        "data",
      );
      const bytes = await readFile(stored);
      bytes[600] ^= 0xff;
      await writeFile(stored, bytes);
      assert.match(
        await failing(
          "svc",
          "tampered",
          "create",
          "--package",
          "runtime-trellis@0.100.0",
          "--participant",
          KEEPER.identity,
        ),
        /digest/i,
      );
      // Nothing refused was added.
      const { deployments } = await cli.run("svc", "list");
      const titles = deployments.map((item: { title: string }) => item.title);
      for (const refused of ["early", "mismatched", "tampered"])
        assert.equal(titles.includes(refused), false, refused);
      assert.ok(titles.includes("pinned"));
    }, browserRuntimeOptions());
  } finally {
    await services.stop();
    await stopRegistry();
  }
});

test("the Console adds a service from a published package and installs its next release", async () => {
  const services = startedServices();
  let stopRegistry = async () => {};
  try {
    await withTrellisRuntime(async (runtime) => {
      const registry = await startRegistry(join(runtime.workdir, "registry"));
      stopRegistry = registry.stop;
      const prefix = `${registry.host}/packages`;
      await publishFixtures(runtime.workdir, prefix);
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(
          `${runtime.consoleUrl}/console/admin/services/registries`,
          { waitUntil: "domcontentloaded" },
        );
        await waitForConsoleShell(page);
        const add = page.getByRole("form", { name: "Add a registry" });
        await add.getByLabel("Name", { exact: true }).fill("local");
        await add.getByLabel("Address", { exact: true }).fill(prefix);
        await add
          .getByRole("button", { name: "Add registry", exact: true })
          .click();
        await page.getByRole("cell", { name: prefix }).waitFor();

        await page.goto(`${runtime.consoleUrl}/console/admin/services/new`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        await page.getByLabel("Name", { exact: true }).fill("console-package");
        await page.getByLabel("From a published package").check();
        // The only registry is chosen already.
        await page
          .getByLabel("Package", { exact: true })
          .fill("runtime-trellis");
        await page.getByLabel("Version", { exact: true }).fill("0.100.0");
        await page
          .getByRole("button", { name: "Look up package", exact: true })
          .click();
        const review = page.getByRole("region", { name: "Published package" });
        await review.getByText("runtime-trellis 0.100.0").waitFor();
        await review.getByText(/Built with trellis /).waitFor();
        await page.getByLabel("Which service").selectOption(PROVIDER.identity);
        await page
          .getByRole("button", { name: "Add service", exact: true })
          .click();
        const dialog = page.getByRole("dialog", {
          name: "Save the service's key",
        });
        const seed = (
          await dialog.getByLabel("Service key").textContent()
        )?.trim();
        assert.ok(seed);
        await dialog
          .getByRole("button", { name: "I've saved it", exact: true })
          .click();
        await page
          .getByRole("link", { name: "Open service", exact: true })
          .click();
        await page
          .getByRole("heading", { name: "console-package", exact: true })
          .waitFor({ state: "visible", timeout: 30_000 });
        const deploymentId = new URL(page.url()).pathname.split("/").at(-1)!;

        // The build the Console installed is the one the service runs.
        const running = services.add(
          TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: PROVIDER,
            seed,
            signal: services.signal,
          }).orThrow(),
        );
        await (await running).stop();
        assert.deepEqual(await updates(runtime, deploymentId), []);

        // The service page installs the next release ahead of its rollout.
        await page
          .getByRole("button", {
            name: "From a published package",
            exact: true,
          })
          .click();
        const install = page.getByRole("form", {
          name: "Install a published build",
        });
        await install
          .getByLabel("Package", { exact: true })
          .fill("runtime-trellis");
        await install.getByLabel("Version", { exact: true }).fill("0.100.1");
        await install
          .getByRole("button", { name: "Look up package", exact: true })
          .click();
        await install
          .getByRole("region", { name: "Published package" })
          .getByText("runtime-trellis 0.100.1")
          .waitFor();
        await install
          .getByRole("button", { name: "Install build", exact: true })
          .click();
        await page.getByText("Installed runtime-trellis 0.100.1.").waitFor();
        const { deployment } = await runtime.callAdminRpc(
          "authDeploymentsGet",
          {
            deploymentId: codecs.ulid.decode(deploymentId),
          },
        );
        assert.equal(deployment.implementationDigest, EXTENDED.digest);
      } finally {
        await context.close();
      }
    }, browserRuntimeOptions());
  } finally {
    await services.stop();
    await stopRegistry();
  }
});

test("Auth signs in to a private registry with the sealed secret an administrator stored", async () => {
  // A bcrypt hash of PASSWORD, as `htpasswd -B` writes it.
  const USERNAME = "publisher";
  const PASSWORD = "registry-test-secret";
  const HASH = "$2b$05$Z6yQM36nPqzMTeONgCReBeU6B2YD4r4HOk5lFyTCYiWK4HiAZ3Tti";
  let stopRegistry = async () => {};
  await withTrellisRuntime(async (runtime) => {
    try {
      const root = join(runtime.workdir, "registry");
      await mkdir(root, { recursive: true });
      await writeFile(join(root, "htpasswd"), `${USERNAME}:${HASH}\n`);
      const registry = await startRegistry(root, join(root, "htpasswd"));
      stopRegistry = registry.stop;
      const prefix = `${registry.host}/packages`;
      await publishFixtures(runtime.workdir, prefix, {
        username: USERNAME,
        password: PASSWORD,
      });
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      let cli: Awaited<ReturnType<typeof loginCliAsAdministrator>>;
      try {
        cli = await loginCliAsAdministrator(runtime, await browser.newPage());
      } finally {
        await browser.close();
      }
      const create = (title: string) =>
        cli.run(
          "svc",
          title,
          "create",
          "--package",
          "runtime-trellis@0.100.0",
          "--participant",
          KEEPER.identity,
        );

      // Anonymous pulls are turned away.
      await cli.run("registries", "set", "private", "--prefix", prefix);
      await assert.rejects(create("anonymous"), /couldn't pull/);

      const secret = join(runtime.workdir, "registry-secret");
      await writeFile(secret, `${PASSWORD}\n`);
      await cli.run(
        "registries",
        "set",
        "private",
        "--prefix",
        prefix,
        "--username",
        USERNAME,
        "--secret-file",
        secret,
      );
      const created = await create("signed-in");
      assert.equal(created.deployment.implementationDigest, KEEPER.digest);

      // The secret is never returned, and never stored as written.
      const listed = await cli.run("registries", "list");
      assert.equal(listed.registries[0].username, USERNAME);
      assert.doesNotMatch(JSON.stringify(listed), new RegExp(PASSWORD));
      const stores = (
        await readdir(runtime.workdir, { recursive: true })
      ).filter((entry) => /\.sqlite(-wal)?$/.test(entry));
      assert.ok(stores.length > 0, "the runtime keeps its state in SQLite");
      for (const entry of stores)
        assert.equal(
          (await readFile(join(runtime.workdir, entry))).includes(PASSWORD),
          false,
          entry,
        );
    } finally {
      await stopRegistry();
    }
  }, browserRuntimeOptions());
});
