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
import { randomUUID, X509Certificate } from "node:crypto";
import { Readable } from "node:stream";
import { connect, type DetailedPeerCertificate } from "node:tls";
import { Agent, request, WebSocket as AgentWebSocket } from "undici";
import { err, ok, type Result } from "@oatscenter/result";

import type { HttpRequest } from "../auth/oauth_core.ts";
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
 * The issuers accepted for each endpoint (`host:port`), which only Trellis's
 * own connections to that endpoint trust. Nothing else in the process does.
 */
const acceptedIssuers = new Map<string, readonly string[]>();

const defaultPorts: Record<string, string> = {
  "https:": "443",
  "wss:": "443",
  "http:": "80",
  "ws:": "80",
  "tls:": "4222",
  "nats:": "4222",
};

function endpointOf(url: string | URL): string {
  const parsed = new URL(url);
  const host = parsed.hostname.replace(/^\[|\]$/g, "");
  return `${host}:${parsed.port || defaultPorts[parsed.protocol] || ""}`;
}

/**
 * Trusts the issuer accepted for `named`'s endpoint at `endpoints` too, which
 * that installation named itself, such as its sign-in and its NATS servers.
 * @internal
 */
export function trustNamedEndpoints(
  named: string,
  endpoints: readonly string[],
): void {
  const issuers = acceptedIssuers.get(endpointOf(named));
  if (issuers)
    for (const endpoint of endpoints)
      acceptedIssuers.set(endpointOf(endpoint), issuers);
}

/**
 * The issuers accepted for any of `servers`, for a connection to them.
 * @internal
 */
export function acceptedIssuersFor(
  servers: readonly string[],
): string[] | undefined {
  const issuers = new Set(
    servers.flatMap((server) => acceptedIssuers.get(endpointOf(server)) ?? []),
  );
  return issuers.size ? [...issuers] : undefined;
}

/** One connection pool per set of accepted issuers. */
const agents = new WeakMap<readonly string[], Agent>();

function agentFor(url: string | URL): Agent | undefined {
  const issuers = acceptedIssuers.get(endpointOf(url));
  if (!issuers) return undefined;
  let agent = agents.get(issuers);
  if (!agent) {
    agent = new Agent({ connect: { ca: [...issuers] } });
    agents.set(issuers, agent);
  }
  return agent;
}

/**
 * `fetch`, trusting the issuer accepted for `input`'s endpoint when there is
 * one.
 * @internal
 */
export async function installationFetch(
  input: string | URL,
  init: HttpRequest = {},
): Promise<Response> {
  const dispatcher = agentFor(input);
  if (!dispatcher) return await fetch(input, init);
  const headers = { ...init.headers };
  if (init.body instanceof URLSearchParams)
    headers["content-type"] ??=
      "application/x-www-form-urlencoded;charset=UTF-8";
  const received = await request(input, {
    dispatcher,
    method: init.method ?? "GET",
    headers,
    body: init.body?.toString(),
    signal: init.signal,
  });
  // Like fetch with `redirect: "error"`; undici follows none by default.
  if (
    received.statusCode >= 300 &&
    received.statusCode < 400 &&
    init.redirect === "error"
  ) {
    received.body.destroy();
    throw new TypeError("fetch failed: redirect");
  }
  const responseHeaders = new Headers();
  for (const [name, value] of Object.entries(received.headers))
    for (const item of [value ?? []].flat()) responseHeaders.append(name, item);
  const empty = [101, 204, 205, 304].includes(received.statusCode);
  if (empty) received.body.destroy();
  return new Response(
    empty
      ? null
      : (Readable.toWeb(received.body) as ReadableStream<Uint8Array>),
    { status: received.statusCode, headers: responseHeaders },
  );
}

/**
 * A WebSocket to `url`, trusting the issuer accepted for its endpoint when
 * there is one.
 * @internal
 */
export function installationWebSocket(url: string): WebSocket {
  const dispatcher = agentFor(url);
  // Node's own WebSocket is this class, from its bundled copy of undici.
  return dispatcher
    ? (new AgentWebSocket(url, { dispatcher }) as WebSocket)
    : new WebSocket(url);
}

/**
 * Makes Trellis's connections to the Trellis at `url` trust the certificate
 * it presents, accepting one nothing vouches for the way a browser would.
 *
 * A server the system trust store vouches for needs nothing, unless it fails
 * an explicitly requested fingerprint. Otherwise the certificate accepted
 * earlier for this server is trusted, and a different one is refused. On first
 * contact the issuer's SHA-256 fingerprint must match `acceptance.fingerprint`
 * (or `TRELLIS_CERTIFICATE_FINGERPRINT`), or `TRELLIS_ACCEPT_SELF_SIGNED` must
 * be `1`, or the operator accepts it on the terminal. The accepted issuer is
 * saved in `acceptance.directory`. Only this installation's endpoints trust
 * it, through `discoverNativeOAuth` and the connections Trellis opens to
 * them; the rest of the process, including `fetch`, does not.
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
  const expected = (
    acceptance.fingerprint ?? process.env[CERTIFICATE_FINGERPRINT_ENV]
  )?.trim();
  const normalize = (value: string) => value.replaceAll(":", "").toUpperCase();
  if (expected && normalize(expected) !== normalize(presented.fingerprint))
    return err(
      new AuthError({
        reason: "CertificateMismatch",
        message: `${server} presents issuer fingerprint ${presented.fingerprint}, not the expected ${expected}`,
      }),
    );
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
    acceptedIssuers.set(server, [accepted]);
    return ok(undefined);
  }

  if (expected) {
    // Matched above.
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
  // Unique to this attempt: another one in this process may be staging the
  // same certificate.
  const staged = `${file}.${randomUUID()}`;
  await writeFile(staged, presented.pem, { mode: 0o600, flag: "wx" });
  await rename(staged, file);
  acceptedIssuers.set(server, [presented.pem]);
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
 * Trusts the certificates `participant` already accepted at `servers`, for a
 * connection that only reaches the broker. Activation, which reaches the
 * server's HTTPS address, accepts them first.
 */
export async function trustAcceptedCertificatesFor(
  participant: string,
  servers: readonly string[],
): Promise<void> {
  const { directory } = certificateAcceptanceFor(
    participantProgram(participant),
  );
  const issuers: string[] = [];
  for (const name of await readdir(directory).catch(() => []))
    if (name.endsWith(".pem"))
      issuers.push(await readFile(join(directory, name), "utf8"));
  if (issuers.length)
    for (const server of servers)
      acceptedIssuers.set(endpointOf(server), issuers);
}

function participantProgram(participant: string): string {
  return participant.replace(/[^A-Za-z0-9._-]/g, "_");
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
