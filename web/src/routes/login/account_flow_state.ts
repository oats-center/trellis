import { decodeTrellisHttpError } from "@qlever-llc/trellis/auth/browser";

/** Provider entry returned by the account-flow state endpoint. */
export type AccountFlowProvider = {
  id: string;
  displayName: string;
};

/** Password policy surfaced for local-password account flows. */
export type AccountFlowPasswordPolicy = {
  minLength: number;
};

/** Portal-safe target account summary returned for target-bound flows. */
export type AccountFlowTarget = {
  userId: string;
  name?: string;
  email?: string;
  active: boolean;
};

/** Supported account-flow kinds surfaced to the built-in portal pages. */
export type AccountFlowKind =
  | "admin_account"
  | "identity_link"
  | "password_reset";

/** Active account-flow state returned by the backend. */
export type ActiveAccountFlowState = {
  status: "active";
  flowId: string;
  kind: AccountFlowKind | string;
  mode?: "create" | "edit";
  username?: string;
  targetUserId?: string;
  allowedProviders: string[] | null;
  profileHint: Record<string, unknown> | null;
  expiresAt: string;
  passwordPolicy?: AccountFlowPasswordPolicy;
  providers: AccountFlowProvider[];
  target?: AccountFlowTarget;
  returnTo?: string;
};

/** Expired or missing account-flow state. */
export type ExpiredAccountFlowState = {
  status: "expired";
  kind?: AccountFlowKind | string;
  targetUserId?: string;
};

/** Already-consumed account-flow state. */
export type ConsumedAccountFlowState = {
  status: "consumed";
  kind: AccountFlowKind | string;
  targetUserId?: string;
  returnTo?: string;
};

/** Account-flow state variants rendered by the portal pages. */
export type AccountFlowState =
  | ActiveAccountFlowState
  | ExpiredAccountFlowState
  | ConsumedAccountFlowState;

/** Local-password completion form values. */
export type LocalPasswordInput = {
  username?: string;
  password: string;
  name: string;
  email: string;
  browserFlowId?: string;
};

/** Successful local-password completion response. */
export type LocalPasswordSuccess = {
  status: "created";
  userId: string;
  returnTo?: string;
  browserFlowId?: string;
};

/** OAuth/OIDC callback query state for a completed account flow. */
export type AccountFlowOAuthCompletion = {
  status: "completed";
  flowId: string;
  userId: string;
  returnTo?: string;
};

/** Minimal fetch-compatible function used by account-flow helpers. */
export type FetchLike = (
  input: string | URL | Request,
  init?: RequestInit,
) => Promise<Response>;

export const ACCOUNT_FLOW_ERROR_MESSAGES: Record<string, string> = {
  flow_not_found: "This account request was not found. Ask for a new link.",
  flow_expired: "This account request has expired. Ask for a new link.",
  flow_already_consumed: "This account request has already been used.",
  flow_consume_conflict:
    "This account request was completed elsewhere. Refresh before trying again.",
  local_identity_exists:
    "That username is already in use. Choose a different username.",
  local_username_mismatch:
    "This password link is bound to a different local username. Ask for a new link.",
  target_user_not_found:
    "The target account for this request no longer exists.",
  target_user_inactive:
    "The target account is inactive. Contact an administrator for access.",
  local_provider_not_allowed:
    "Username and password completion is not allowed for this request.",
  flow_missing_target_user:
    "This account request is missing its target account.",
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseProvider(value: unknown): AccountFlowProvider | null {
  if (!isRecord(value)) return null;
  if (typeof value.id !== "string" || typeof value.displayName !== "string") {
    return null;
  }
  return { id: value.id, displayName: value.displayName };
}

function parseTarget(value: unknown): AccountFlowTarget | undefined {
  if (!isRecord(value)) return undefined;
  if (typeof value.userId !== "string" || typeof value.active !== "boolean") {
    return undefined;
  }
  return {
    userId: value.userId,
    ...(typeof value.name === "string" ? { name: value.name } : {}),
    ...(typeof value.email === "string" ? { email: value.email } : {}),
    active: value.active,
  };
}

function parseProviders(value: unknown): AccountFlowProvider[] {
  if (!Array.isArray(value)) return [];
  return value.map(parseProvider).filter((provider) => provider !== null);
}

function parsePasswordPolicy(
  value: unknown,
): AccountFlowPasswordPolicy | undefined {
  if (!isRecord(value)) return undefined;
  return typeof value.minLength === "number" &&
      Number.isInteger(value.minLength) &&
      value.minLength > 0
    ? { minLength: value.minLength }
    : undefined;
}

function parseAllowedProviders(value: unknown): string[] | null {
  if (value === null) return null;
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === "string");
}

