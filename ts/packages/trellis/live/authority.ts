// Retained live-authority guards for active observation sessions.
//
// A live session stays authorized for its entire lifetime without a network
// lookup per frame. This module wraps the existing provider cache's retained
// lease in a typed, fail-closed check. A valid identity-preserving refresh on
// the same connection generation replaces the retained lease without closing
// the session or resetting its sequence and credit state; revocation, lost
// coverage, expiry and epoch movement are observable through
// {@link LiveAuthorityGuard.subscribeChanges} and
// {@link LiveAuthorityGuard.checkNow}.

import type { PermissionAtom } from "../auth/protocol_wasm.ts";
import {
  type AuthorizationProviderCache,
  type LiveLeaseView,
} from "../auth/authorization/provider_cache.ts";
import { LiveEnd, LiveStreamError } from "./types.ts";

/** Pinned peer identity from a verified authorization context. */
export type PinnedPeerIdentity = {
  connectionId: string;
  sessionKey: string;
  principalId: string;
  participantId: string;
  deploymentId?: string;
  instanceId?: string;
};

/** Why a retained live authority is no longer usable. */
export type LiveAuthorityLost =
  | "transport_unavailable"
  | "epoch_changed"
  | "coverage_lost"
  | "revoked"
  | "coverage_unknown"
  | "expired"
  | "identity_changed"
  | "binding_changed"
  | "permission_lost";

export type LiveGuardRequirement =
  | { kind: "observer"; permission: PermissionAtom }
  | { kind: "local-provider" }
  | { kind: "peer-provider"; expected: PinnedPeerIdentity };

type VerifiedContext = LiveLeaseView["verified"]["context"];

function identityOf(context: VerifiedContext): PinnedPeerIdentity {
  return {
    connectionId: context.connectionId,
    sessionKey: context.sessionKey,
    principalId: context.principalId,
    participantId: context.participantId,
    ...(context.deploymentId ? { deploymentId: context.deploymentId } : {}),
    ...(context.instanceId ? { instanceId: context.instanceId } : {}),
  };
}

function sameIdentity(
  left: PinnedPeerIdentity,
  right: PinnedPeerIdentity,
): boolean {
  return left.connectionId === right.connectionId &&
    left.sessionKey === right.sessionKey &&
    left.principalId === right.principalId &&
    left.participantId === right.participantId &&
    (left.deploymentId ?? "") === (right.deploymentId ?? "") &&
    (left.instanceId ?? "") === (right.instanceId ?? "");
}

