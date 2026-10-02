import {
  classifyTransportAuthorizationWasm,
  TRANSPORT_AUTHORIZATION_FORMAT_V1,
  type TransportAuthorizationV1,
} from "../protocol_wasm.ts";

/**
 * Retained transport-authorization status for one logical connection.
 *
 * `unavailable` covers a real disconnect and a socket whose own authority no
 * longer covers its admitted policy; it never means "application authorization
 * is revoked", which is a separate observation.
 */
export type TransportAuthorizationStatus =
  | "unavailable"
  | "current"
  | "upgrade_available";

/**
 * Prefix the runtime sets on every callout-owned attachment's authenticated
 * name: `trellis.auth.v1:<contextDigest>:<serverId>:<clientId>`.
 */
const ADMISSION_NAME_PREFIX = "trellis.auth.v1:";

/**
 * Parse the signed-context digest out of an authenticated admission name.
 *
 * Returns `undefined` for any name that is not a well-formed Trellis admission
 * marker, so a foreign or malformed name is never mistaken for admitted
 * authority.
 * The complete marker requires nonempty opaque digest and server fields,
 * no whitespace, and a canonical positive decimal u64 client ID.
 */
export function admissionContextDigest(
  authenticatedUser: string,
): string | undefined {
  if (!authenticatedUser.startsWith(ADMISSION_NAME_PREFIX)) return undefined;
  const remainder = authenticatedUser.slice(ADMISSION_NAME_PREFIX.length);
  const fields = remainder.split(":");
  const [digest, serverId, clientId] = fields;
  if (
    fields.length !== 3 || !digest || !serverId ||
    /[\p{White_Space}\uFEFF]/u.test(remainder) ||
    !/^[1-9][0-9]*$/u.test(clientId) ||
    BigInt(clientId) > 18446744073709551615n
  ) return undefined;
  return digest;
}

/** Result of one bounded own-user-information read. */
export type OwnAdmission = {
  authenticatedUser: string;
  contextDigest?: string;
};

/** Minimal NATS request surface used for the own-user-information read. */
export type OwnUserInfoRequester = {
  request(
    subject: string,
    payload?: Uint8Array,
    opts?: { timeout?: number },
  ): Promise<{ json<T>(): T }>;
};

/**
 * Read this attachment's authenticated identity from the broker.
 *
 * Best effort by design: notification is triggered by a hint or a refresh, and
 * a missing or malformed reply must never install authority or lose
 * enforcement, so any failure yields `undefined` instead of throwing.
 */
export async function readOwnAdmission(
  nc: OwnUserInfoRequester,
  timeoutMs: number,
): Promise<OwnAdmission | undefined> {
  try {
    const reply = await nc.request("$SYS.REQ.USER.INFO", undefined, {
      timeout: timeoutMs,
    });
    const info = reply.json<{ data?: { user?: unknown } }>();
    const user = info?.data?.user;
    if (typeof user !== "string" || user.length === 0) return undefined;
    return {
      authenticatedUser: user,
      contextDigest: admissionContextDigest(user),
    };
  } catch {
    return undefined;
  }
}

/**
 * Retained admitted-transport state for one logical connection.
 *
 * Holds the exact signed policy actually admitted on the current physical
 * attachment (`A`) and recomputes the retained upgrade status against the
 * newest valid application policy (`D`). It installs no authority: a change
 * here only records a passive upgrade notice or marks transport unavailable.
 */
export class TransportAuthorizationState {
  #admittedDigest?: string;
  #admitted?: TransportAuthorizationV1;
  #allowed?: TransportAuthorizationV1;
  #status: TransportAuthorizationStatus = "unavailable";
  #onStatusChanged?: (status: TransportAuthorizationStatus) => void;

  /** Register the retained status observer; it receives semantic changes and the current value. */
  onStatusChanged(
    callback: (status: TransportAuthorizationStatus) => void,
  ): void {
    this.#onStatusChanged = callback;
    callback(this.#status);
  }

  status(): TransportAuthorizationStatus {
    return this.#status;
  }

  admittedDigest(): string | undefined {
    return this.#admittedDigest;
  }

  admittedPolicy(): TransportAuthorizationV1 | undefined {
    return this.#admitted;
  }

  /** Newest valid application policy D used for the last comparison. */
  allowedPolicy(): TransportAuthorizationV1 | undefined {
    return this.#allowed;
  }

  /**
   * Record the policy actually admitted on the current physical generation and
   * recompute the status against the newest valid application policy.
   *
   * Called only after an actual admission read, never as a side effect of an
   * ordinary context promotion, so an in-place renewal does not pretend the
   * socket adopted the newer policy.
   */
  async recordAdmission(args: {
    contextDigest: string;
    policy: TransportAuthorizationV1;
    allowed?: TransportAuthorizationV1;
    nowUnixSeconds: number;
  }): Promise<void> {
    this.#admittedDigest = args.contextDigest;
    this.#admitted = args.policy;
    await this.recompute(args.allowed, args.nowUnixSeconds);
  }

  /**
   * Recompute against the newest valid application policy without a new read.
   *
   * A duplicate renewal that changes only the context digest does not change
   * the semantic status, so no repeated transition is published.
   */
  async recompute(
    allowed: TransportAuthorizationV1 | undefined,
    nowUnixSeconds: number,
  ): Promise<void> {
    this.#allowed = allowed;
    const admitted = this.#admitted;
    if (!admitted) {
      this.#setStatus("unavailable");
      return;
    }
    if (!allowed) {
      this.#setStatus("current");
      return;
    }
    const relation = await classifyTransportAuthorizationWasm(
      admitted,
      allowed,
      nowUnixSeconds,
    );
    this.#setStatus(
      relation === "current"
        ? "current"
        : relation === "upgrade_available"
        ? "upgrade_available"
        : "unavailable",
    );
  }

  /** Record a real physical loss; the next admission recomputes from scratch. */
  markDisconnected(): void {
    this.#admittedDigest = undefined;
    this.#admitted = undefined;
    this.#setStatus("unavailable");
  }

  #setStatus(next: TransportAuthorizationStatus): void {
    if (next === this.#status) return;
    this.#status = next;
    this.#onStatusChanged?.(next);
  }
}

/**
 * Whether the admitted policy covers the exact subjects an operation needs.
 *
 * The need is checked as subject-union containment against A using the same
 * shared full-witness implementation as the runtime, so `a.>` covers `a.b` and
 * `a.*` does not cover `a.b.c`. Literal string-set subtraction is never used.
 * The response allowance and hard deadline are copied from A so this answers a
 * pure subject-coverage question.
 */
export async function admittedPolicyCovers(
  admitted: TransportAuthorizationV1,
  required: { publish?: readonly string[]; subscribe?: readonly string[] },
  nowUnixSeconds: number,
): Promise<boolean> {
  const need: TransportAuthorizationV1 = {
    format: TRANSPORT_AUTHORIZATION_FORMAT_V1,
    account: admitted.account,
    publishAllow: canonicalPatterns(required.publish),
    subscribeAllow: canonicalPatterns(required.subscribe),
    response: admitted.response,
    hardExpiresAt: admitted.hardExpiresAt,
  };
  const relation = await classifyTransportAuthorizationWasm(
    need,
    admitted,
    nowUnixSeconds,
  );
  return relation !== "reduction_required";
}

/** Canonicalize a subject-pattern list to the sorted, unique policy form. */
function canonicalPatterns(patterns: readonly string[] | undefined): string[] {
  return [...new Set(patterns ?? [])].sort();
}