/** Return a safe same-origin relative return target, if present. */
export function safeRelativeReturnTo(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  if (!trimmed.startsWith("/") || trimmed.startsWith("//")) return undefined;
  return trimmed;
}

function parseJsonRecord(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : {};
}

async function parseJson(response: Response): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    return null;
  }
}

/** Extract the account flow id from a portal URL. */
export function accountFlowIdFromUrl(url: URL): string | null {
  const flowId = url.searchParams.get("flowId")?.trim();
  return flowId && flowId.length > 0 ? flowId : null;
}

/** Build the backend URL that starts OAuth/OIDC login for an account flow. */
export function accountFlowProviderLoginUrl(
  trellisUrl: string,
  flowId: string,
  providerId: string,
  continuation?: {
    browserFlowId: string;
    portalBindingDigest: string;
  },
): string {
  const url = new URL(
    `/auth/account-flow/${encodeURIComponent(flowId)}/login/${
      encodeURIComponent(providerId)
    }`,
    trellisUrl,
  );
  if (continuation) {
    url.searchParams.set("browserFlowId", continuation.browserFlowId);
    url.searchParams.set(
      "portalBindingDigest",
      continuation.portalBindingDigest,
    );
  }
  return url.toString();
}

/** Parse account-flow OAuth/OIDC completion query parameters from a portal URL. */
export function parseAccountFlowOAuthCompletion(
  url: URL,
): AccountFlowOAuthCompletion | null {
  const flowId = accountFlowIdFromUrl(url);
  const status = url.searchParams.get("status")?.trim();
  const userId = url.searchParams.get("userId")?.trim();
  const returnTo = safeRelativeReturnTo(url.searchParams.get("returnTo"));

  if (flowId && status === "completed" && userId && userId.length > 0) {
    return {
      status: "completed",
      flowId,
      userId,
      ...(returnTo ? { returnTo } : {}),
    };
  }

  return null;
}

/** Return a string profile hint value if it was included by the backend. */
export function profileHintString(
  profileHint: Record<string, unknown> | null | undefined,
  key: string,
): string {
  if (!profileHint) return "";
  const value = profileHint[key];
  return typeof value === "string" ? value : "";
}

/** Prefer target profile values over profile hints for local account defaults. */
export function defaultProfileValue(
  state: ActiveAccountFlowState,
  key: "username" | "name" | "email",
): string {
  if (key === "username" && state.username) return state.username;
  const targetValue = key === "username" ? undefined : state.target?.[key];
  if (targetValue) return targetValue;
  return profileHintString(state.profileHint, key);
}

/** Whether the active flow offers built-in local username/password completion. */
export function hasLocalProvider(state: ActiveAccountFlowState): boolean {
  return state.providers.some((provider) => provider.id === "local");
}

/** Providers that require OAuth/OIDC account-flow completion. */
export function unavailableProviders(
  state: ActiveAccountFlowState,
): AccountFlowProvider[] {
  return state.providers.filter((provider) => provider.id !== "local");
}

/** Human-friendly label for known flow kinds. */
export function flowKindLabel(kind: string): string {
  switch (kind) {
    case "identity_link":
      return "account link";
    case "local_password_reset":
      return "password reset";
    case "admin_account":
      return "administrator account";
    default:
      return kind.replaceAll("_", " ");
  }
}