/** Canonical JSON with sorted object keys; permission atoms have no arrays. */
function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    return `{${
      Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
        .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`)
        .join(",")
    }}`;
  }
  return JSON.stringify(value);
}

function grantAllows(
  context: VerifiedContext,
  permission: PermissionAtom,
): boolean {
  // Compare semantically: the verifier's projection and this projection may
  // order object keys differently for the same authority atom.
  const wanted = canonicalJson(permission);
  return context.grants.permissions.some((granted) =>
    canonicalJson(granted) === wanted
  );
}

/** Map one authority loss into its bounded terminal outcome. */
export function authorityLostEnd(lost: LiveAuthorityLost): LiveEnd {
  switch (lost) {
    case "transport_unavailable":
      return new LiveEnd(
        "disconnected",
        new LiveStreamError("disconnected", "local transport is not usable"),
      );
    case "epoch_changed":
      return new LiveEnd(
        "disconnected",
        new LiveStreamError("disconnected", "local transport epoch changed"),
      );
    case "revoked":
      return new LiveEnd(
        "authorization_lost",
        new LiveStreamError(
          "authorization_revoked",
          "authorization context was revoked",
        ),
      );
    case "expired":
      return new LiveEnd(
        "authorization_lost",
        new LiveStreamError(
          "authorization_expired",
          "authorization context expired",
        ),
      );
    case "permission_lost":
      return new LiveEnd(
        "authorization_lost",
        new LiveStreamError(
          "permission_denied",
          "required permission was lost",
        ),
      );
    case "identity_changed":
    case "binding_changed":
      return new LiveEnd(
        "binding_changed",
        new LiveStreamError("binding_changed", "pinned identity changed"),
      );
    case "coverage_lost":
    case "coverage_unknown":
      return new LiveEnd(
        "authorization_lost",
        new LiveStreamError(
          "authorization_unavailable",
          "exact revocation coverage is unavailable",
        ),
      );
  }
}

/**
 * Retained authorization coverage for one live session.
 *
 * Replacement retains and fully validates the new evidence before releasing
 * the old covered lease. Dropping an uncommitted candidate releases its lease.
 */
export class LiveAuthorityGuard {
  readonly #cache: AuthorizationProviderCache;
  #lease: LiveLeaseView | undefined;
  readonly #requirement: LiveGuardRequirement;
  readonly #identity: PinnedPeerIdentity;
  readonly #epoch: number;

  private constructor(
    cache: AuthorizationProviderCache,
    lease: LiveLeaseView,
    requirement: LiveGuardRequirement,
    identity: PinnedPeerIdentity,
    epoch: number,
  ) {
    this.#cache = cache;
    this.#lease = lease;
    this.#requirement = requirement;
    this.#identity = identity;
    this.#epoch = epoch;
  }

  /**
   * Retain a live guard for one exact digest, permission and pinned identity.
   *
   * @throws LiveAuthorityLost when the exact covered evidence is unavailable.
   */
  static async retain(
    cache: AuthorizationProviderCache,
    digest: string,
    requirement: LiveGuardRequirement,
  ): Promise<LiveAuthorityGuard> {
    const epoch = cache.connectionGeneration();
    const lease = await retainLease(cache, digest, epoch);
    try {
      const identity = identityOf(lease.verified.context);
      if (requirement.kind === "observer") {
        if (!grantAllows(lease.verified.context, requirement.permission)) {
          throw new LiveAuthorityGuardError("permission_lost");
        }
      } else if (requirement.kind === "peer-provider") {
        if (!sameIdentity(identity, requirement.expected)) {
          throw new LiveAuthorityGuardError("identity_changed");
        }
      }
      return new LiveAuthorityGuard(
        cache,
        lease,
        requirement,
        identity,
        epoch,
      );
    } catch (error) {
      cache.releaseLiveLease(lease);
      throw error;
    }
  }

  /** Return the pinned identity this session retains. */
  get identity(): PinnedPeerIdentity {
    return this.#identity;
  }

  /** Return the retained context digest. */
  get contextDigest(): string {
    return this.#lease?.verified.contextDigest ?? "";
  }

  /** Return the retained context expiry in Unix seconds. */
  get expiresAtSeconds(): number {
    return this.#lease?.verified.context.expiresAt ?? 0;
  }

  /** Return whether the retained context grants one exact permission atom. */
  allows(permission: PermissionAtom): boolean {
    const lease = this.#lease;
    if (!lease) return false;
    return grantAllows(lease.verified.context, permission);
  }

  /**
   * Perform the synchronous local authority check.
   *
   * Uses only retained local state: no network read and no re-verification of
   * an opening proof. Returns the precise typed reason, never a lossy boolean.
   */
  checkNow(): LiveAuthorityLost | undefined {
    const lease = this.#lease;
    if (!lease) return "coverage_lost";
    if (!this.#cache.health().healthy) return "transport_unavailable";
    if (this.#cache.connectionGeneration() !== this.#epoch) {
      return "epoch_changed";
    }
    const coverage = this.#cache.liveLeaseCoverage(lease);
    if (coverage === "revoked") return "revoked";
    if (coverage === "epoch") return "epoch_changed";
    if (coverage === "lost") return "coverage_lost";
    const context = lease.verified.context;
    const now = this.#cache.liveNowSeconds();
    if (
      typeof context.notBefore !== "number" ||
      typeof context.expiresAt !== "number" ||
      context.notBefore > now || context.expiresAt <= now
    ) {
      return "expired";
    }
    if (!sameIdentity(identityOf(context), this.#identity)) {
      return "identity_changed";
    }
    if (this.#requirement.kind === "observer") {
      if (!grantAllows(context, this.#requirement.permission)) {
        return "permission_lost";
      }
    }
    return undefined;
  }

  /**
   * Resolve and validate a replacement without installing it.
   *
   * @throws LiveAuthorityLost when the replacement does not preserve the
   * pinned identity, role requirement and connection generation.
   */
  async prepareReplacement(
    digest: string,
  ): Promise<LiveAuthorityGuard> {
    const lease = await retainLease(this.#cache, digest, this.#epoch);
    try {
      const identity = identityOf(lease.verified.context);
      if (!sameIdentity(identity, this.#identity)) {
        throw new LiveAuthorityGuardError("identity_changed");
      }
      if (this.#requirement.kind === "observer") {
        if (
          !grantAllows(lease.verified.context, this.#requirement.permission)
        ) {
          throw new LiveAuthorityGuardError("permission_lost");
        }
      }
      return new LiveAuthorityGuard(
        this.#cache,
        lease,
        this.#requirement,
        identity,
        this.#epoch,
      );
    } catch (error) {
      this.#cache.releaseLiveLease(lease);
      throw error;
    }
  }

  /**
   * Atomically install a prepared replacement on the same generation.
   *
   * The old covered lease is released only after the new one is installed.
   */
  commitReplacement(
    candidate: LiveAuthorityGuard,
  ): LiveAuthorityLost | undefined {
    if (
      this.#cache.connectionGeneration() !== this.#epoch ||
      candidate.#epoch !== this.#epoch
    ) {
      return "epoch_changed";
    }
    if (!sameIdentity(candidate.#identity, this.#identity)) {
      return "identity_changed";
    }
    const candidateLease = candidate.#take();
    if (!candidateLease) return "coverage_lost";
    const previous = this.#lease;
    this.#lease = candidateLease;
    if (previous) this.#cache.releaseLiveLease(previous);
    return undefined;
  }

  /** Subscribe to local changes that can invalidate this guard. */
  subscribeChanges(callback: () => void): () => void {
    return this.#cache.subscribeLiveChanges(callback);
  }

  /** Release every retained lease held by this guard. */
  release(): void {
    const lease = this.#take();
    if (lease) this.#cache.releaseLiveLease(lease);
  }

  #take(): LiveLeaseView | undefined {
    const lease = this.#lease;
    this.#lease = undefined;
    return lease;
  }
}

/** Typed failure used while retaining a guard. */
export class LiveAuthorityGuardError extends Error {
  readonly lost: LiveAuthorityLost;

  constructor(lost: LiveAuthorityLost) {
    super(`live authority is not usable: ${lost}`);
    this.name = "LiveAuthorityGuardError";
    this.lost = lost;
  }
}

async function retainLease(
  cache: AuthorizationProviderCache,
  digest: string,
  epoch: number,
): Promise<LiveLeaseView> {
  try {
    return await cache.retainLiveLease(digest, epoch);
  } catch {
    if (!cache.health().healthy) {
      throw new LiveAuthorityGuardError("transport_unavailable");
    }
    if (cache.connectionGeneration() !== epoch) {
      throw new LiveAuthorityGuardError("epoch_changed");
    }
    throw new LiveAuthorityGuardError("coverage_lost");
  }
}

/** Extract the typed loss from one guard failure, if it is one. */
export function authorityLostFrom(
  error: unknown,
): LiveAuthorityLost | undefined {
  return error instanceof LiveAuthorityGuardError ? error.lost : undefined;
}
