import { copy, ensureDir } from "@std/fs";
import { dirname, fromFileUrl, join } from "@std/path";
import { z } from "zod";
import { assertEquals, assertStringIncludes } from "@std/assert";
import { TrellisTestRuntime } from "@oatscenter/trellis-test";
import { ulid } from "ulid";
import { participants as serviceParticipants } from "../../../demos/ts/service/trellis/index.js";
import { participants as deviceParticipants } from "../../../demos/ts/device/trellis/index.js";
import { participants as appParticipants } from "../../../demos/app/trellis/index.js";
import { participants as testParticipants } from "../../packages/trellis-test/trellis/index.js";

const repository = fromFileUrl(new URL("../../../", import.meta.url));
const isolated = await Deno.makeTempDir({ prefix: "trellis-demos-" });
const failures: unknown[] = [];

async function run(args: string[], cwd: string) {
  const status = await new Deno.Command(Deno.execPath(), {
    args,
    cwd,
    env: {
      PATH: `${join(isolated, "bin")}:/usr/bin:/bin`,
      NODE_PATH: "",
      TRELLIS_CACHE: join(isolated, "empty-api-cache"),
    },
    stdout: "inherit",
    stderr: "inherit",
  }).spawn().status;
  if (!status.success) {
    throw new Error(`deno ${args.join(" ")} failed (${status.code})`);
  }
}

