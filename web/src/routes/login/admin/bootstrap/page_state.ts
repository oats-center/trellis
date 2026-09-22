import { decodeTrellisHttpError } from "@qlever-llc/trellis/auth/browser";

import {
  accountFlowProviderLoginUrl,
  type AccountFlowState,
  type ActiveAccountFlowState,
  hasLocalProvider,
  loadAccountFlowState,
  parseAccountFlowOAuthCompletion,
  unavailableProviders,
} from "../../account_flow_state.ts";

/** Form values accepted by the local-password admin bootstrap endpoint. */
export type AdminBootstrapInput = {
  username: string;
  password: string;
  name: string;
  email: string;
  browserFlowId?: string;
  portalBindingDigest?: string;
};

/** Successful admin bootstrap completion response. */
export type AdminBootstrapSuccess = {
  status: "created" | "updated";
  userId: string;
  browserFlowId?: string;
};

/** Backend error details used for user-facing bootstrap messages. */
export type BootstrapErrorDetails = {
  status: number;
  error: string | null;
  message?: string | null;
};

/** Minimal fetch-compatible function used by the completion helper. */
export type FetchLike = (
  input: string | URL | Request,
  init?: RequestInit,
) => Promise<Response>;

export {
  accountFlowProviderLoginUrl,
  hasLocalProvider,
  loadAccountFlowState,
  parseAccountFlowOAuthCompletion,
  unavailableProviders,
};
export type { AccountFlowState, ActiveAccountFlowState };

const KNOWN_ERROR_MESSAGES: Record<string, string> = {
  flow_not_found:
    "This bootstrap request was not found. Start bootstrap again.",
  flow_expired: "This bootstrap request has expired. Start bootstrap again.",
  flow_already_consumed: "This bootstrap request has already been used.",
  admin_already_exists:
    "An admin account already exists for this Trellis instance.",
  local_identity_exists:
    "That username is already in use. Choose a different username.",
  flow_wrong_kind: "This request cannot create an admin account.",
  flow_missing_admin_capability:
    "This request is missing permission to create the first admin account.",
  flow_consume_conflict:
    "This bootstrap request was completed elsewhere. Refresh and check the admin account.",
  password_unchanged: "New password must differ from the current password.",
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function successBody(value: unknown): AdminBootstrapSuccess | null {
  if (!isRecord(value)) return null;
  if (value.status !== "created" && value.status !== "updated") return null;
  if (typeof value.userId !== "string") return null;
  return {
    status: value.status,
    userId: value.userId,
    ...(typeof value.browserFlowId === "string"
      ? { browserFlowId: value.browserFlowId }
      : {}),
  };
}

async function parseJson(response: Response): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    return null;
  }
}

/** Extract the bootstrap flow id from a portal URL. */
export function adminBootstrapFlowId(url: URL): string | null {
  const flowId = url.searchParams.get("flowId")?.trim();
  return flowId && flowId.length > 0 ? flowId : null;
}

/** Convert backend bootstrap errors into concise user-facing messages. */
export function formatAdminBootstrapError(
  details: BootstrapErrorDetails,
): string {
  if (details.message) return details.message;
  if (details.error && details.error in KNOWN_ERROR_MESSAGES) {
    return KNOWN_ERROR_MESSAGES[details.error];
  }

  if (details.error) {
    return `Bootstrap failed (${details.status}): ${details.error}`;
  }

  return `Bootstrap failed with status ${details.status}.`;
}

/** Complete a local-password admin bootstrap flow against the Trellis auth endpoint. */
export async function completeAdminBootstrap(
  trellisUrl: string,
  flowId: string,
  input: AdminBootstrapInput,
  fetcher: FetchLike = fetch,
): Promise<AdminBootstrapSuccess> {
  const url = new URL(
    `/auth/account-flow/${encodeURIComponent(flowId)}/local-password`,
    trellisUrl,
  );
  const payload: Record<string, string | null> = {
    username: input.username,
    password: input.password,
    name: input.name.trim() || null,
    email: input.email.trim() || null,
  };
  if (input.browserFlowId) payload.browserFlowId = input.browserFlowId;
  if (input.portalBindingDigest) {
    payload.portalBindingDigest = input.portalBindingDigest;
  }

  const response = await fetcher(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (!response.ok) {
    const error = await decodeTrellisHttpError(response);
    throw new Error(
      formatAdminBootstrapError({ status: error.status, error: error.code }),
    );
  }

  const success = successBody(await parseJson(response));
  if (success) return success;
  throw new Error(
    "Bootstrap completed but the server returned an unexpected response.",
  );
}
