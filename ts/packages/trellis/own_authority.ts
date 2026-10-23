import type { SessionAuthorityOwner } from "./auth/authority.ts";
import type {
  AuthenticatedCaller,
  CatalogActionIdentity,
} from "./auth/protocol_wasm.ts";
import { toCatalogAction } from "./session.ts";
import type { RuntimeApi } from "./participant_runtime/api.ts";
import { TransportError } from "./errors/TransportError.ts";

/** One action authorized through the verified own authority's exact catalog. */
export type OwnAuthorizedAction = Readonly<{
  apiId: string;
  action: Readonly<CatalogActionIdentity>;
}>;

/** Advisory projection of a currently usable, verified and promoted own authority. */
export type OwnAuthoritySnapshot = Readonly<{
  authorityDigest: string;
  principalId: string;
  principalKind: AuthenticatedCaller["principalKind"];
  binding: Readonly<AuthenticatedCaller["binding"]>;
  authorizationSessionId: string;
  loginSessionId?: string;
  oauthGrantId?: string;
  runtimeId: string;
  expiresAt: number;
  capabilities: readonly Readonly<
    AuthenticatedCaller["capabilities"][number]
  >[];
  actions: readonly OwnAuthorizedAction[];
}>;

/** Read the current projection and observe replacement or loss of usability. */
export interface OwnAuthorityView {
  readonly current: OwnAuthoritySnapshot | undefined;
  subscribe(
    listener: (snapshot: OwnAuthoritySnapshot | undefined) => void,
  ): () => void;
}

/** Projects the existing owner without installing authority or owning another cache. @internal */
export class PromotedOwnAuthorityView implements OwnAuthorityView {
  #snapshot?: OwnAuthoritySnapshot;
  #generation = 0;
  #listeners = new Set<(snapshot: OwnAuthoritySnapshot | undefined) => void>();
  #unsubscribe: () => void;
  #closed = false;

  constructor(
    private readonly owner: SessionAuthorityOwner,
    private readonly api: RuntimeApi,
  ) {
    this.#unsubscribe = owner.subscribeChanges(() => {
      void this.refresh().catch(() => undefined);
    });
  }

  get current(): OwnAuthoritySnapshot | undefined {
    if (this.#closed || !this.#snapshot) return undefined;
    try {
      const lease = this.owner.own();
      try {
        return lease.digest === this.#snapshot.authorityDigest
          ? this.#snapshot
          : undefined;
      } finally {
        lease.release();
      }
    } catch {
      return undefined;
    }
  }

  subscribe(
    listener: (snapshot: OwnAuthoritySnapshot | undefined) => void,
  ): () => void {
    this.#listeners.add(listener);
    listener(this.current);
    return () => this.#listeners.delete(listener);
  }

  /** Recompute from one retained verified installation, rejecting stale completions. */
  async refresh(): Promise<void> {
    const generation = ++this.#generation;
    // Keep the immutable projection across transport suspension. `current`
    // hides it unless the owner is usable and still holds this exact digest;
    // refreshing unchanged authority must not temporarily withdraw actions.
    for (const listener of this.#listeners) listener(this.current);
    if (this.#closed) return;
    try {
      const lease = this.owner.own();
      try {
        const allowed: OwnAuthorizedAction[] = [];
        const permissions = [
          ...Object.values(this.api.rpc).map(
            (descriptor) => descriptor.permission,
          ),
          ...Object.values(this.api.events).flatMap((descriptor) => [
            descriptor.publishPermission,
            descriptor.subscribePermission,
          ]),
          ...Object.values(this.api.operations).flatMap((descriptor) => [
            descriptor.permissions.invoke,
            descriptor.permissions.observe,
            descriptor.permissions.cancel,
            ...Object.values(descriptor.permissions.control),
          ]),
          ...Object.values(this.api.lives ?? {}).map(
            (descriptor) => descriptor.permission,
          ),
        ];
        for (const permission of permissions) {
          const apiId = `${permission.apiId}@${permission.apiVersion}`;
          const action = toCatalogAction(permission);
          if (await lease.allows(apiId, action))
            allowed.push(
              Object.freeze({ apiId, action: Object.freeze(action) }),
            );
        }
        const acceptedCapabilities = new Map<
          string,
          { identityGeneration: string; consentRevision: string }
        >();
        for (const api of lease.authority.apis) {
          const catalog = await lease.authenticatedCatalog(api.apiId);
          if (!catalog || !Array.isArray(catalog.capabilities)) continue;
          for (const capability of catalog.capabilities) {
            if (
              capability &&
              typeof capability === "object" &&
              !Array.isArray(capability) &&
              typeof capability.capabilityId === "string" &&
              typeof capability.identityGeneration === "string" &&
              typeof capability.consentRevision === "string"
            ) {
              acceptedCapabilities.set(capability.capabilityId, {
                identityGeneration: capability.identityGeneration,
                consentRevision: capability.consentRevision,
              });
            }
          }
        }
        lease.assertCurrent();
        if (
          this.#closed ||
          generation !== this.#generation ||
          this.owner.ownDigest !== lease.digest
        )
          return;
        const authority = lease.authority;
        this.#snapshot = Object.freeze({
          authorityDigest: lease.digest,
          principalId: authority.principalId,
          principalKind: authority.principalKind,
          binding: Object.freeze({ ...authority.binding }),
          authorizationSessionId: authority.authorizationSessionId,
          loginSessionId: authority.loginSessionId,
          oauthGrantId: authority.oauthGrantId,
          runtimeId: authority.runtimeId,
          expiresAt: authority.expiresAt,
          capabilities: Object.freeze(
            authority.capabilities
              .filter((capability) => {
                const accepted = acceptedCapabilities.get(
                  capability.capabilityId,
                );
                return (
                  accepted?.identityGeneration ===
                    capability.identityGeneration &&
                  accepted.consentRevision === capability.consentRevision
                );
              })
              .map((capability) => Object.freeze({ ...capability })),
          ),
          actions: Object.freeze(allowed),
        });
      } finally {
        lease.release();
      }
    } catch (cause) {
      if (generation !== this.#generation) return;
      for (const listener of this.#listeners) listener(undefined);
      throw new TransportError({
        code: "trellis.authority.unavailable",
        message: "The current authority catalog could not be verified.",
        hint: "Reconnect when verified authority is available.",
        cause,
      });
    }
    for (const listener of this.#listeners) listener(this.current);
  }

  /** Stop observing the logical owner's lifecycle. */
  close(): void {
    this.#closed = true;
    ++this.#generation;
    this.#snapshot = undefined;
    this.#unsubscribe();
    for (const listener of this.#listeners) listener(undefined);
    this.#listeners.clear();
  }
}
