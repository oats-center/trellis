/**
 * Self-enrollment of service instances with an enrollment secret
 * (design/auth/instance-enrollment.md). A process makes its own identity key,
 * registers it once over HTTPS, and then connects through ordinary admission.
 */
import { chmod, mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { hostname } from "node:os";
import { dirname } from "node:path";
import { ulid } from "ulid";
import { createAuth } from "../../auth/session_auth.ts";
import { decodeTrellisHttpError } from "../../auth/http_error.ts";
import { base64urlDecode, base64urlEncode, sha256 } from "../../auth/utils.ts";
import type { LoggerLike } from "../../globals.ts";

/** Environment variable the SDK reads an enrollment secret from when no seed or secret is given. */
export const ENROLLMENT_SECRET_ENV = "TRELLIS_ENROLLMENT_SECRET";

/** How a service with no provisioned seed gets its identity. */
export type TrellisServiceEnrollmentOpts = {
  /** The deployment's enrollment secret; otherwise `TRELLIS_ENROLLMENT_SECRET`. */
  secret?: string;
  /**
   * Keep the identity key in this file, so the process keeps its instance
   * across restarts. Without it the key lives in memory only: every start
   * enrolls a new instance, which retires itself at a graceful stop.
   */
  keyFile?: string;
  /** Title of the new instance; the host name by default. */
  title?: string;
};

/** What Auth reports about an identity key. */
export type ServiceIdentityState =
  "active" | "disabled" | "removed" | "retired" | "unregistered";

/** The instance an enrollment created, or recovered on a retry. */
export type EnrolledInstance = {
  instanceId: string;
  deploymentId: string;
  title: string;
};

type Target = { trellisUrl: string; origin: string; signal?: AbortSignal };

const TRANSIENT_STATUS = new Set([500, 502, 503, 504]);

function pause(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(signal.reason);
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        reject(signal.reason);
      },
      { once: true },
    );
  });
}

/** Make a new random identity seed. */
export function newIdentitySeed(): string {
  return base64urlEncode(crypto.getRandomValues(new Uint8Array(32)));
}

/** The host name an enrolled instance is titled with by default. */
export function defaultInstanceTitle(): string {
  return process.env["HOSTNAME"]?.trim() || hostname() || "service";
}

/**
 * Send one request signed by `seed`'s key, retrying only failures that say
 * nothing about the request: network errors and unavailable responses. The
 * request ID stays the same across retries, so a lost response is recovered.
 */
async function signedPost(
  target: Target,
  path: "/auth/service/enroll" | "/auth/service/identity",
  seed: string,
  fields: Record<string, unknown>,
): Promise<unknown> {
  const identity = await createAuth({ sessionKeySeed: seed });
  const requestId = ulid();
  const identityKeyId = base64urlEncode(
    await sha256(base64urlDecode(identity.sessionKey)),
  );
  let delay = 250;
  for (let attempt = 1; ; attempt += 1) {
    const unsignedRequest = {
      ...fields,
      requestId,
      iat: Date.now(),
      identityKeyId,
      identityPublicKey: identity.sessionKey,
    };
    const proof = await identity.signSessionProof({
      purpose:
        path === "/auth/service/enroll"
          ? "serviceEnrollment"
          : "serviceIdentityStatus",
      origin: target.origin,
      unsignedRequest,
    });
    let response: Response;
    try {
      response = await fetch(new URL(path, target.trellisUrl), {
        method: "POST",
        signal: target.signal,
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ ...unsignedRequest, proof }),
        // The body carries the signed request and, for enrollment, the
        // secret: it goes to Trellis or nowhere.
        redirect: "manual",
      });
    } catch (error) {
      if (target.signal?.aborted || attempt >= 8) throw error;
      await pause(delay, target.signal);
      delay = Math.min(delay * 2, 5_000);
      continue;
    }
    if (response.ok) return await response.json();
    if (
      response.type === "opaqueredirect" ||
      (response.status >= 300 && response.status < 400)
    ) {
      await response.body?.cancel();
      throw new Error(
        `Trellis answered ${path} with a ${response.status} redirect to ${response.headers.get("location") ?? "an unnamed location"}; enrollment requests don't follow redirects`,
      );
    }
    if (TRANSIENT_STATUS.has(response.status) && attempt < 8) {
      await response.body?.cancel();
      await pause(delay, target.signal);
      delay = Math.min(delay * 2, 5_000);
      continue;
    }
    throw await decodeTrellisHttpError(response);
  }
}

/**
 * Register `seed`'s key as a new instance of the deployment `secret` belongs
 * to. Repeating it for the same key returns the same instance.
 */
export async function enrollServiceIdentity(
  target: Target & {
    secret: string;
    seed: string;
    participantId: string;
    participantDigest: string;
    title: string;
  },
): Promise<EnrolledInstance> {
  const body = await signedPost(target, "/auth/service/enroll", target.seed, {
    enrollmentSecret: target.secret,
    participantId: target.participantId,
    participantDigest: target.participantDigest,
    title: target.title,
  });
  const enrolled = body as Partial<EnrolledInstance>;
  if (
    typeof enrolled.instanceId !== "string" ||
    typeof enrolled.deploymentId !== "string" ||
    typeof enrolled.title !== "string"
  )
    throw new Error("Invalid service enrollment response");
  return enrolled as EnrolledInstance;
}

