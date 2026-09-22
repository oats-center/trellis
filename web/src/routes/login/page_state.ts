import {
  classifyBrowserAuthError,
  decodeTrellisHttpError,
} from "@qlever-llc/trellis/auth/browser";

export const MISSING_PORTAL_FLOW_ID_ERROR = "Missing flow id.";

export const GENERIC_MISSING_CAPABILITY_LABEL = "Additional access required";

export interface CapabilityMetadata {
  displayName: string;
  description: string;
  consequence?: string;
}

function isSamePageLocation(currentUrl: URL, location: string | null): boolean {
  if (!location) return false;

  const nextUrl = new URL(location, currentUrl);
  return `${nextUrl.origin}${nextUrl.pathname}${nextUrl.search}` ===
    `${currentUrl.origin}${currentUrl.pathname}${currentUrl.search}`;
}

type LocalLoginSuccess = {
  state: "authenticated" | "approval_required" | "approved";
  flowId: string;
};

type LocalLoginFetch = (
  input: string | URL | Request,
  init?: RequestInit,
) => Promise<Response>;

type LocalRegistrationFetch = LocalLoginFetch;

type LocalLoginRequestRecord = {
  flowId: string;
  username: string;
  password: string;
  portalBindingDigest: string;
};

type LocalRegistrationRequestRecord = {
  username: string;
  password: string;
  name: string;
  email: string;
  portalBindingDigest: string;
};

type RegistrationProvider = {
  id: string;
  displayName: string;
};

type RegistrationOptions = {
  localIdentity?: { available?: unknown };
  federatedIdentity?: {
    available?: unknown;
    providers?: unknown;
  };
};

type RegistrationFlowState = {
  status?: unknown;
  registration?: RegistrationOptions;
};

type PortalReturnState = {
  status?: unknown;
  returnLocation?: unknown;
};

function localLoginUrl(trellisUrl: string): URL {
  return new URL("/auth/login/local", trellisUrl);
}

