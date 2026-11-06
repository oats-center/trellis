import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { X509Certificate } from "node:crypto";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:https";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { rootCertificates } from "node:tls";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { ulid } from "ulid";

import {
  apps,
  participants,
  types,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const native = pathToFileURL(
  join(import.meta.dirname, "../packages/trellis/native.ts"),
).href;
const service = pathToFileURL(
  join(import.meta.dirname, "../packages/trellis/service/mod.ts"),
).href;
const fixture = pathToFileURL(
  join(
    import.meta.dirname,
    "../../integration/fixtures/runtime/packages/runtime-trellis/index.js",
  ),
).href;

/**
 * A program that trusts nothing beyond the system store: it accepts, as many
 * times at once as it is told, then discovers.
 */
const program = `
import { acceptServerCertificate, discoverNativeOAuth } from ${JSON.stringify(native)};
const [issuer, directory, count = "1"] = process.argv.slice(1);
const accepting = Array.from({ length: Number(count) }, () =>
  acceptServerCertificate(issuer, { directory, interactive: false }));
const accepted = (await Promise.all(accepting)).find((result) => result.isErr()) ?? (await accepting[0]);
if (accepted.isErr()) {
  console.log(JSON.stringify({ reason: accepted.error.reason, message: accepted.error.message }));
} else {
  const discovered = await discoverNativeOAuth(issuer);
  console.log(JSON.stringify(discovered.isErr() ? { reason: discovered.error.reason } : { issuer: discovered.orThrow().issuer }));
}
`;

test("a program accepts a development installation's certificate by its fingerprint", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const fingerprint = new X509Certificate(
        await readFile(runtime.caCertificate),
      ).fingerprint256;
      const run = async (
        directory: string,
        env: Record<string, string> = {},
        address = runtime.trellisUrl,
        count = 1,
      ): Promise<{ reason?: string; message?: string; issuer?: string }> => {
        const { NODE_EXTRA_CA_CERTS: _, ...inherited } = process.env;
        const { stdout } = await promisify(execFile)(
          process.execPath,
          [
            "--experimental-transform-types",
            "--no-warnings",
            "--input-type=module",
            "-e",
            program,
            address,
            directory,
            String(count),
          ],
          { env: { ...inherited, ...env } },
        );
        return JSON.parse(stdout.trim().split("\n").at(-1) ?? "{}");
      };
      const directory = await mkdtemp(join(tmpdir(), "trellis-accepted-"));

      // Nothing vouches for it and there's no terminal: refused, naming the
      // fingerprint to confirm.
      const refused = await run(directory);
      assert.equal(refused.reason, "UntrustedCertificate");
      assert(refused.message?.includes(fingerprint), refused.message);

      const accepted = await run(directory, {
        TRELLIS_CERTIFICATE_FINGERPRINT: fingerprint,
      });
      assert.equal(accepted.issuer, runtime.trellisUrl);
      // Remembered: the next start needs nothing.
      assert.equal((await run(directory)).issuer, runtime.trellisUrl);

      const fresh = await mkdtemp(join(tmpdir(), "trellis-accepted-"));
      // The Console's address, as people type it, leads to its sign-in.
      const selfSigned = await run(
        fresh,
        { TRELLIS_ACCEPT_SELF_SIGNED: "1" },
        runtime.consoleUrl,
      );
      assert.equal(selfSigned.issuer, runtime.trellisUrl);

      // A program opening two connections at its first start accepts the
      // same certificate twice at once; both must succeed.
      for (let attempt = 0; attempt < 5; attempt++) {
        const together = await run(
          await mkdtemp(join(tmpdir(), "trellis-accepted-")),
          { TRELLIS_CERTIFICATE_FINGERPRINT: fingerprint },
          runtime.trellisUrl,
          2,
        );
        assert.equal(together.issuer, runtime.trellisUrl, together.message);
      }
    },
    { development: true },
  );
});

/**
 * A program that accepts a certificate at one address, then at another whose
 * pin it already saved.
 */
const repinned = `
import { acceptServerCertificate } from ${JSON.stringify(native)};
const [first, second, directory, fingerprint] = process.argv.slice(1);
(await acceptServerCertificate(first, { directory, fingerprint, interactive: false })).orThrow();
const result = await acceptServerCertificate(second, { directory, interactive: false });
console.log(JSON.stringify(result.isErr() ? { reason: result.error.reason } : { accepted: true }));
`;

