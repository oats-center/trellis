import { execFile } from "node:child_process";
import {
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  randomBytes,
  sign,
  verify,
  type KeyObject,
} from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { promisify } from "node:util";
import { z } from "zod";

/**
 * Matched-security HTTP material: a generated CA, mutual-TLS server and client
 * identities, and one Ed25519 request-signing key. TLS supplies channel
 * integrity and peer identity; the signature authorizes each request, as a
 * Trellis session request proof does. Trellis revocation, authority renewal and
 * per-frame proofs over a brokered path have no HTTP equivalent here.
 */
export type HttpTlsMaterial = {
  ca: Buffer;
  serverCert: Buffer;
  serverKey: Buffer;
  clientCert: Buffer;
  clientKey: Buffer;
  signingKey: KeyObject;
  verifyingKey: KeyObject;
};

const SigningJwk = z.object({ d: z.string(), x: z.string() });
const DOMAIN = "trellis-benchmark-http-request-v1";
const MAX_SKEW_MS = 60_000;

/**
 * Generate an ephemeral P-256 CA, server and client certificates for
 * 127.0.0.1, and one Ed25519 request-signing key into a new directory.
 */
export async function generateHttpTls(dir: string): Promise<void> {
  await mkdir(dir);
  const openssl = (...argv: string[]) =>
    promisify(execFile)("openssl", argv, { cwd: dir });
  const key = [
    "-newkey",
    "ec",
    "-pkeyopt",
    "ec_paramgen_curve:P-256",
    "-nodes",
  ];
  await openssl(
    "req",
    "-x509",
    ...key,
    "-keyout",
    "ca.key",
    "-out",
    "ca.pem",
    "-days",
    "2",
    "-subj",
    "/CN=trellis-benchmark-ca",
    "-addext",
    "basicConstraints=critical,CA:TRUE",
    "-addext",
    "keyUsage=critical,keyCertSign,cRLSign",
  );
  for (const [name, usage] of [
    ["server", "serverAuth"],
    ["client", "clientAuth"],
  ] as const) {
    await writeFile(
      `${dir}/${name}.ext`,
      `basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=${usage}\nsubjectAltName=IP:127.0.0.1\n`,
    );
    await openssl(
      "req",
      ...key,
      "-keyout",
      `${name}.key`,
      "-out",
      `${name}.csr`,
      "-subj",
      `/CN=trellis-benchmark-${name}`,
    );
    await openssl(
      "x509",
      "-req",
      "-in",
      `${name}.csr`,
      "-CA",
      "ca.pem",
      "-CAkey",
      "ca.key",
      "-CAcreateserial",
      "-out",
      `${name}.pem`,
      "-days",
      "2",
      "-extfile",
      `${name}.ext`,
    );
  }
  const signing = generateKeyPairSync("ed25519").privateKey.export({
    format: "jwk",
  });
  await writeFile(
    `${dir}/request-signing.json`,
    JSON.stringify({ d: signing.d, x: signing.x }),
  );
}

/** Read the coordinator-generated material from its directory. */
export async function loadHttpTls(dir: string): Promise<HttpTlsMaterial> {
  const jwk = SigningJwk.parse(
    JSON.parse(await readFile(`${dir}/request-signing.json`, "utf8")),
  );
  return {
    ca: await readFile(`${dir}/ca.pem`),
    serverCert: await readFile(`${dir}/server.pem`),
    serverKey: await readFile(`${dir}/server.key`),
    clientCert: await readFile(`${dir}/client.pem`),
    clientKey: await readFile(`${dir}/client.key`),
    signingKey: createPrivateKey({
      key: { kty: "OKP", crv: "Ed25519", d: jwk.d, x: jwk.x },
      format: "jwk",
    }),
    verifyingKey: createPublicKey({
      key: { kty: "OKP", crv: "Ed25519", x: jwk.x },
      format: "jwk",
    }),
  };
}

function canonical(
  method: string,
  target: string,
  length: string,
  timestamp: string,
  nonce: string,
): Buffer {
  return Buffer.from(
    [DOMAIN, method, target, length, timestamp, nonce].join("\n"),
  );
}

/** Headers authorizing one request; length is the declared body size or "-". */
export function signRequest(
  material: HttpTlsMaterial,
  method: string,
  target: string,
  length: number | undefined,
): Record<string, string> {
  const timestamp = String(Date.now());
  const nonce = randomBytes(16).toString("base64url");
  const declared = length === undefined ? "-" : String(length);
  return {
    "x-bench-timestamp": timestamp,
    "x-bench-nonce": nonce,
    "x-bench-signature": sign(
      null,
      canonical(method, target, declared, timestamp, nonce),
      material.signingKey,
    ).toString("base64url"),
  };
}

/** Server-side request verifier with single-use nonces inside the skew window. */
export function requestVerifier(material: HttpTlsMaterial) {
  const seen = new Map<string, number>();
  return (
    method: string,
    target: string,
    headers: Record<string, string | string[] | undefined>,
  ): string | undefined => {
    const one = (name: string) => {
      const value = headers[name];
      return typeof value === "string" ? value : undefined;
    };
    const timestamp = one("x-bench-timestamp");
    const nonce = one("x-bench-nonce");
    const signature = one("x-bench-signature");
    if (!timestamp || !nonce || !signature) return "missing request proof";
    const now = Date.now();
    if (Math.abs(now - Number(timestamp)) > MAX_SKEW_MS) {
      return "request proof outside skew window";
    }
    for (const [key, at] of seen)
      if (now - at > 2 * MAX_SKEW_MS) seen.delete(key);
    if (seen.has(nonce)) return "replayed request proof";
    const declared = one("content-length") ?? "-";
    if (
      !verify(
        null,
        canonical(method, target, declared, timestamp, nonce),
        material.verifyingKey,
        Buffer.from(signature, "base64url"),
      )
    ) {
      return "invalid request proof";
    }
    seen.set(nonce, now);
    return undefined;
  };
}
