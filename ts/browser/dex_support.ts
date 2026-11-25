// A real Dex for OIDC sign-in tests, served from a private CA nothing on
// the machine trusts, the way an OIDC provider often is.

import { execFileSync, spawn } from "node:child_process";
import { X509Certificate } from "node:crypto";
import { once } from "node:events";
import { mkdir, open, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { ulid } from "ulid";

import { reserveLocalPort } from "../packages/trellis-testkit/src/ports.ts";
import { trustInstallationCa } from "../packages/trellis-testkit/src/tls_trust.ts";
import { waitFor } from "../packages/trellis-testkit/src/wait.ts";

/** Dex's static-password example hash, for the password "password". */
const PASSWORD_HASH =
  "$2a$10$2b2cU8CPhOTaGrs1HRQuAueS7JTT5ZHsHSzYiFPm1leZck7Mc8T4W";

/** A running Dex. Stopping it is the caller's job. */
export type TestDex = {
  /** The issuer URL to register as the provider's address. */
  readonly issuer: string;
  readonly clientId: string;
  readonly clientSecret: string;
  /** SHA-256 fingerprint of the private CA that issued Dex's certificate. */
  readonly fingerprint: string;
  /** The `sub` Dex gives each email's account, by email. */
  readonly subjects: ReadonlyMap<string, string>;
  /** Dex's log, for failure messages. */
  log(): Promise<string>;
  stop(): Promise<void>;
};

/**
 * Starts the producer-built Dex (`TRELLIS_TEST_DEX_BIN`) with a static client
 * that redirects to `redirectUri` (one address, or several when more than one
 * Trellis signs in through the same client) and a password user per email,
 * each signing in with the password "password". Dex serves a leaf certificate followed by
 * its generated CA. The test process and test browsers trust that CA, so they
 * can reach Dex; Trellis does not, unless a provider pins it.
 */
export async function startDex(options: {
  directory: string;
  redirectUri: string | readonly string[];
  emails: readonly string[];
}): Promise<TestDex> {
  const binary = process.env.TRELLIS_TEST_DEX_BIN;
  if (!binary)
    throw new Error(
      "TRELLIS_TEST_DEX_BIN must point to the pinned Dex binary; run the browser suite through scripts/run-browser-tests.sh",
    );
  const directory = join(options.directory, `dex-${ulid()}`);
  await mkdir(directory, { recursive: true });
  const file = (name: string) => join(directory, name);
  const openssl = (...args: string[]) =>
    execFileSync("openssl", args, { stdio: "pipe" });
  const curve = ["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256"];
  openssl(
    "req",
    "-x509",
    ...curve,
    "-nodes",
    "-days",
    "1",
    "-subj",
    "/CN=Company identity test CA",
    "-addext",
    "basicConstraints=critical,CA:TRUE",
    "-addext",
    "keyUsage=critical,keyCertSign,cRLSign",
    "-keyout",
    file("ca-key.pem"),
    "-out",
    file("ca.pem"),
  );
  openssl(
    "req",
    "-new",
    ...curve,
    "-nodes",
    "-subj",
    "/CN=Company identity test",
    "-keyout",
    file("key.pem"),
    "-out",
    file("leaf.csr"),
  );
  await writeFile(
    file("leaf.cnf"),
    "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n" +
      "extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n",
  );
  openssl(
    "x509",
    "-req",
    "-in",
    file("leaf.csr"),
    "-CA",
    file("ca.pem"),
    "-CAkey",
    file("ca-key.pem"),
    "-CAcreateserial",
    "-days",
    "1",
    "-extfile",
    file("leaf.cnf"),
    "-out",
    file("leaf.pem"),
  );
  const ca = await readFile(file("ca.pem"), "utf8");
  await writeFile(
    file("chain.pem"),
    `${await readFile(file("leaf.pem"), "utf8")}${ca}`,
  );

  const reservation = await reserveLocalPort();
  const issuer = `https://127.0.0.1:${reservation.port}/dex`;
  const clientId = "trellis-company-sign-in";
  const clientSecret = `dex-secret-${ulid()}`;
  const userIds = new Map(options.emails.map((email) => [email, ulid()]));
  const users = [...userIds]
    .map(
      ([email, userId]) =>
        `- email: ${email}\n  username: ${email.split("@")[0]}\n` +
        `  userID: ${userId}\n  hash: '${PASSWORD_HASH}'\n`,
    )
    .join("");
  await writeFile(
    file("dex.yaml"),
    `issuer: ${issuer}
storage:
  type: memory
web:
  https: 127.0.0.1:${reservation.port}
  tlsCert: ${file("chain.pem")}
  tlsKey: ${file("key.pem")}
logger:
  level: error
oauth2:
  skipApprovalScreen: true
staticClients:
- id: ${clientId}
  name: Trellis
  secret: ${clientSecret}
  redirectURIs: [${[options.redirectUri].flat().join(", ")}]
enablePasswordDB: true
staticPasswords:
${users}`,
  );
  const logFile = file("dex.log");
  const log = () => readFile(logFile, "utf8").catch(() => "");
  const output = await open(logFile, "w");
  await reservation.releaseForSpawn();
  const dex = spawn(binary, ["serve", file("dex.yaml")], {
    stdio: ["ignore", output.fd, output.fd],
  });
  const exited = once(dex, "exit");
  const stop = async () => {
    await reservation.release();
    if (dex.exitCode === null && dex.signalCode === null) {
      dex.kill("SIGTERM");
      await exited;
    }
    await output.close();
  };
  try {
    trustInstallationCa(file("ca.pem"));
    await waitFor(
      async () => {
        if (dex.exitCode !== null)
          throw new Error(
            `Dex exited before it served discovery: ${await log()}`,
          );
        return await fetch(`${issuer}/.well-known/openid-configuration`)
          .then((response) => response.ok)
          .catch(() => false);
      },
      { timeoutMs: 15_000, intervalMs: 50 },
    );
    await reservation.release();
  } catch (cause) {
    await stop();
    throw new Error(`${String(cause)}\n${await log()}`, { cause });
  }
  return {
    issuer,
    clientId,
    clientSecret,
    fingerprint: new X509Certificate(ca).fingerprint256,
    subjects: new Map(
      [...userIds].map(([email, userId]) => [email, dexSubject(userId)]),
    ),
    log,
    stop,
  };
}

/**
 * The `sub` Dex issues a static-password user: a protobuf message of its user
 * ID and the `local` connector ID, base64url-encoded without padding. Both IDs
 * here are ASCII and shorter than 128 bytes, so each field is a tag byte, a
 * one-byte length, and the bytes.
 */
function dexSubject(userId: string): string {
  const field = (tag: number, value: string) => {
    const bytes = Buffer.from(value, "utf8");
    return Buffer.concat([Buffer.from([tag, bytes.length]), bytes]);
  };
  return Buffer.concat([field(0x0a, userId), field(0x12, "local")]).toString(
    "base64url",
  );
}
