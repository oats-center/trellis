import type {
  AuthenticatedCaller,
  CatalogActionIdentity,
  SessionAuthority,
} from "../auth/protocol_wasm.ts";
import type {
  SessionAuthorityOwner,
  VerifiedAuthority,
} from "../auth/authority.ts";
import { LiveEnd, LiveStreamError } from "./types.ts";

/** Actual authenticated client/deployment identity, including browser/native bindings. */
export type PinnedPeerIdentity = Pick<
  AuthenticatedCaller,
  "runtimeId" | "sessionPublicKey" | "principalId" | "binding"
>;

/** Why a retained session can no longer authorize new frames. */
export type LiveAuthorityLost =
  | "unavailable"
  | "revoked"
  | "expired"
  | "identity_changed"
  | "binding_changed"
  | "permission_lost";

/** Compiler-owned action or certified provider role, never a permission-atom set. */
export type LiveGuardRequirement =
  | { kind: "session" }
  | { kind: "observer"; apiId: string; action: CatalogActionIdentity }
  | { kind: "local-provider"; apiId: string; action: CatalogActionIdentity }
  | {
      kind: "peer-provider";
      expected: PinnedPeerIdentity;
      apiId: string;
      generation: string;
      action: CatalogActionIdentity;
    };

/** Translate an authority lifetime failure into the existing Live terminal vocabulary. */
export function authorityLostEnd(lost: LiveAuthorityLost): LiveEnd {
  switch (lost) {
    case "revoked":
      return new LiveEnd(
        "authorization_lost",
        new LiveStreamError(
          "authorization_revoked",
          "authorization session was revoked",
        ),
      );
    case "expired":
      return new LiveEnd(
        "authorization_lost",
        new LiveStreamError(
          "authorization_expired",
          "session authority expired",
        ),
      );
    case "permission_lost":
      return new LiveEnd(
        "authorization_lost",
        new LiveStreamError(
          "permission_denied",
          "required capability membership was lost",
        ),
      );
    case "identity_changed":
    case "binding_changed":
      return new LiveEnd(
        "binding_changed",
        new LiveStreamError("binding_changed", "pinned identity changed"),
      );
    case "unavailable":
      return new LiveEnd(
        "authorization_lost",
        new LiveStreamError(
          "authorization_unavailable",
          "verified session authority is unavailable",
        ),
      );
  }
}

/** Retains verified application authority independently of physical transport generations. */
export class LiveAuthorityGuard {
  readonly #owner: SessionAuthorityOwner;
  readonly #requirement: LiveGuardRequirement;
  readonly #identity: PinnedPeerIdentity;
  readonly #tracksLocal: boolean;
  #lease?: VerifiedAuthority;

