import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { X509Certificate } from "node:crypto";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { rootCertificates } from "node:tls";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

import {
  apps,
  participants,
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
