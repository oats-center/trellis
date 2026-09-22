import {
  type BrowserPortalFlowState as PortalFlowState,
  fetchPortalFlowState,
  getOrCreatePortalBinding,
  type PortalBinding,
  portalFlowIdFromUrl,
  portalProviderLoginUrl,
  submitPortalApproval,
  TrellisHttpError,
} from "@qlever-llc/trellis/auth/browser";

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
  flowId: string | null = $state(null);
  state: PortalFlowState | null = $state(null);
  loading = $state(false);
  error: string | null = $state(null);
  errorCode: string | null = $state(null);

  #config: AuthConfig;
  #getUrl: () => URL;
  #sessionStorage: Storage;
  #binding: PortalBinding | null = null;

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
      const flowId = portalFlowIdFromUrl(this.#getUrl());
      this.flowId = flowId;
      if (!flowId) {
        this.error = "Missing flow id.";
        this.errorCode = "missing_flow_id";
        return null;
      }

      this.#binding = await getOrCreatePortalBinding(
        flowId,
        this.#sessionStorage,
      );
      const state = await fetchPortalFlowState(
        this.#config,
        flowId,
        this.#binding,
      );
      this.state = state;
      return state;
    } catch (error) {
      this.error = errorMessage(error);
      this.errorCode = error instanceof TrellisHttpError ? error.code : null;
      this.state = null;
      return null;
    } finally {
      this.loading = false;
    }
  }

  providerUrl(providerId: string): string {
    if (!this.flowId) {
      throw new Error("Missing flow id.");
    }

    if (!this.#binding) throw new Error("Portal flow has not loaded.");
    return portalProviderLoginUrl(
      this.#config,
      providerId,
      this.flowId,
      this.#binding,
    );
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
    if (!this.flowId) {
      this.error = "Missing flow id.";
      return null;
    }

    this.loading = true;
    this.error = null;
    this.errorCode = null;

    try {
      const state = await submitPortalApproval(
        this.#config,
        this.flowId,
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