test("a certificate accepted for one address doesn't stand in for another's pin", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const fingerprint = new X509Certificate(
        await readFile(runtime.caCertificate),
      ).fingerprint256;
      // The installation's certificate names both addresses.
      const first = new URL(runtime.trellisUrl);
      first.hostname = "127.0.0.1";
      const second = new URL(runtime.trellisUrl);
      second.hostname = "localhost";
      const directory = await mkdtemp(join(tmpdir(), "trellis-accepted-"));
      // This program once accepted a different issuer at the second address.
      await writeFile(
        join(directory, `localhost_${second.port}.pem`),
        rootCertificates[0],
      );
      const { NODE_EXTRA_CA_CERTS: _, ...inherited } = process.env;
      const { stdout } = await promisify(execFile)(
        process.execPath,
        [
          "--experimental-transform-types",
          "--no-warnings",
          "--input-type=module",
          "-e",
          repinned,
          first.href,
          second.href,
          directory,
          fingerprint,
        ],
        { env: inherited },
      );
      assert.deepEqual(JSON.parse(stdout.trim().split("\n").at(-1) ?? "{}"), {
        reason: "CertificateChanged",
      });
    },
    { development: true },
  );
});

/** A service that trusts only what it accepted, then answers one call. */
const acceptingService = `
import { Result } from "@oatscenter/result";
import { TrellisService } from ${JSON.stringify(service)};
import { participants } from ${JSON.stringify(fixture)};
const [trellisUrl, seed] = process.argv.slice(1);
const service = await TrellisService.connect({
  trellisUrl,
  participant: participants.Provider.participant,
  seed,
}).orThrow();
await service.handleEcho(async ({ input }) => {
  setTimeout(() => void service.stop(), 100);
  return Result.ok(input);
});
console.log("READY");
`;

for (const websocketOnly of [false, true])
  test(`a service connects to a development installation it accepted by fingerprint${websocketOnly ? ", over the WebSocket" : ""}`, async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const fingerprint = new X509Certificate(
          await readFile(runtime.caCertificate),
        ).fingerprint256;
        const identity = await runtime.registerService({
          name: "accepting-provider",
          contract: participants.Provider.participant,
        });
        const { NODE_EXTRA_CA_CERTS: _, ...inherited } = process.env;
        const child = spawn(
          process.execPath,
          [
            "--experimental-transform-types",
            "--no-warnings",
            "--input-type=module",
            "-e",
            acceptingService,
            runtime.trellisUrl,
            identity.seed,
          ],
          {
            cwd: join(import.meta.dirname, "../.."),
            stdio: ["ignore", "pipe", "inherit"],
            env: {
              ...inherited,
              TRELLIS_CERTIFICATE_FINGERPRINT: fingerprint,
              XDG_STATE_HOME: await mkdtemp(join(tmpdir(), "trellis-state-")),
            },
          },
        );
        let output = "";
        child.stdout.on(
          "data",
          (chunk: Buffer) => (output += chunk.toString()),
        );
        const exited = new Promise<number | null>((resolve) =>
          child.once("exit", (code) => resolve(code)),
        );
        try {
          await runtime.waitFor(
            () =>
              output.includes("READY") || child.exitCode !== null || undefined,
            { timeoutMs: 60_000 },
          );
          assert(output.includes("READY"), "the service didn't connect");
          const caller = await runtime.connectClient({
            name: "accepting-caller",
            contract: apps.Caller.request,
          });
          try {
            assert.deepEqual(
              await caller.echo({ value: "over TLS" }).orThrow(),
              {
                value: "over TLS",
              },
            );
          } finally {
            await caller.connection.close();
          }
          assert.equal(await exited, 0);
        } finally {
          child.kill();
        }
      },
      { development: true, websocketOnly },
    );
  });

const device = pathToFileURL(
  join(import.meta.dirname, "../packages/trellis/device.ts"),
).href;
const auth = pathToFileURL(
  join(import.meta.dirname, "../packages/trellis/auth/mod.ts"),
).href;