function localRegistrationUrl(trellisUrl: string, flowId: string): URL {
  return new URL(
    `/auth/flow/${encodeURIComponent(flowId)}/register/local`,
    trellisUrl,
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isCapabilityMetadata(value: unknown): value is CapabilityMetadata {
  if (!isRecord(value)) return false;
  return (
    typeof value.displayName === "string" &&
    typeof value.description === "string" &&
    (value.consequence === undefined || typeof value.consequence === "string")
  );
}

async function responseErrorCode(response: Response): Promise<string | null> {
  return (await decodeTrellisHttpError(response)).code;
}

export function localLoginErrorMessage(
  status: number,
  errorCode: string | null,
): string {
  if (errorCode === "invalid_credentials") {
    return "Invalid username or password.";
  }
  if (errorCode === "user_inactive") {
    return "This account is inactive. Contact an administrator for access.";
  }
  if (status === 404) {
    return "This sign-in request has expired. Return to the app and start sign-in again.";
  }
  if (status === 400) return "Unable to submit this sign-in request.";
  return "Unable to sign in. Please try again.";
}

export function localRegistrationErrorMessage(
  status: number,
  errorCode: string | null,
): string {
  if (errorCode === "username_taken") {
    return "That username is already in use.";
  }
  if (errorCode === "registration_unavailable") {
    return "Account creation is not available for this sign-in request.";
  }
  if (status === 404) {
    return "This sign-in request has expired. Return to the app and start sign-in again.";
  }
  if (status === 400) return "Unable to create this account.";
  return "Unable to create account. Please try again.";
}

export function isLocalRegistrationAvailable(flowState: unknown): boolean {
  if (!isRecord(flowState) || flowState.status !== "choose_provider") {
    return false;
  }
  const registration = (flowState as RegistrationFlowState).registration;
  return registration?.localIdentity?.available === true;
}

export function federatedRegistrationProviders(
  flowState: unknown,
): RegistrationProvider[] {
  if (!isRecord(flowState) || flowState.status !== "choose_provider") {
    return [];
  }
  const registration = (flowState as RegistrationFlowState).registration;
  const federatedIdentity = registration?.federatedIdentity;
  if (federatedIdentity?.available !== true) return [];
  if (!Array.isArray(federatedIdentity.providers)) return [];
  return federatedIdentity.providers.filter(
    (provider): provider is RegistrationProvider =>
      isRecord(provider) &&
      typeof provider.id === "string" &&
      typeof provider.displayName === "string",
  );
}

export function capabilityEntries(
  capabilities: unknown,
): { key: string; capability: CapabilityMetadata }[] {
  if (!isRecord(capabilities)) return [];
  return Object.entries(capabilities)
    .filter((entry): entry is [string, CapabilityMetadata] =>
      isCapabilityMetadata(entry[1])
    )
    .map(([key, capability]) => ({ key, capability }));
}

export function capabilityMetadata(
  capabilities: unknown,
  key: string,
): CapabilityMetadata | null {
  if (!isRecord(capabilities)) return null;
  const value = capabilities[key];
  return isCapabilityMetadata(value) ? value : null;
}

export function portalCapabilityDisplayName(
  capabilities: unknown,
  key: string,
): string {
  return capabilityMetadata(capabilities, key)?.displayName ??
    GENERIC_MISSING_CAPABILITY_LABEL;
}

export function isFederatedRegistrationAvailable(flowState: unknown): boolean {
  if (!isRecord(flowState) || flowState.status !== "choose_provider") {
    return false;
  }
  const registration = (flowState as RegistrationFlowState).registration;
  return registration?.federatedIdentity?.available === true;
}

export function isFederatedRegistrationProvider(
  flowState: unknown,
  providerId: string,
): boolean {
  return federatedRegistrationProviders(flowState).some((provider) =>
    provider.id === providerId
  );
}

export async function submitLocalLogin(
  trellisUrl: string,
  request: LocalLoginRequestRecord,
  fetchFn: LocalLoginFetch = fetch,
): Promise<LocalLoginSuccess> {
  const response = await fetchFn(localLoginUrl(trellisUrl), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
  });

  if (!response.ok) {
    throw new Error(
      localLoginErrorMessage(
        response.status,
        await responseErrorCode(response),
      ),
    );
  }

  const body: unknown = await response.json();
  if (
    !isRecord(body) ||
    (body.state !== "authenticated" &&
      body.state !== "approval_required" &&
      body.state !== "approved") ||
    body.flowId !== request.flowId
  ) {
    throw new Error("Unable to sign in. Please try again.");
  }

  return { state: body.state, flowId: request.flowId };
}

export async function submitLocalRegistration(
  trellisUrl: string,
  flowId: string,
  request: LocalRegistrationRequestRecord,
  fetchFn: LocalRegistrationFetch = fetch,
): Promise<void> {
  const response = await fetchFn(localRegistrationUrl(trellisUrl, flowId), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
  });

  if (!response.ok) {
    throw new Error(
      localRegistrationErrorMessage(
        response.status,
        await responseErrorCode(response),
      ),
    );
  }
}

export function shouldStayOnPortalCompletionPage(
  currentUrl: URL,
  redirectLocation: string | null,
): boolean {
  return isSamePageLocation(currentUrl, redirectLocation);
}

export function portalReturnLocation(
  flowState: unknown,
  fallbackLocation: string,
): string {
  if (!isRecord(flowState)) return fallbackLocation;
  const { status, returnLocation } = flowState as PortalReturnState;
  if (
    (status === "approval_denied" ||
      status === "insufficient_capabilities" ||
      status === "expired") &&
    typeof returnLocation === "string" &&
    returnLocation.length > 0
  ) {
    return returnLocation;
  }
  return fallbackLocation;
}

export function shouldOfferPortalReturnLink(
  currentUrl: URL,
  returnLocation: string | null | undefined,
): boolean {
  if (!returnLocation) return false;
  return !isSamePageLocation(currentUrl, returnLocation);
}

export function classifyPortalFlowError(
  error: string | null,
  errorCode: string | null = null,
) {
  if (!error && !errorCode) return null;
  if (error === MISSING_PORTAL_FLOW_ID_ERROR) {
    return {
      kind: "recoverable_expired_flow",
      recoverable: true,
      reason: "missing_flow_id",
    };
  }
  return classifyBrowserAuthError(errorCode ? { code: errorCode } : null);
}

export function shouldShowPortalExpiredState(
  error: string | null,
  errorCode: string | null = null,
): boolean {
  const portalClassification = classifyPortalFlowError(error, errorCode);
  return portalClassification?.kind === "recoverable_expired_flow";
}

export function visiblePortalFlowError(
  error: string | null,
  errorCode: string | null = null,
): string | null {
  if (!error) return null;
  const portalClassification = classifyPortalFlowError(error, errorCode);
  return portalClassification?.recoverable ? null : error;
}