/** Ask Auth what became of `seed`'s identity key. */
export async function serviceIdentityStatus(
  target: Target & { seed: string },
): Promise<ServiceIdentityState> {
  const body = (await signedPost(
    target,
    "/auth/service/identity",
    target.seed,
    {},
  )) as { state?: unknown };
  switch (body.state) {
    case "active":
    case "disabled":
    case "removed":
    case "retired":
    case "unregistered":
      return body.state;
    default:
      throw new Error("Invalid service identity status response");
  }
}

/** Read a key file's seed, or `undefined` when there is no file yet. */
export async function readKeyFile(path: string): Promise<string | undefined> {
  let text: string;
  try {
    text = await readFile(path, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
  const seed = text.trim();
  if (base64urlDecode(seed).length !== 32)
    throw new Error(`${path} does not hold a Trellis identity seed`);
  return seed;
}

/**
 * Replace the key file with `seed`, readable by its owner only. It is written
 * before the key is enrolled, so a crash between the two never loses a key
 * Auth already registered.
 */
export async function writeKeyFile(path: string, seed: string): Promise<void> {
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });
  const temporary = `${path}.${process.pid}.tmp`;
  await writeFile(temporary, `${seed}\n`, { mode: 0o600, flag: "w" });
  await chmod(temporary, 0o600);
  await rename(temporary, path);
}

/**
 * Connect with the identity the caller gave or the one enrollment yields.
 *
 * - With `seed`, it connects with that provisioned identity.
 * - Without `keyFile`, it makes a key in memory, enrolls it, and connects;
 *   `retireOnStop` is then true.
 * - With `keyFile`, it connects with the stored key first and retries
 *   transient failures. Only when Auth reports that key retired or
 *   unregistered does it make a new key, store it, and enroll. A key whose
 *   instance an administrator removed stops the process with an error.
 * @internal
 */
export async function connectWithServiceIdentity<T>(args: {
  seed: string | undefined;
  enrollment: TrellisServiceEnrollmentOpts | undefined;
  trellisUrl: string;
  origin: string;
  signal: AbortSignal | undefined;
  participantId: string;
  participantDigest: string;
  log: LoggerLike;
  connect(seed: string): Promise<T>;
}): Promise<{
  connected: T;
  seed: string;
  retireOnStop: boolean;
  enrolled: boolean;
}> {
  if (args.seed !== undefined) {
    if (args.enrollment !== undefined)
      throw new Error("Give either a seed or an enrollment secret, not both");
    return {
      connected: await args.connect(args.seed),
      seed: args.seed,
      retireOnStop: false,
      enrolled: false,
    };
  }
  const secret =
    args.enrollment?.secret ??
    (process.env[ENROLLMENT_SECRET_ENV]?.trim() || undefined);
  const keyFile = args.enrollment?.keyFile;
  const target = {
    trellisUrl: args.trellisUrl,
    origin: args.origin,
    signal: args.signal,
  };
  const enroll = async (): Promise<string> => {
    if (secret === undefined)
      throw new Error(
        keyFile === undefined
          ? `A service needs a seed or an enrollment secret (${ENROLLMENT_SECRET_ENV})`
          : `The key in ${keyFile} can no longer connect, and no enrollment secret (${ENROLLMENT_SECRET_ENV}) was given to enroll a new one`,
      );
    const seed = newIdentitySeed();
    if (keyFile !== undefined) await writeKeyFile(keyFile, seed);
    const instance = await enrollServiceIdentity({
      ...target,
      secret,
      seed,
      participantId: args.participantId,
      participantDigest: args.participantDigest,
      title: args.enrollment?.title ?? defaultInstanceTitle(),
    });
    args.log.info(instance, "Enrolled as a new instance");
    return seed;
  };
  if (keyFile === undefined) {
    const seed = await enroll();
    return {
      connected: await args.connect(seed),
      seed,
      retireOnStop: true,
      enrolled: true,
    };
  }
  let seed = await readKeyFile(keyFile);
  if (seed === undefined) {
    seed = await enroll();
    return {
      connected: await args.connect(seed),
      seed,
      retireOnStop: false,
      enrolled: true,
    };
  }
  let delay = 1_000;
  for (;;) {
    try {
      return {
        connected: await args.connect(seed),
        seed,
        retireOnStop: false,
        enrolled: true,
      };
    } catch (error) {
      if (args.signal?.aborted) throw error;
      let state: ServiceIdentityState | undefined;
      try {
        state = await serviceIdentityStatus({ ...target, seed });
      } catch {
        // Trellis is unreachable: try the same key again.
      }
      if (state === "removed")
        throw new Error(
          `An administrator removed the instance whose key is in ${keyFile}; this process will not enroll again on its own`,
        );
      if (state === "retired" || state === "unregistered") {
        args.log.info(
          { keyFile, state },
          "The stored key cannot connect; enrolling a new instance",
        );
        seed = await enroll();
        return {
          connected: await args.connect(seed),
          seed,
          retireOnStop: false,
          enrolled: true,
        };
      }
      args.log.warn(
        { keyFile, state, error },
        "Could not connect with the stored key; retrying",
      );
      await pause(delay, args.signal);
      delay = Math.min(delay * 2, 30_000);
    }
  }
}