/**
 * A device that trusts only what it accepted: activated and waiting for
 * approval, or, in a later start, connecting with what it discovered then.
 */
const acceptingDevice = `
import { checkDeviceActivation, TrellisDevice } from ${JSON.stringify(device)};
import { deriveDeviceIdentity } from ${JSON.stringify(auth)};
import { discoverNativeOAuth } from ${JSON.stringify(native)};
import { participants } from ${JSON.stringify(fixture)};
const [mode, trellisUrl, root, provisioningSecret, statePath, discovered] = process.argv.slice(1);
const participant = participants.StateDevice.participant;
const rootSecret = Buffer.from(root, "hex");
if (mode === "activate") {
  const activation = await checkDeviceActivation({ trellisUrl, participant, rootSecret, provisioningSecret, statePath });
  console.log(JSON.stringify({ activationUrl: activation.activationUrl, confirmationCode: activation.confirmationCode }));
  await activation.waitForOnlineApproval();
  const server = (await discoverNativeOAuth(trellisUrl)).orThrow();
  console.log(JSON.stringify({
    servers: [...server.natsServers],
    trellisInstanceId: server.trust.instanceId,
    audienceNatsAccount: server.trust.account,
    issuers: [server.trust.issuer],
 natsSentinel: server.natsSentinel,
  }));
} else {
  const identity = await deriveDeviceIdentity(rootSecret);
  const connected = await TrellisDevice.connect({
    ...JSON.parse(discovered),
    seed: identity.identitySeedBase64url,
    participant,
  }).orThrow();
  console.log("CONNECTED");
  await connected.connection.close();
}
`;

for (const websocketOnly of [false, true])
  test(`a device on a development installation it accepted by fingerprint is approved, then connects${websocketOnly ? ", over the WebSocket" : ""}`, async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const fingerprint = new X509Certificate(
          await readFile(runtime.caCertificate),
        ).fingerprint256;
        const participant = participants.StateDevice.participant;
        const deployment = await runtime.contracts.apply({
          deployment: "accepting-device",
          contract: participant,
        });
        assert(deployment.deploymentId);
        const provisioned = await runtime.devices.provision({
          deploymentId: types.AuthDeploymentGetRequestCodec.decode({
            deploymentId: deployment.deploymentId,
          }).deploymentId,
          title: "Accepting device",
          idempotencyKey: ulid(),
        });
        const provisioningSecret = provisioned.provisioningSecret;
        assert(provisioningSecret);
        const root = Buffer.from(
          crypto.getRandomValues(new Uint8Array(32)),
        ).toString("hex");
        const { NODE_EXTRA_CA_CERTS: _, ...inherited } = process.env;
        const env = {
          ...inherited,
          TRELLIS_CERTIFICATE_FINGERPRINT: fingerprint,
          XDG_STATE_HOME: await mkdtemp(join(tmpdir(), "trellis-state-")),
        };
        const start = (mode: string, discovered = "") => {
          const child = spawn(
            process.execPath,
            [
              "--experimental-transform-types",
              "--no-warnings",
              "--input-type=module",
              "-e",
              acceptingDevice,
              mode,
              runtime.trellisUrl,
              root,
              provisioningSecret,
              join(runtime.workdir, "accepting-device.json"),
              discovered,
            ],
            {
              cwd: join(import.meta.dirname, "../.."),
              stdio: ["ignore", "pipe", "inherit"],
              env,
            },
          );
          const lines: string[] = [];
          let partial = "";
          child.stdout.on("data", (chunk: Buffer) => {
            const text = partial + chunk.toString();
            const complete = text.split("\n");
            partial = complete.pop() ?? "";
            lines.push(...complete);
          });
          const exited = new Promise<number | null>((resolve) =>
            child.once("exit", (code) => resolve(code)),
          );
          return { child, lines, exited };
        };

        const activating = start("activate");
        try {
          const pending = JSON.parse(
            await runtime.waitFor(
              () =>
                activating.lines[0] ??
                (activating.child.exitCode !== null ? "{}" : undefined),
              { timeoutMs: 60_000 },
            ),
          );
          assert(pending.activationUrl, "the device didn't ask for activation");
          const reviewId = types.AuthDeviceReviewGetRequestCodec.decode({
            reviewId: new URL(pending.activationUrl).searchParams.get(
              "reviewId",
            ),
            confirmationCode: pending.confirmationCode,
          }).reviewId;
          const { review } = await runtime.callAdminRpc(
            "authDevicesReviewsGet",
            {
              reviewId,
              confirmationCode: pending.confirmationCode,
            },
          );
          await runtime.callAdminRpc("authDevicesReviewsDecide", {
            reviewId,
            confirmationCode: pending.confirmationCode,
            expectedRevision: review.revision,
            decision: "approved",
            idempotencyKey: ulid(),
          });
          // The device sees the approval over the same accepted certificate.
          const code = await Promise.race([
            activating.exited,
            new Promise<"waiting">((resolve) =>
              setTimeout(() => resolve("waiting"), 30_000),
            ),
          ]);
          assert.equal(code, 0, "the device didn't see its approval");
        } finally {
          activating.child.kill();
        }

        // A later start connects with only what it remembered.
        const connecting = start("connect", activating.lines[1] ?? "{}");
        try {
          assert.equal(await connecting.exited, 0);
          assert.deepEqual(connecting.lines, ["CONNECTED"]);
        } finally {
          connecting.child.kill();
        }
      },
      { development: true, websocketOnly },
    );
  });

