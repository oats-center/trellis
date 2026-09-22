import {
  type AuthorizationContextBundle,
  AuthorizationContextCache,
  AuthorizationContextRefreshError,
  type AuthorizationRoutingMaterial,
  refreshAuthorizationContext,
  startAuthorizationContextRefresh,
  type TrellisAuth,
  type VerifiedAuthorizationContext,
} from "@qlever-llc/trellis/auth";

export type AuthorizationContextStatus =
  | "empty"
  | "loading"
  | "ready"
  | "refreshing"
  | "degraded"
  | "expired"
  | "authRequired"
  | "failed";

/** Reactive projection of the shared TypeScript authorization context cache. */
export class AuthorizationContextController {
  #cache: AuthorizationContextCache;
  #context = $state.raw<VerifiedAuthorizationContext>();
  #status = $state<AuthorizationContextStatus>("empty");
  #error = $state.raw<unknown>();
  #stop?: () => void;
  #epoch = 0;

  constructor(
    trellisUrl: string,
    fetch?: typeof globalThis.fetch,
  ) {
    this.#cache = new AuthorizationContextCache(trellisUrl, fetch);
  }

  get context(): VerifiedAuthorizationContext | undefined {
    return this.#context;
  }

  get status(): AuthorizationContextStatus {
    return this.#status;
  }

  get error(): unknown {
    return this.#error;
  }

  get refreshAt(): number | undefined {
    return this.#context?.refreshAt;
  }

  get expiresAt(): number | undefined {
    return this.#context?.context.expiresAt;
  }

  async install(
    bundle: AuthorizationContextBundle,
    routing: AuthorizationRoutingMaterial,
  ): Promise<void> {
    this.#status = "loading";
    this.#error = undefined;
    try {
      this.#context = await this.#cache.install(bundle, routing);
      this.#status = "ready";
    } catch (error) {
      this.#status = "failed";
      this.#error = error;
      throw error;
    }
  }

  async refresh(sessionId: string, auth: TrellisAuth): Promise<void> {
    const epoch = this.#epoch;
    const clearGuard = this.#cache.clearGuard();
    this.#status = "refreshing";
    try {
      this.#context = await refreshAuthorizationContext({
        trellisUrl: this.#cache.trellisUrl,
        sessionId,
        auth,
        cache: this.#cache,
        shouldInstall: () => epoch === this.#epoch,
      });
      this.#status = "ready";
      this.#error = undefined;
    } catch (error) {
      if (epoch !== this.#epoch) return;
      if (error instanceof AuthorizationContextRefreshError && error.terminal) {
        if (!(await this.#cache.clearIfCurrent(clearGuard))) {
          this.#context = this.#cache.current();
          this.#status = "ready";
          this.#error = undefined;
          return;
        }
        this.#context = undefined;
      }
      this.#status = error instanceof AuthorizationContextRefreshError &&
          error.terminal
        ? "authRequired"
        : this.#context?.context.expiresAt !== undefined &&
            this.#context.context.expiresAt <= this.#cache.correctedNowSeconds()
        ? "expired"
        : "degraded";
      this.#error = error;
      throw error;
    }
  }

  async refreshNow(sessionId: string, auth: TrellisAuth): Promise<void> {
    await this.refresh(sessionId, auth);
  }

  start(sessionId: string, auth: TrellisAuth): void {
    this.stop();
    if (!this.#context) this.#status = "refreshing";
    this.#stop = startAuthorizationContextRefresh({
      trellisUrl: this.#cache.trellisUrl,
      sessionId,
      auth,
      cache: this.#cache,
      onRefresh: (context) => {
        this.#context = context;
        this.#status = "ready";
        this.#error = undefined;
      },
      onTerminalFailure: (error) => {
        this.#context = undefined;
        this.#status = "authRequired";
        this.#error = error;
      },
      onTransientFailure: (error) => {
        this.#status = "degraded";
        this.#error = error;
      },
    });
  }

  stop(): void {
    this.#stop?.();
    this.#stop = undefined;
  }

  async clear(): Promise<void> {
    this.#epoch += 1;
    this.stop();
    await this.#cache.clear();
    this.#context = undefined;
    this.#status = "empty";
    this.#error = undefined;
  }
}
