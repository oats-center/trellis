import {
  chmod,
  mkdir,
  readdir,
  readFile,
  rename,
  writeFile,
} from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import { createInterface } from "node:readline/promises";
import { X509Certificate } from "node:crypto";
import {
  connect,
  getCACertificates,
  setDefaultCACertificates,
  type DetailedPeerCertificate,
} from "node:tls";
import { err, ok, type Result } from "@oatscenter/result";

import { AuthError } from "../errors/AuthError.ts";

/** Accepts only a certificate whose issuer has this SHA-256 fingerprint. */
export const CERTIFICATE_FINGERPRINT_ENV = "TRELLIS_CERTIFICATE_FINGERPRINT";
/** When `1`, accepts whatever certificate the server presents on first contact. */
export const ACCEPT_SELF_SIGNED_ENV = "TRELLIS_ACCEPT_SELF_SIGNED";

/** How {@link acceptServerCertificate} may accept an unvouched certificate. */
export interface CertificateAcceptance {
  /** This program's folder for accepted certificates, one file per server. */
  readonly directory: string;
  /** Accept only this issuer fingerprint; otherwise `TRELLIS_CERTIFICATE_FINGERPRINT`. */
  readonly fingerprint?: string;
  /** Ask on the terminal when nothing else decides. Defaults to true. */
  readonly interactive?: boolean;
}

/**
 * Remembers accepted certificates for `program` under the user's state folder
 * (`$XDG_STATE_HOME/trellis/<program>/servers`).
 */
export function certificateAcceptanceFor(
  program: string,
): CertificateAcceptance {
  const configured = process.env["XDG_STATE_HOME"];
  const state =
    configured && isAbsolute(configured)
      ? configured
      : join(homedir(), ".local", "state");
  return { directory: join(state, "trellis", program, "servers") };
}

/**
 * Makes this process trust the certificate a Trellis at `url` presents,
 * accepting one nothing vouches for the way a browser would.
 *
 * A server the system trust store vouches for needs nothing. Otherwise the
 * certificate accepted earlier for this server is trusted, and a different one
 * is refused. On first contact the issuer's SHA-256 fingerprint must match
 * `acceptance.fingerprint` (or `TRELLIS_CERTIFICATE_FINGERPRINT`), or
 * `TRELLIS_ACCEPT_SELF_SIGNED` must be `1`, or the operator accepts it on the
 * terminal. The accepted issuer is saved in `acceptance.directory` and added to
 * this process's TLS trust, which `fetch` and NATS connections use.
 */
export async function acceptServerCertificate(
  url: string,
  acceptance: CertificateAcceptance,
): Promise<Result<void, AuthError>> {
  let target: URL;
  try {
    target = new URL(url);
  } catch {
    return err(new AuthError({ reason: "InvalidIssuer" }));
  }
  if (target.protocol !== "https:") return ok(undefined);
  const host = target.hostname.replace(/^\[|\]$/g, "");
  const port = Number(target.port || 443);
  const server = `${host}:${port}`;
  const file = join(
    acceptance.directory,
    `${host.replaceAll(":", "_")}_${port}.pem`,
  );
  const presented = await presentedIssuer(host, port);
  if (!presented) return err(new AuthError({ reason: "OAuthUnavailable" }));
  if (presented.authorized) return ok(undefined);

  const accepted = await readFile(file, "utf8").catch(() => undefined);
  if (accepted !== undefined) {
    const fingerprint = new X509Certificate(accepted).fingerprint256;
    if (fingerprint !== presented.fingerprint)
      return err(
        new AuthError({
          reason: "CertificateChanged",
          message: `${server} presents a different certificate than the one accepted for it (accepted ${fingerprint}, presented ${presented.fingerprint}); someone may be intercepting the connection. If the operator replaced it, delete ${file} and connect again`,
        }),
      );
    trust(accepted);
    return ok(undefined);
  }

  const expected = (
    acceptance.fingerprint ?? process.env[CERTIFICATE_FINGERPRINT_ENV]
  )?.trim();
  const normalize = (value: string) => value.replaceAll(":", "").toUpperCase();
  if (expected) {
    if (normalize(expected) !== normalize(presented.fingerprint))
      return err(
        new AuthError({
          reason: "CertificateMismatch",
          message: `${server} presents issuer fingerprint ${presented.fingerprint}, not the expected ${expected}`,
        }),
      );
  } else if (process.env[ACCEPT_SELF_SIGNED_ENV]?.trim() === "1") {
    process.stderr.write(
      `Accepting ${server}'s certificate because ${ACCEPT_SELF_SIGNED_ENV}=1; its issuer's SHA-256 fingerprint is\n  ${presented.fingerprint}\n`,
    );
  } else if (
    acceptance.interactive !== false &&
    process.stdin.isTTY &&
    process.stderr.isTTY
  ) {
    const prompt = createInterface({
      input: process.stdin,
      output: process.stderr,
    });
    const answer = await prompt.question(
      `${server} presents a certificate nothing on this machine vouches for.\nConfirm its issuer's SHA-256 fingerprint with the operator:\n  ${presented.fingerprint}\nAccept it? [y/N] `,
    );
    prompt.close();
    if (!["y", "yes"].includes(answer.trim().toLowerCase()))
      return err(
        new AuthError({
          reason: "CertificateDeclined",
          message: `the certificate for ${server} was not accepted`,
        }),
      );
  } else {
    return err(
      new AuthError({
        reason: "UntrustedCertificate",
        message: `${server} presents a certificate nothing on this machine vouches for; its issuer's SHA-256 fingerprint is ${presented.fingerprint}. After confirming it with the operator, set ${CERTIFICATE_FINGERPRINT_ENV} to it, or set ${ACCEPT_SELF_SIGNED_ENV}=1 for development`,
      }),
    );
  }

  await mkdir(acceptance.directory, { recursive: true, mode: 0o700 });
  await chmod(acceptance.directory, 0o700);
  const staged = `${file}.${process.pid}`;
  await writeFile(staged, presented.pem, { mode: 0o600 });
  await rename(staged, file);
  trust(presented.pem);
  return ok(undefined);
}