  private constructor(
    owner: SessionAuthorityOwner,
    lease: VerifiedAuthority,
    requirement: LiveGuardRequirement,
    tracksLocal: boolean,
  ) {
    this.#owner = owner;
    this.#lease = lease;
    this.#requirement = requirement;
    this.#tracksLocal = tracksLocal;
    this.#identity = {
      runtimeId: lease.authority.runtimeId,
      sessionPublicKey: lease.authority.sessionPublicKey,
      principalId: lease.authority.principalId,
      binding: lease.authority.binding,
    };
  }

  /** Verify before retention, including real catalog membership for observers. */
  static async retain(
    owner: SessionAuthorityOwner,
    digest: string,
    requirement: LiveGuardRequirement,
    retainAuthority?: () => VerifiedAuthority,
  ): Promise<LiveAuthorityGuard> {
    const lease = retainAuthority
      ? retainAuthority()
      : await owner.peer(digest);
    try {
      if (requirement.kind === "observer")
        await lease.authorize(requirement.apiId, requirement.action);
      if (
        requirement.kind === "peer-provider" &&
        !sameIdentity(lease.authority, requirement.expected)
      )
        throw new LiveAuthorityGuardError("identity_changed");
      if (
        (requirement.kind === "local-provider" ||
          requirement.kind === "peer-provider") &&
        !lease.authority.providerBindings.length
      )
        throw new LiveAuthorityGuardError("permission_lost");
      if (
        requirement.kind === "local-provider" ||
        requirement.kind === "peer-provider"
      ) {
        const generation =
          requirement.kind === "peer-provider"
            ? requirement.generation
            : lease.authority.providerBindings.find(
                (binding) => binding.apiId === requirement.apiId,
              )?.generation;
        if (!generation) throw new LiveAuthorityGuardError("permission_lost");
        await lease.authorizeProvider(
          requirement.apiId,
          generation,
          requirement.action,
        );
      }
      lease.assertCurrent();
      return new LiveAuthorityGuard(
        owner,
        lease,
        requirement,
        requirement.kind === "local-provider" || owner.ownDigest === digest,
      );
    } catch (error) {
      lease.release();
      throw error;
    }
  }

  /** Authenticated pinned endpoint identity. */
  get identity(): PinnedPeerIdentity {
    return this.#identity;
  }
  /** Accepted API stamps from the retained signed authority. */
  get apis(): SessionAuthority["apis"] {
    return this.#lease?.authority.apis ?? [];
  }
  /** Exact retained signed authority content address. */
  get contextDigest(): string {
    return this.#lease?.digest ?? "";
  }
  /** Deadline of this exact authority, not a historical CONNECT credential. */
  get expiresAtSeconds(): number {
    return this.#lease?.authority.expiresAt ?? 0;
  }

  /** Adopt an already promoted own renewal without changing accepted Live state. */
  async reconcile(): Promise<LiveAuthorityLost | undefined> {
    if (
      this.#tracksLocal &&
      this.#owner.ownDigest &&
      this.#owner.ownDigest !== this.contextDigest
    ) {
      let candidate: LiveAuthorityGuard | undefined;
      try {
        candidate = await this.prepareReplacement(this.#owner.ownDigest);
        return this.commitReplacement(candidate);
      } catch (error) {
        return authorityLostFrom(error) ?? "unavailable";
      } finally {
        candidate?.release();
      }
    }
    return this.checkNow();
  }

  /** Local current-time and sticky retirement check; frames perform no registry reads. */
  checkNow(): LiveAuthorityLost | undefined {
    if (!this.#lease) return "unavailable";
    if (!this.#lease.published) return "unavailable";
    if (this.#owner.isRevoked(this.#lease.authority.authorizationSessionId))
      return "revoked";
    if (this.#lease.authority.expiresAt <= this.#owner.policy().nowUnixSeconds)
      return "expired";
    if (!sameIdentity(this.#lease.authority, this.#identity))
      return "identity_changed";
    try {
      this.#lease.assertCurrent();
      return undefined;
    } catch {
      return "unavailable";
    }
  }

  /** Validate replacement identity, authority and action before mutating active Live state. */
  async prepareReplacement(digest: string): Promise<LiveAuthorityGuard> {
    const candidate = await LiveAuthorityGuard.retain(
      this.#owner,
      digest,
      this.#requirement,
    );
    if (!sameIdentity(candidate.#identity, this.#identity)) {
      candidate.release();
      throw new LiveAuthorityGuardError("identity_changed");
    }
    return candidate;
  }

  /** Promote only a still-current candidate from the same trusted owner. */
  commitReplacement(
    candidate: LiveAuthorityGuard,
  ): LiveAuthorityLost | undefined {
    if (
      candidate.#owner !== this.#owner ||
      !sameIdentity(candidate.#identity, this.#identity)
    )
      return "identity_changed";
    const lost = candidate.checkNow();
    if (lost) return lost;
    const previous = this.#lease;
    this.#lease = candidate.#lease;
    candidate.#lease = undefined;
    previous?.release();
    return undefined;
  }

  /** Wake existing reconciliation on own renewal, sticky retirement or shutdown. */
  subscribeChanges(callback: () => void): () => void {
    return this.#owner.subscribeChanges(callback);
  }
  /** Release retained local material; accepted domain work remains committed. */
  release(): void {
    this.#lease?.release();
    this.#lease = undefined;
  }
}

/** Typed authority-lifetime failure for the existing long-lived terminal mapping. */
export class LiveAuthorityGuardError extends Error {
  constructor(readonly lost: LiveAuthorityLost) {
    super(`live authority is not usable: ${lost}`);
  }
}

/** Recover a guard's typed loss without classifying exception message text. */
export function authorityLostFrom(
  error: unknown,
): LiveAuthorityLost | undefined {
  return error instanceof LiveAuthorityGuardError ? error.lost : undefined;
}

function sameIdentity(
  left: PinnedPeerIdentity | SessionAuthority,
  right: PinnedPeerIdentity,
): boolean {
  return (
    left.runtimeId === right.runtimeId &&
    left.sessionPublicKey === right.sessionPublicKey &&
    left.principalId === right.principalId &&
    left.binding.kind === right.binding.kind &&
    Object.entries(left.binding).every(
      ([key, value]) => Reflect.get(right.binding, key) === value,
    ) &&
    Object.keys(left.binding).length === Object.keys(right.binding).length
  );
}
