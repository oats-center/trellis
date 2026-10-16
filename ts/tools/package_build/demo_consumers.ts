import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
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
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { ulid } from "ulid";
import { codecs } from "../../packages/trellis/generated.ts";
import { TrellisTestRuntime } from "../../packages/trellis-testkit/index.ts";
import { participants as serviceParticipants } from "../../../demos/ts/service/trellis/index.js";
import { participants as deviceParticipants } from "../../../demos/ts/device/trellis/index.js";

const repository = fileURLToPath(new URL("../../../", import.meta.url));
const isolated = await mkdtemp(join(tmpdir(), "trellis-demos-"));
const npm =
  process.env.npm_execpath ??
  (await realpath(join(dirname(process.execPath), "npm")));
const run = promisify(execFile);
try {
  const bin = join(isolated, "bin");
  await mkdir(bin);
  await symlink(process.execPath, join(bin, "node"));
  const env = {
    ...process.env,
    PATH: `${bin}:/usr/bin:/bin`,
    NODE_PATH: "",
    TRELLIS_CACHE: join(isolated, "empty-api-cache"),
    XDG_STATE_HOME: join(isolated, "state"),
    OTEL_SDK_DISABLED: "true",
  };
  for (const project of ["demos/app", "demos/ts"]) {
    const destination = join(isolated, project);
    await cp(join(repository, project), destination, {
      recursive: true,
      filter: (path) =>
        ![
          "node_modules",
          ".svelte-kit",
          ".trellis",
          "build",
          "deno.lock",
        ].includes(basename(path)),
    });
    const manifest = JSON.parse(
      await readFile(join(destination, "package.json"), "utf8"),
    );
    for (const name of ["result", "trellis", "trellis-svelte"]) {
      if (!manifest.dependencies[`@oatscenter/${name}`]) continue;
      await cp(
        join(repository, "ts/packages", name, "npm"),
        join(destination, ".sdk", name),
        { recursive: true },
      );
      manifest.dependencies[`@oatscenter/${name}`] = `file:./.sdk/${name}`;
    }
    await writeFile(
      join(destination, "package.json"),
      JSON.stringify(manifest),
    );
    for (const args of [
      [
        "install",
        "--ignore-scripts",
        "--install-links",
        "--no-audit",
        "--no-fund",
      ],
      ["run", "check"],
      ["run", "build"],
    ]) {
      const output = await run(process.execPath, [npm, ...args], {
        cwd: destination,
        env,
        maxBuffer: 4 * 1024 * 1024,
      });
      process.stdout.write(output.stdout);
      process.stderr.write(output.stderr);
    }
    if (project !== "demos/ts") continue;
    await using runtime = await TrellisTestRuntime.start({
      trellis: {
        source: {
          kind: "path",
          cli:
            process.env.TRELLIS_TEST_CLI_BIN ??
            join(repository, "target/debug/trellis"),
          server:
            process.env.TRELLIS_TEST_SERVER_BIN ??
            join(repository, "target/debug/trellis-server"),
        },
      },
    });
    const identity = await runtime.registerService({
      name: "native-demo",
      contract: serviceParticipants.Service.participant,
    });
    const deployment = await runtime.contracts.apply({
      deployment: "device",
      contract: deviceParticipants.Device.participant,
    });
    const provisioned = await runtime.devices.provision({
      deploymentId: codecs.ulid.decode(deployment.deploymentId),
      title: "Native demo device",
      idempotencyKey: ulid(),
    });
    assert(provisioned.provisioningSecret);
    const rootSecret = Buffer.from(
      crypto.getRandomValues(new Uint8Array(32)),
    ).toString("base64url");
    const service = spawn(
      process.execPath,
      ["service/build/service/src/main.js", runtime.oauthIssuer, identity.seed],
      { cwd: destination, env, stdio: ["ignore", "inherit", "inherit"] },
    );
    const serviceExit = once(service, "exit");
    try {
      const device = spawn(
        process.execPath,
        [
          "device/build/device/src/main.js",
          runtime.oauthIssuer,
          rootSecret,
          provisioned.provisioningSecret,
        ],
        { cwd: destination, env, stdio: ["pipe", "pipe", "inherit"] },
      );
      const deviceExit = once(device, "exit");
      const timeout = setTimeout(() => device.kill("SIGTERM"), 45_000);
      let output = "";
      let approved = false;
      let selections = 0;
      try {
        device.stdout.setEncoding("utf8");
        for await (const chunk of device.stdout) {
          output += chunk;
          const activationUrl = output.match(
            /Please activate device at: (\S+)/,
          )?.[1];
          const confirmationCode = output.match(
            /Confirmation code: (\S+)/,
          )?.[1];
          if (!approved && activationUrl && confirmationCode) {
            const reviewId = codecs.ulid.decode(
              new URL(activationUrl).searchParams.get("reviewId"),
            );
            assert(reviewId);
            const { review } = await runtime.callAdminRpc(
              "authDevicesReviewsGet",
              { reviewId, confirmationCode },
            );
            await runtime.callAdminRpc("authDevicesReviewsDecide", {
              reviewId,
              confirmationCode,
              expectedRevision: review.revision,
              decision: "approved",
              idempotencyKey: ulid(),
            });
            approved = true;
          }
          if ((output.match(/Select option: /g)?.length ?? 0) > selections)
            device.stdin.write(selections++ === 0 ? "1\n" : "0\n");
        }
        assert.equal((await deviceExit)[0], 0, output);
        assert(output.includes("Connected Field Device"), output);
        assert(output.includes("Assigned Inspections"), output);
        assert(output.includes("[HIGH]"), output);
      } finally {
        clearTimeout(timeout);
        if (device.exitCode === null && device.signalCode === null)
          device.kill("SIGTERM");
        await deviceExit;
      }
      // Exercise the packaged generated Device facade, not a checkout client.
      await writeFile(
        join(destination, "peer.mjs"),
        `
import assert from "node:assert/strict";
import { TrellisDevice } from "@oatscenter/trellis/device";
import { decodeTransferGrant } from "@oatscenter/trellis";
import { deriveDeviceIdentity, base64urlDecode } from "@oatscenter/trellis/auth";
import { discoverNativeOAuth } from "@oatscenter/trellis/native";
import { participants } from "./device/trellis/index.js";
const server = (await discoverNativeOAuth(process.argv[2])).orThrow();
const identity = await deriveDeviceIdentity(base64urlDecode(process.argv[3]));
const device = await TrellisDevice.connect({ participant: participants.Device.participant, seed: identity.identitySeedBase64url, servers: [...server.natsServers], trellisInstanceId: server.trust.instanceId, audienceNatsAccount: server.trust.account, issuers: [server.trust.issuer] }).orThrow();
try {
  const assignments = await device.assignmentsList({ page: { limit: 50 } }).orThrow();
  assert(assignments.items.length > 0);
  const bytes = new Uint8Array([0, 127, 255]);
  const upload = await device.evidenceUpload({ key: "evidence/node-roundtrip", evidenceType: "field-photo" }).transfer(bytes).start().orThrow();
  const uploaded = await upload.wait().orThrow();
  assert.equal(uploaded.terminal.state, "completed", JSON.stringify(uploaded.terminal, (_, value) => typeof value === "bigint" ? value.toString() : value));
  assert(uploaded.terminal.output);
  const download = await device.evidenceDownload({ key: uploaded.terminal.output.key }).orThrow();
  const grant = decodeTransferGrant(download.transfer).orThrow();
  assert.equal(grant.direction, "receive");
  assert.deepEqual(await device.transfer(grant).bytes().orThrow(), bytes);
  const operation = await device.reportsGenerate({ inspectionId: assignments.items[0].inspectionId, reportComment: "packaged Node acceptance" }).start().orThrow();
  const observation = await operation.live({}).orThrow();
  await observation.close().orThrow();
  await observation.closed;
  assert((await device.assignmentsList({ page: { limit: 50 } })).isOk());
} finally { await device.connection.close(); }
`,
      );
      const peer = await run(
        process.execPath,
        ["peer.mjs", runtime.oauthIssuer, rootSecret],
        { cwd: destination, env, timeout: 45_000, maxBuffer: 4 * 1024 * 1024 },
      );
      process.stdout.write(peer.stdout);
      process.stderr.write(peer.stderr);
    } finally {
      if (service.exitCode === null && service.signalCode === null)
        service.kill("SIGTERM");
      assert.equal((await serviceExit)[0], 0);
    }
  }
} finally {
  await rm(isolated, { recursive: true, force: true });
}
