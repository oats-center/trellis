import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { X509Certificate } from "node:crypto";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

import { withTrellisRuntime } from "./_support/runtime.ts";

const native = pathToFileURL(
  join(import.meta.dirname, "../packages/trellis/native.ts"),
).href;

/** A program that trusts nothing beyond the system store: it accepts, then discovers. */
const program = `
import { acceptServerCertificate, discoverNativeOAuth } from ${JSON.stringify(native)};
const [issuer, directory] = process.argv.slice(1);
const accepted = await acceptServerCertificate(issuer, { directory, interactive: false });
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
        address = runtime.oauthIssuer,
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
      assert.equal(accepted.issuer, runtime.oauthIssuer);
      // Remembered: the next start needs nothing.
      assert.equal((await run(directory)).issuer, runtime.oauthIssuer);

      const fresh = await mkdtemp(join(tmpdir(), "trellis-accepted-"));
      // The Console's address, as people type it, leads to its sign-in.
      const selfSigned = await run(
        fresh,
        { TRELLIS_ACCEPT_SELF_SIGNED: "1" },
        runtime.trellisUrl,
      );
      assert.equal(selfSigned.issuer, runtime.oauthIssuer);
    },
    { development: true },
  );
});
