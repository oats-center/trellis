import type {
  AuthorizationContextVerificationPolicy,
} from "../protocol_wasm.ts";
import type {
  AuthorizationContextBundle,
  AuthorizationContextVerificationMaterial,
  AuthorizationRoutingMaterial,
  AuthorizationRuntimeBinding,
  VerifiedAuthorizationContext,
} from "./types.ts";

/** Verifies and exposes the current in-memory authorization context. */
export class AuthorizationContextCache {
  #bundle?: AuthorizationContextBundle;
  #verified?: VerifiedAuthorizationContext;
  #runtime?: AuthorizationRuntimeBinding;
  #routing?: AuthorizationRoutingMaterial;
  #clockOffsetMs = 0;
  #operation = 0;
  #generation = 0;
  #refreshRequest?: () => void;
  #refreshRequestPending = false;
  #candidate?: {
    operation: number;
    bundle: AuthorizationContextBundle;
    verified: VerifiedAuthorizationContext;
    runtime?: AuthorizationRuntimeBinding;
    routing: AuthorizationRoutingMaterial;
    commitAdditional?: () => void;
  };

  constructor(
    readonly trellisUrl: string,
    readonly fetch: typeof globalThis.fetch = globalThis.fetch,
    readonly now: () => number = Date.now,
  ) {}

  async install(
    bundle: AuthorizationContextBundle,
    routing: AuthorizationRoutingMaterial,
    nowUnixSeconds = this.correctedNowSeconds(),
    shouldInstall: () => boolean = () => true,
    runtime?: AuthorizationRuntimeBinding,
    installAdditional: (
      verified: VerifiedAuthorizationContext,
    ) => void | (() => void) | Promise<void | (() => void)> = () => {},
  ): Promise<VerifiedAuthorizationContext> {
    const verified = await this.prepare(
      bundle,
      routing,
      nowUnixSeconds,
      shouldInstall,
      runtime,
      installAdditional,
    );
    return this.promote(verified.contextDigest, shouldInstall);
  }

  /** Verify and retain one private refresh candidate without exposing it to application use. */
  async prepare(
    bundle: AuthorizationContextBundle,
    routing: AuthorizationRoutingMaterial,
    nowUnixSeconds = this.correctedNowSeconds(),
    shouldInstall: () => boolean = () => true,
    runtime?: AuthorizationRuntimeBinding,
    installAdditional: (
      verified: VerifiedAuthorizationContext,
    ) => void | (() => void) | Promise<void | (() => void)> = () => {},
  ): Promise<VerifiedAuthorizationContext> {
    const operation = ++this.#operation;
    const verificationPolicy = authorizationContextVerificationPolicy(
      bundle.policy,
      nowUnixSeconds,
    );
    const verified = await verifyAuthorizationContext({
      bundle,
      policy: verificationPolicy,
    });
    const nextRuntime = runtime ?? this.#runtime;
    if (
      nextRuntime &&
      (nextRuntime.connectionId !== verified.context.connectionId ||
        nextRuntime.loginSessionId !== verified.context.loginSessionId ||
        nextRuntime.participantId !== verified.context.participantId ||
        nextRuntime.inboxPrefix !== verified.context.inboxPrefix ||
        !Object.values(nextRuntime.transports).some((transport) =>
          transport.natsServers.length > 0
        ))
    ) {
      throw new Error(
        "authorization runtime binding does not match signed context",
      );
    }
    if (!shouldInstall() || operation !== this.#operation) {
      throw new Error("authorization context installation stopped");
    }
    const nextBundle = structuredClone(bundle);
    const installedRuntime = nextRuntime
      ? structuredClone(nextRuntime)
      : undefined;
    const nextRouting = structuredClone(routing);
    const commitAdditional = await installAdditional(verified);
    if (!shouldInstall() || operation !== this.#operation) {
      throw new Error("authorization context installation stopped");
    }
    this.#candidate = {
      operation,
      bundle: nextBundle,
      verified,
      runtime: installedRuntime,
      routing: nextRouting,
      commitAdditional: typeof commitAdditional === "function"
        ? commitAdditional
        : undefined,
    };
    return verified;
  }

  /** Promote only the exact prepared candidate after transport admission and own coverage. */
  promote(
    expectedDigest: string,
    shouldInstall: () => boolean = () => true,
  ): VerifiedAuthorizationContext {
    const candidate = this.#candidate;
    if (
      !candidate || candidate.operation !== this.#operation ||
      candidate.verified.contextDigest !== expectedDigest || !shouldInstall()
    ) {
      throw new Error("authorization candidate changed before promotion");
    }
    candidate.commitAdditional?.();
    this.#bundle = candidate.bundle;
    this.#verified = candidate.verified;
    this.#runtime = candidate.runtime;
    this.#routing = candidate.routing;
    this.#candidate = undefined;
    this.#generation += 1;
    return candidate.verified;
  }

  /** Return the installed context identity without time or usability checks. */
  storedContextDigest(): string | undefined {
    return this.#verified?.contextDigest;
  }

  /** Drop a private candidate that became unusable before promotion. */
  invalidateCandidate(digest: string): boolean {
    if (this.#candidate?.verified.contextDigest === digest) {
      this.#candidate = undefined;
      return true;
    }
    return false;
  }

  /** Return the verified candidate used only for transport reauthorization. */
  transportCurrent(): VerifiedAuthorizationContext {
    return this.#candidate?.verified ?? this.current();
  }

  hasCandidate(): boolean {
    return this.#candidate !== undefined;
  }

  transportRuntimeBinding(): AuthorizationRuntimeBinding {
    const runtime = this.#candidate?.runtime ?? this.#runtime;
    if (!runtime) {
      throw new Error("no authorization runtime metadata is installed");
    }
    return structuredClone(runtime);
  }

  transportRoutingJwt(): string {
    const routing = this.#candidate?.routing ?? this.#routing;
    if (
      !routing || routing.bootstrapJwtExpiresAt <= this.correctedNowSeconds()
    ) throw new Error("authorization routing JWT expired");
    return routing.bootstrapJwt;
  }

  /** Returns the number of successfully installed authorization contexts. */
  generation(): number {
    return this.#generation;
  }

  current(
    nowUnixSeconds = this.correctedNowSeconds(),
  ): VerifiedAuthorizationContext {
    const verified = this.#verified;
    if (
      !verified || verified.context.notBefore > nowUnixSeconds ||
      verified.context.expiresAt <= nowUnixSeconds
    ) {
      throw new Error("no current authorization context");
    }
    return verified;
  }

  bundle(): AuthorizationContextBundle {
    if (!this.#bundle) throw new Error("no authorization context is installed");
    return structuredClone(this.#bundle);
  }

  /** Return the current compact verifier policy without cloning the context bundle. */
  verificationPolicy(nowUnixSeconds = this.correctedNowSeconds()) {
    if (!this.#bundle) throw new Error("no authorization context is installed");
    return authorizationContextVerificationPolicy(
      this.#bundle.policy,
      nowUnixSeconds,
    );
  }

  shouldRefresh(nowUnixSeconds = this.correctedNowSeconds()): boolean {
    this.current(nowUnixSeconds);
    return nowUnixSeconds >= this.routingRefreshAt();
  }

  async clear(): Promise<void> {
    this.#operation += 1;
    this.#bundle = undefined;
    this.#verified = undefined;
    this.#routing = undefined;
    this.#candidate = undefined;
  }

  /** Capture the exact material owned by an in-flight refresh. */
  clearGuard(): readonly [string | null, string | null, number] {
    return [
      this.#verified?.contextDigest ?? null,
      this.#routing?.bootstrapJwt ?? null,
      this.#operation,
    ];
  }

  /** Clear terminal state only if no newer context or route JWT replaced it. */
  async clearIfCurrent(
    guard: readonly [string | null, string | null, number],
  ): Promise<boolean> {
    if (
      (this.#verified?.contextDigest ?? null) !== guard[0] ||
      (this.#routing?.bootstrapJwt ?? null) !== guard[1] ||
      this.#operation !== guard[2]
    ) return false;
    this.#operation += 1;
    this.#bundle = undefined;
    this.#verified = undefined;
    this.#routing = undefined;
    this.#candidate = undefined;
    return true;
  }

  /** Return reconnect metadata retained with the signed context. */
  runtimeBinding(): AuthorizationRuntimeBinding {
    if (!this.#runtime) {
      throw new Error("no authorization runtime metadata is installed");
    }
    return structuredClone(this.#runtime);
  }

  routingJwt(): string {
    if (
      !this.#routing ||
      this.#routing.bootstrapJwtExpiresAt <= this.correctedNowSeconds()
    ) {
      throw new Error("authorization routing JWT expired");
    }
    return this.#routing.bootstrapJwt;
  }

  routingRefreshAt(): number {
    const context = this.current();
    if (!this.#routing || !this.#bundle) return this.correctedNowSeconds();
    return Math.min(
      context.refreshAt,
      this.#routing.bootstrapJwtExpiresAt -
        this.#bundle.policy.refreshLeadSeconds,
    );
  }

  setServerClockOffsetMs(offsetMs: number): void {
    this.#clockOffsetMs = offsetMs;
  }

  serverClockOffsetMs(): number {
    return this.#clockOffsetMs;
  }

  correctedNowSeconds(): number {
    return Math.floor((this.now() + this.#clockOffsetMs) / 1_000);
  }

  nowMilliseconds(): number {
    return this.now();
  }

  /**
   * Return the installed chain material for a provider-side local verifier.
   *
   * This is an in-process handoff; it performs no registry I/O.
   */
  installedVerificationMaterial(): AuthorizationContextVerificationMaterial {
    if (
      !this.#bundle || !this.#verified
    ) {
      throw new Error("no authorization verification material is installed");
    }
    return {
      issuer: structuredClone(this.#bundle.issuer),
      context: structuredClone(this.#bundle.context),
      contextDigest: this.#verified.contextDigest,
      policy: authorizationContextVerificationPolicy(
        this.#bundle.policy,
        this.correctedNowSeconds(),
      ),
      verified: structuredClone(this.#verified),
    };
  }

  /** Register the single existing refresh task wake callback. */
  registerRefreshRequest(callback: () => void): () => void {
    if (this.#refreshRequest) {
      throw new Error("authorization refresh callback is already registered");
    }
    this.#refreshRequest = callback;
    if (this.#refreshRequestPending) {
      this.#refreshRequestPending = false;
      callback();
    }
    return () => {
      if (this.#refreshRequest === callback) this.#refreshRequest = undefined;
    };
  }

  /** Wake the existing refresh task after trust-floor advance. */
  requestRefresh(): void {
    const callback = this.#refreshRequest;
    if (callback) {
      callback();
    } else {
      this.#refreshRequestPending = true;
    }
  }
}

/** Build the exact policy passed to the Rust/WASM verifier. */
export function authorizationContextVerificationPolicy(
  policy: AuthorizationContextBundle["policy"],
  nowUnixSeconds: number,
): AuthorizationContextVerificationPolicy {
  return {
    nowUnixSeconds,
    allowedClockSkewSeconds: policy.allowedClockSkewSeconds,
    maximumContextLifetimeSeconds: policy.maximumContextLifetimeSeconds,
    maximumContextBytes: policy.maximumContextBytes,
    maximumPermissions: policy.maximumPermissions,
    refreshLeadSeconds: policy.refreshLeadSeconds,
    refreshJitterSeconds: policy.refreshJitterSeconds,
  };
}

/** Verify a complete authorization context chain through Rust/WASM. */
export async function verifyAuthorizationContext(args: {
  bundle: AuthorizationContextBundle;
  policy: AuthorizationContextVerificationPolicy;
}): Promise<VerifiedAuthorizationContext> {
  const { verifyAuthorizationContextWasm } = await import(
    "../protocol_wasm.ts"
  );
  const result = await verifyAuthorizationContextWasm({
    issuer: args.bundle.issuer,
    context: args.bundle.context,
    policy: args.policy,
  });
  return result;
}