try {
  await ensureDir(join(isolated, "bin"));
  await Deno.symlink(Deno.execPath(), join(isolated, "bin", "deno"));
  const node = await new Deno.Command("node", {
    args: ["-p", "process.execPath"],
  }).output();
  if (!node.success) throw new Error("Could not locate installed Node");
  await Deno.symlink(
    new TextDecoder().decode(node.stdout).trim(),
    join(isolated, "bin", "node"),
  );
  for (const project of ["demos/ts", "demos/app"]) {
    const destination = join(isolated, project);
    const files = await new Deno.Command("git", {
      args: [
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        project,
      ],
      cwd: repository,
    }).output();
    if (!files.success) throw new Error("Could not list demo sources");
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
      const target = join(isolated, file);
      await ensureDir(dirname(target));
      await Deno.copyFile(join(repository, file), target);
    }
    // Generated packages are deliberately ignored, but required before an ordinary build.
    for (
      const output of project === "demos/ts"
        ? ["service/trellis", "device/trellis"]
        : ["trellis"]
    ) {
      await copy(join(repository, project, output), join(destination, output));
    }
    const configPath = join(destination, "deno.json");
    const config = z.object({
      imports: z.record(z.string(), z.string()),
      links: z.array(z.string()).optional(),
    })
      .passthrough().parse(JSON.parse(await Deno.readTextFile(configPath)));
    config.links = [];
    for (
      const name of project === "demos/ts"
        ? ["result", "trellis"]
        : ["result", "trellis", "trellis-svelte"]
    ) {
      await copy(
        join(repository, "ts/packages", name, "npm"),
        join(destination, ".sdk", name),
      );
      config.links.push(`./.sdk/${name}`);
      const manifest = z.object({ name: z.string(), version: z.string() })
        .parse(
          JSON.parse(
            await Deno.readTextFile(
              join(destination, ".sdk", name, "package.json"),
            ),
          ),
        );
      config.imports[manifest.name] =
        `npm:${manifest.name}@${manifest.version}`;
    }
    await Deno.writeTextFile(configPath, JSON.stringify(config, null, 2));
    await run(["install"], destination);
    await run(["task", "check"], destination);
    if (project === "demos/ts") {
      await run(["task", "-c", "service/deno.json", "build"], destination);
      await run(["task", "-c", "device/deno.json", "build"], destination);
      const runtime = await TrellisTestRuntime.start({
        trellis: {
          command: {
            cmd: Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
              join(repository, "target/debug/trellis-server"),
            args: ["--config", "{config}", "all"],
          },
        },
      });
      try {
        const identity = await runtime.registerService({
          name: "native-demo",
          contract: serviceParticipants.Service.participant,
        });
        const caller = await runtime.connectClient({
          name: "demo-app",
          contract: appParticipants.App.participant,
        });
        const admin = await runtime.connectClient({
          name: "device-reviewer",
          contract: testParticipants.cli.participant,
        });
        await runtime.deployments.create({
          id: "device",
          kind: "device",
          reviewMode: "required",
        });
        const approval = await runtime.contracts.apply({
          deployment: "device",
          contract: deviceParticipants.Device.participant,
        });
        for (const engine of ["node", "deno"]) {
          const secret = crypto.getRandomValues(new Uint8Array(32));
          const rootSecret = btoa(String.fromCharCode(...secret))
            .replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
          const provisioned = await runtime.devices.provision({
            deploymentId: "device",
            idempotencyKey: ulid(),
            identityPublicKey: null,
            instanceId: null,
            participantId: deviceParticipants.Device.participant.id,
          });
          const { instanceId, principalId } = provisioned.device;
          const provisioningSecret = provisioned.provisioningSecret;
          if (!instanceId || !principalId || !provisioningSecret) {
            throw new Error(
              "Device provisioning returned incomplete credentials",
            );
          }
          const command = join(isolated, "bin", engine);
          const prefix = engine === "deno" ? ["run", "-A"] : [];
          const env = {
            PATH: `${join(isolated, "bin")}:/usr/bin:/bin`,
            NODE_PATH: "",
            TRELLIS_CACHE: join(isolated, "empty-api-cache"),
            XDG_STATE_HOME: join(isolated, "state", engine),
            OTEL_SDK_DISABLED: "true",
          };
          const service = new Deno.Command(command, {
            args: [
              ...prefix,
              "service/build/main.mjs",
              runtime.trellisUrl,
              identity.seed,
            ],
            cwd: destination,
            env,
            stdout: "inherit",
            stderr: "inherit",
          }).spawn();
          let exited = false;
          const serviceStatus = service.status.then((status) => {
            exited = true;
            return status;
          });
          try {
            const result = await runtime.waitFor(async () => {
              if (exited) {
                throw new Error(
                  `${engine} demo service exited (${
                    (await serviceStatus).code
                  })`,
                );
              }
              const response = await caller.assignmentsList(
                { page: { limit: 50 } },
                { timeout: 1000 },
              );
              return response.isOk() ? response.orThrow() : false;
            }, { timeoutMs: 30_000 });
            assertEquals(result.items.length > 0, true);
            // Direct use of the final owned Feed handle through the packaged
            // SDK: the synchronous close receipt, the once-resolving `closed`
            // promise, and async-disposal ownership.
            {
              const feed = await caller.auditFeed({}).orThrow();
              const receipt = await feed.close().orThrow();
              assertEquals(
                typeof receipt.cleanup,
                "string",
                `${engine} feed close receipt`,
              );
              await feed.closed;
              await using owned = await caller.auditFeed({}).orThrow();
              assertEquals(
                typeof owned[Symbol.asyncDispose],
                "function",
                `${engine} owned handle async disposal`,
              );
            }
            // Direct use of the final owned Operation handle: starting durable
            // work and closing its observation never cancels the operation.
            {
              const assignment = result.items[0];
              const operation = await caller.reportsGenerate({
                inspectionId: assignment.inspectionId,
                reportComment: "packaged-consumer acceptance",
              }).start().orThrow();
              await using observation = await operation.watch({}).orThrow();
              const receipt = await observation.close().orThrow();
              assertEquals(
                typeof receipt.cleanup,
                "string",
                `${engine} operation observation close receipt`,
              );
              assertEquals(
                (await caller.reportsList({ page: { limit: 50 } })).isOk(),
                true,
                `${engine} durable operation survives observer close`,
              );
            }
            const sites = await caller.sitesList({ page: { limit: 50 } });
            if (sites.isErr()) {
              failures.push(
                new Error(
                  `${engine} demo Sites.List: ${JSON.stringify(sites)}`,
                ),
              );
            }
            const device = new Deno.Command(command, {
              args: [
                ...prefix,
                "device/build/main.mjs",
                runtime.trellisUrl,
                rootSecret,
                provisioningSecret,
              ],
              cwd: destination,
              env,
              stdin: "piped",
              stdout: "piped",
              stderr: "inherit",
            }).spawn();
            let deviceExited = false;
            const deviceStatus = device.status.then((status) => {
              deviceExited = true;
              return status;
            });
            const writer = device.stdin.getWriter();
            let output = "";
            let selections = 0;
            let approved = false;
            const timeout = setTimeout(() => device.kill("SIGTERM"), 30_000);
            try {
              for await (
                const text of device.stdout.pipeThrough(new TextDecoderStream())
              ) {
                output += text;
                if (
                  !approved && output.includes("Please activate device at:")
                ) {
                  const review = await runtime.waitFor(async () => {
                    const reviews = await admin
                      .deviceUserAuthoritiesReviewsList({
                        deploymentId: approval.deploymentId,
                        state: "pending",
                      }).orThrow();
                    return reviews.items[0] ?? false;
                  });
                  const decision = await admin
                    .deviceUserAuthoritiesReviewsDecide({
                      decision: "approve",
                      expectedVersion: review.version,
                      idempotencyKey: ulid(),
                      reason: null,
                      reviewId: review.reviewId,
                    });
                  assertEquals(decision.isOk(), true);
                  approved = true;
                }
                if (
                  (output.match(/Select option: /g)?.length ?? 0) > selections
                ) {
                  await writer.write(
                    new TextEncoder().encode(
                      selections++ === 0 ? "1\n" : "0\n",
                    ),
                  );
                }
              }
              assertEquals((await deviceStatus).code, 0, output);
              assertStringIncludes(output, "Connected Field Device");
              assertStringIncludes(output, "Assigned Inspections");
              assertStringIncludes(output, "[HIGH]");
            } finally {
              clearTimeout(timeout);
              writer.releaseLock();
              if (!deviceExited) device.kill("SIGTERM");
              await deviceStatus;
            }
            console.log(
              `${engine}: actual demo service RPC and device assignment workflow passed`,
            );
          } catch (error) {
            console.error(`${engine}: native demo failure`, error);
            failures.push(error);
          } finally {
            if (!exited) service.kill("SIGTERM");
            assertEquals((await serviceStatus).code, 0);
          }
        }
      } finally {
        await runtime.stop();
      }
    }
    if (project === "demos/app") await run(["task", "build"], destination);
  }
  if (failures.length) {
    throw new AggregateError(failures, "Native demo workflow failures");
  }
} finally {
  await Deno.remove(isolated, { recursive: true });
}