/** Parse account-flow state from the backend's portal-safe JSON shape. */
export function parseAccountFlowState(value: unknown): AccountFlowState {
  const body = parseJsonRecord(value);
  if (body.status === "expired") {
    return {
      status: "expired",
      ...(typeof body.kind === "string" ? { kind: body.kind } : {}),
      ...(typeof body.targetUserId === "string"
        ? { targetUserId: body.targetUserId }
        : {}),
      ...(safeRelativeReturnTo(body.returnTo)
        ? { returnTo: safeRelativeReturnTo(body.returnTo) }
        : {}),
    };
  }

  if (body.status === "consumed" && typeof body.kind === "string") {
    return {
      status: "consumed",
      kind: body.kind,
      ...(body.mode === "create" || body.mode === "edit"
        ? { mode: body.mode }
        : {}),
      ...(typeof body.username === "string" ? { username: body.username } : {}),
      ...(typeof body.targetUserId === "string"
        ? { targetUserId: body.targetUserId }
        : {}),
    };
  }

  if (
    (body.status === "active" || body.status === "pending") &&
    typeof body.flowId === "string" &&
    typeof body.kind === "string" &&
    (typeof body.expiresAt === "string" ||
      (typeof body.expiresAt === "number" &&
        Number.isSafeInteger(body.expiresAt)))
  ) {
    const target = parseTarget(body.target);
    const passwordPolicy = parsePasswordPolicy(body.passwordPolicy);
    const allowedProviders = parseAllowedProviders(body.allowedProviders);
    return {
      status: "active",
      flowId: body.flowId,
      kind: body.kind,
      ...(body.mode === "create" || body.mode === "edit"
        ? { mode: body.mode }
        : {}),
      ...(typeof body.username === "string" ? { username: body.username } : {}),
      ...(typeof body.targetUserId === "string"
        ? { targetUserId: body.targetUserId }
        : {}),
      allowedProviders,
      profileHint: isRecord(body.profileHint) ? body.profileHint : null,
      expiresAt: typeof body.expiresAt === "number"
        ? new Date(body.expiresAt).toISOString()
        : body.expiresAt,
      ...(passwordPolicy ? { passwordPolicy } : {}),
      providers: Array.isArray(body.providers)
        ? parseProviders(body.providers)
        : (allowedProviders ?? []).map((id) => ({ id, displayName: id })),
      ...(target ? { target } : {}),
      ...(safeRelativeReturnTo(body.returnTo)
        ? { returnTo: safeRelativeReturnTo(body.returnTo) }
        : {}),
    };
  }

  throw new Error("The account request returned an unexpected response.");
}

/** Convert backend account-flow errors into user-facing messages. */
export function formatAccountFlowError(
  status: number,
  error: string | null,
): string {
  if (error === "local_password_too_short") {
    return "Choose a longer password.";
  }
  if (error && error in ACCOUNT_FLOW_ERROR_MESSAGES) {
    return ACCOUNT_FLOW_ERROR_MESSAGES[error];
  }
  if (error) return `Account request failed (${status}): ${error}`;
  return `Account request failed with status ${status}.`;
}

/** Load account-flow state from the Trellis auth endpoint. */
export async function loadAccountFlowState(
  trellisUrl: string,
  flowId: string,
  fetcher: FetchLike = fetch,
): Promise<AccountFlowState> {
  const response = await fetcher(
    new URL(`/auth/account-flow/${encodeURIComponent(flowId)}`, trellisUrl),
  );
  if (!response.ok) {
    const error = await decodeTrellisHttpError(response);
    throw new Error(formatAccountFlowError(error.status, error.code));
  }
  const body = await parseJson(response);
  return parseAccountFlowState(body);
}

/** Complete an account flow through the local-password endpoint. */
export async function completeAccountFlowLocalPassword(
  trellisUrl: string,
  flowId: string,
  input: LocalPasswordInput,
  fetcher: FetchLike = fetch,
): Promise<LocalPasswordSuccess> {
  const payload: Record<string, string> = {
    password: input.password,
  };
  const username = input.username?.trim();
  const name = input.name.trim();
  const email = input.email.trim();
  if (username) payload.username = username;
  if (name) payload.name = name;
  if (email) payload.email = email;
  if (input.browserFlowId) payload.browserFlowId = input.browserFlowId;

  const response = await fetcher(
    new URL(
      `/auth/account-flow/${encodeURIComponent(flowId)}/local-password`,
      trellisUrl,
    ),
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(payload),
    },
  );
  if (!response.ok) {
    const error = await decodeTrellisHttpError(response);
    throw new Error(formatAccountFlowError(error.status, error.code));
  }
  const body = await parseJson(response);
  if (
    isRecord(body) &&
    (body.status === "created" || body.status === "updated") &&
    typeof body.userId === "string"
  ) {
    const returnTo = safeRelativeReturnTo(body.returnTo);
    const browserFlowId = typeof body.browserFlowId === "string"
      ? body.browserFlowId
      : undefined;
    return {
      status: "created",
      userId: body.userId,
      ...(returnTo ? { returnTo } : {}),
      ...(browserFlowId ? { browserFlowId } : {}),
    };
  }
  throw new Error(
    "Account request completed but returned an unexpected response.",
  );
}
