import {
  type BrowserPortalFlowState as PortalFlowState,
  createPortalBinding,
  fetchPortalFlowState,
  fetchPortalIntentState,
  getOrCreatePortalBinding,
  type PortalBinding,
  portalIntentFromUrl,
  portalProviderLoginUrl,
  portalTransactionIdFromUrl,
  startPortalTransaction,
  submitPortalApproval,
  TrellisHttpError,
} from "@oatscenter/trellis/auth/browser";

type AuthConfig = { authUrl: string };

export type CreatePortalFlowConfig = AuthConfig & {
  getUrl?: () => URL;
  sessionStorage?: Storage;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function defaultGetUrl(): URL {
  return new URL(globalThis.location.href);
}

export class PortalFlowController {
  transactionId: string | null = $state(null);
  state: PortalFlowState | null = $state(null);
  loading = $state(false);
  error: string | null = $state(null);
  errorCode: string | null = $state(null);
  /** The previous attempt expired; authenticating here starts another from the same intent. */
  authenticationAttemptExpired = $state(false);

  #config: AuthConfig;
  #getUrl: () => URL;
  #sessionStorage: Storage;
  #binding: PortalBinding | null = null;
  #intent: string | null = null;
  #initialized = false;

  constructor(config: CreatePortalFlowConfig) {
    this.#config = { authUrl: config.authUrl };
    this.#getUrl = config.getUrl ?? defaultGetUrl;
    this.#sessionStorage = config.sessionStorage ?? globalThis.sessionStorage;
  }

  async load(): Promise<PortalFlowState | null> {
    this.loading = true;
    this.error = null;
    this.errorCode = null;
    this.state = null;

    try {
      const url = this.#getUrl();
      if (!this.#initialized) {
        this.transactionId = portalTransactionIdFromUrl(url);
        this.#initialized = true;
      }
      this.#intent ??= portalIntentFromUrl(url) ??
        (this.transactionId
          ? this.#sessionStorage.getItem(
            `trellis.portal.intent.${this.transactionId}`,
          )
          : null);
      if (!this.transactionId && !this.#intent) {
        this.error = "Missing sign-in intent.";
        this.errorCode = "missing_signin_intent";
        return null;
      }
      let state: PortalFlowState;
      if (this.transactionId) {
        this.#binding = await getOrCreatePortalBinding(
          this.transactionId,
          this.#sessionStorage,
        );
        state = await fetchPortalFlowState(
          this.#config,
          this.transactionId,
          this.#binding,
        );
      } else {
        state = await fetchPortalIntentState(this.#config, this.#intent!);
      }
      if (state.status === "expired" && this.#intent) {
        this.authenticationAttemptExpired = true;
        this.transactionId = null;
        this.#binding = null;
        state = await fetchPortalIntentState(this.#config, this.#intent);
      }
      this.state = state;
      return state;
    } catch (error) {
      if (
        this.transactionId && this.#intent &&
        error instanceof TrellisHttpError &&
        (error.code === "transaction_expired" ||
          error.code === "transaction_not_found")
      ) {
        this.transactionId = null;
        this.#binding = null;
        this.authenticationAttemptExpired = true;
        return await this.load();
      }
      this.error = errorMessage(error);
      this.errorCode = error instanceof TrellisHttpError ? error.code : null;
      this.state = null;
      return null;
    } finally {
      this.loading = false;
    }
  }

  async providerUrl(providerId: string): Promise<string> {
    const transactionId = await this.beginAuthentication();
    return portalProviderLoginUrl(
      this.#config,
      providerId,
      transactionId,
      this.binding,
    );
  }

  /** Create a bounded attempt only when the user submits credentials or selects a provider. */
  async beginAuthentication(): Promise<string> {
    if (this.transactionId) return this.transactionId;
    if (!this.#intent || this.state?.status !== "choose_provider") {
      throw new Error("Sign-in intent has not loaded.");
    }
    const binding = await createPortalBinding();
    const transactionId = await startPortalTransaction(
      this.#config,
      this.#intent,
      binding,
    );
    this.#sessionStorage.setItem(
      `trellis.portal-binding.v1:${transactionId}`,
      binding.secret,
    );
    this.#sessionStorage.setItem(
      `trellis.portal.intent.${transactionId}`,
      this.#intent,
    );
    this.#binding = binding;
    this.transactionId = transactionId;
    this.authenticationAttemptExpired = false;
    const url = this.#getUrl();
    url.searchParams.set("transactionId", transactionId);
    globalThis.history?.replaceState(null, "", url);
    return transactionId;
  }

  get binding(): PortalBinding {
    if (!this.#binding) throw new Error("Portal flow has not loaded.");
    return this.#binding;
  }

  async approve(): Promise<PortalFlowState | null> {
    return this.#submit("approved");
  }

  async deny(): Promise<PortalFlowState | null> {
    return this.#submit("denied");
  }

  async #submit(
    decision: "approved" | "denied",
  ): Promise<PortalFlowState | null> {
    if (!this.transactionId) {
      this.error = "Missing authentication transaction.";
      return null;
    }

    this.loading = true;
    this.error = null;
    this.errorCode = null;

    try {
      const state = await submitPortalApproval(
        this.#config,
        this.transactionId,
        this.binding,
        decision,
      );
      this.state = state;
      return state;
    } catch (error) {
      this.error = errorMessage(error);
      this.errorCode = error instanceof TrellisHttpError ? error.code : null;
      return null;
    } finally {
      this.loading = false;
    }
  }
}

export function createPortalFlow(
  config: CreatePortalFlowConfig,
): PortalFlowController {
  return new PortalFlowController(config);
}