/**
 * Accepts the server's certificate for the program `participant` names,
 * throwing when it isn't accepted. Services and devices call this before
 * their first request.
 */
export async function acceptServerCertificateFor(
  url: string,
  participant: string,
): Promise<void> {
  const accepted = await acceptServerCertificate(
    url,
    certificateAcceptanceFor(participantProgram(participant)),
  );
  if (accepted.isErr()) throw accepted.error;
}

/**
 * Trusts the certificates `participant` already accepted, for a connection
 * that only reaches the broker. Activation, which reaches the server's HTTPS
 * address, accepts them first.
 */
export async function trustAcceptedCertificatesFor(
  participant: string,
): Promise<void> {
  const { directory } = certificateAcceptanceFor(
    participantProgram(participant),
  );
  for (const name of await readdir(directory).catch(() => []))
    if (name.endsWith(".pem"))
      trust(await readFile(join(directory, name), "utf8"));
}

function participantProgram(participant: string): string {
  return participant.replace(/[^A-Za-z0-9._-]/g, "_");
}

/** Adds `pem` to the CAs this process's TLS connections trust. */
function trust(pem: string): void {
  const current = getCACertificates("default");
  if (!current.includes(pem)) setDefaultCACertificates([...current, pem]);
}

/**
 * The issuer that ends the chain the server presents, without trusting it,
 * and whether the system trust store already vouches for the server.
 */
async function presentedIssuer(
  host: string,
  port: number,
): Promise<
  { authorized: boolean; fingerprint: string; pem: string } | undefined
> {
  return await new Promise((resolve) => {
    const socket = connect({
      host,
      port,
      servername: /^[\d.:]+$/.test(host) ? undefined : host,
      rejectUnauthorized: false,
      timeout: 10_000,
    });
    const done = (
      value:
        { authorized: boolean; fingerprint: string; pem: string } | undefined,
    ) => {
      socket.destroy();
      resolve(value);
    };
    socket.once("error", () => done(undefined));
    socket.once("timeout", () => done(undefined));
    socket.once("secureConnect", () => {
      let certificate: DetailedPeerCertificate =
        socket.getPeerCertificate(true);
      while (
        certificate.issuerCertificate &&
        certificate.issuerCertificate !== certificate
      )
        certificate = certificate.issuerCertificate;
      if (!certificate.raw) return done(undefined);
      const issuer = new X509Certificate(certificate.raw);
      done({
        authorized: socket.authorized,
        fingerprint: issuer.fingerprint256,
        pem: issuer.toString(),
      });
    });
  });
}
