import {
  classifyTransportAuthorizationWasm,
  TRANSPORT_AUTHORIZATION_FORMAT_V1,
  type TransportAuthorizationV1,
} from "../protocol_wasm.ts";
import { TransportError } from "../../errors/TransportError.ts";

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
 */
export function admissionContextDigest(
  authenticatedUser: string,
): string | undefined {
  if (!authenticatedUser.startsWith(ADMISSION_NAME_PREFIX)) return undefined;
  const remainder = authenticatedUser.slice(ADMISSION_NAME_PREFIX.length);
  const separator = remainder.indexOf(":");
  if (separator <= 0) return undefined;
  return remainder.slice(0, separator);
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

/**
 * Distinct runtime failure for a granted capability whose broker subjects are
 * not yet admitted on the current attachment.
 *
 * This is not `permission_denied`: the application grant exists, only the
 * physical attachment has not adopted it yet. It never triggers a reconnect.
 */
export function transportUpgradeRequiredError(
  context: Record<string, unknown>,
): TransportError {
  return new TransportError({
    code: "transport_upgrade_required",
    message:
      "This operation requires transport authority not yet admitted on the current connection.",
    hint:
      "Adopt the granted capability with connection.refreshTransport(), then retry.",
    context,
  });
}

/** Canonicalize a subject-pattern list to the sorted, unique policy form. */
function canonicalPatterns(patterns: readonly string[] | undefined): string[] {
  return [...new Set(patterns ?? [])].sort();
}

/**
 * Read-only admitted-transport view used by request boundaries.
 *
 * Exposes A, the newest valid application policy D, and the retained status so
 * a boundary can tell "the grant exists but the attachment has not adopted it"
 * apart from a genuine permission failure. @internal
 */
export type TransportAuthorizationGate = {
  status(): TransportAuthorizationStatus;
  admittedPolicy(): TransportAuthorizationV1 | undefined;
  allowedPolicy(): TransportAuthorizationV1 | undefined;
  nowSeconds(): number;
};

/**
 * Whether `required` is a granted capability the current attachment has not
 * adopted yet.
 *
 * Returns false when transport authority is unavailable or the requirement is
 * not granted at all, so the caller falls through to the ordinary server
 * decision rather than masking a real denial.
 */
export async function requiresTransportUpgrade(
  gate: TransportAuthorizationGate,
  required: { publish?: readonly string[]; subscribe?: readonly string[] },
): Promise<boolean> {
  if (gate.status() !== "upgrade_available") return false;
  const admitted = gate.admittedPolicy();
  const allowed = gate.allowedPolicy();
  if (!admitted || !allowed) return false;
  const now = gate.nowSeconds();
  if (await admittedPolicyCovers(admitted, required, now)) return false;
  return await admittedPolicyCovers(allowed, required, now);
}

/**
 * Bounded transport-admission check for one resource operation.
 *
 * Returns the transport failure when the operation is granted by the newest
 * application policy but absent from the admitted attachment, and `undefined`
 * when the operation may proceed (or is not a transport problem). @internal
 */
export type ResourceTransportCheck = (
  action: "read" | "write",
) => Promise<TransportError | undefined>;

/**
 * Build the transport check for one KV or Store bucket.
 *
 * Reads and writes are distinguished by the same subjects the runtime compiler
 * grants per resource action: KV reads by the direct-get grant, KV writes by the
 * bucket subject grant; Store reads by the stream-info grant, Store writes by
 * the object put/subject grant. The mapping is pinned by the shared
 * resource-grant fixture so it cannot drift from the compiler silently.
 * @internal
 */
export function resourceTransportCheck(
  gate: TransportAuthorizationGate,
  kind: "kv" | "store",
  bucket: string,
): ResourceTransportCheck {
  const subjects = kind === "kv"
    ? {
      read: [`$JS.API.DIRECT.GET.KV_${bucket}`],
      write: [`$KV.${bucket}.>`],
    }
    : {
      read: [`$JS.API.STREAM.INFO.OBJ_${bucket}`],
      write: [`$O.${bucket}.C.>`],
    };
  return async (action) => {
    const blocked = await requiresTransportUpgrade(gate, {
      publish: subjects[action],
    });
    return blocked
      ? transportUpgradeRequiredError({ resource: bucket, kind, action })
      : undefined;
  };
}