/**
 * A program that connects to one installation, then to another that names the
 * first's address as its own sign-in, then to the first again.
 */
const named = `
import { acceptServerCertificate, discoverNativeOAuth } from ${JSON.stringify(native)};
const [first, other, directory, firstFingerprint, otherFingerprint] = process.argv.slice(1);
const discover = async (url) => {
  const found = await discoverNativeOAuth(url);
  return found.isErr() ? found.error.reason : found.orThrow().issuer;
};
(await acceptServerCertificate(first, { directory, fingerprint: firstFingerprint, interactive: false })).orThrow();
const before = await discover(first);
(await acceptServerCertificate(other, { directory, fingerprint: otherFingerprint, interactive: false })).orThrow();
const claimed = await discover(other);
console.log(JSON.stringify({ before, claimed, after: await discover(first) }));
`;

test("an installation can't make another's address trust its certificate", async () => {
  await withTrellisRuntime(
    async (first) => {
      await withTrellisRuntime(
        async (other) => {
          // Another installation's certificate, answering at an address that
          // names the first installation as its sign-in.
          const tls = join(other.workdir, "config", "trellis", "tls");
          const claiming = createServer(
            {
              cert: await readFile(join(tls, "nats.crt")),
              key: await readFile(join(tls, "nats.key")),
            },
            (request, response) => {
              if (request.url === "/.well-known/oauth-protected-resource") {
                response.setHeader("Content-Type", "application/json");
                response.end(
                  JSON.stringify({
                    resource: claimingUrl,
                    authorization_servers: [first.trellisUrl],
                  }),
                );
              } else {
                response.statusCode = 404;
                response.end();
              }
            },
          );
          await new Promise<void>((resolve) =>
            claiming.listen(0, "127.0.0.1", resolve),
          );
          const address = claiming.address();
          assert(address && typeof address === "object");
          const claimingUrl = `https://127.0.0.1:${address.port}`;
          try {
            const fingerprintOf = async (runtime: { caCertificate: string }) =>
              new X509Certificate(await readFile(runtime.caCertificate))
                .fingerprint256;
            const { NODE_EXTRA_CA_CERTS: _, ...inherited } = process.env;
            const { stdout } = await promisify(execFile)(
              process.execPath,
              [
                "--experimental-transform-types",
                "--no-warnings",
                "--input-type=module",
                "-e",
                named,
                first.trellisUrl,
                claimingUrl,
                await mkdtemp(join(tmpdir(), "trellis-accepted-")),
                await fingerprintOf(first),
                await fingerprintOf(other),
              ],
              { env: inherited },
            );
            assert.deepEqual(
              JSON.parse(stdout.trim().split("\n").at(-1) ?? "{}"),
              {
                before: first.trellisUrl,
                claimed: "CertificateConflict",
                after: first.trellisUrl,
              },
            );
          } finally {
            claiming.close();
          }
        },
        { development: true },
      );
    },
    { development: true },
  );
});
