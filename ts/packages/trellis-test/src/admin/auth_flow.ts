import type { ClientAuthContinuation } from "@oatscenter/trellis";
import {
  createPortalBinding,
  fetchPortalFlowState,
  type PortalBinding,
  type PortalFlowState,
  submitPortalApproval,
} from "@oatscenter/trellis/auth/browser";

import { ADMIN_USERNAME } from "./methods.ts";
import { recordTrellisDuration } from "./metrics.ts";
import { postJson } from "./transport.ts";

export function flowIdFromUrl(url: string): string {
  const flowId = new URL(url).searchParams.get("flowId");
  if (!flowId) throw new Error(`Trellis auth URL is missing flowId: ${url}`);
  return flowId;
}

export function adminAccountTokenFromUrl(url: string): string {
  const token = new URL(url).searchParams.get("adminAccountToken");
  if (!token) {
    throw new Error(
      `Trellis administrator URL is missing adminAccountToken: ${url}`,
    );
  }
  return token;
}

export async function performLocalLogin(args: {
  trellisUrl: string;
  flowId: string;
  password: string;
  binding?: PortalBinding;
}): Promise<PortalBinding> {
  const binding = args.binding ?? await createPortalBinding();
  const startedAt = performance.now();
  try {
    await postJson(`${args.trellisUrl}/auth/login/local`, {
      flowId: args.flowId,
      username: ADMIN_USERNAME,
      password: args.password,
      portalBindingDigest: binding.digest,
    });
  } finally {
    recordTrellisDuration(
      "trellis.auth.flow.duration",
      performance.now() - startedAt,
      { phase: "local_login", authFlow: "local" },
    );
  }
  return binding;
}

export async function approveLocalFlowIfNeeded(args: {
  trellisUrl: string;
  flowId: string;
  binding: PortalBinding;
  prepareApproval?: (
    state: Extract<PortalFlowState, { status: "approval_required" }>,
  ) => Promise<void>;
}): Promise<void> {
  const startedAt = performance.now();
  const initialFetchStartedAt = performance.now();
  const state = await fetchPortalFlowState(
    {
      authUrl: args.trellisUrl,
      portalOrigin: new URL(args.trellisUrl).origin,
    },
    args.flowId,
    args.binding,
  );
  recordTrellisDuration(
    "trellis.auth.flow.duration",
    performance.now() - initialFetchStartedAt,
    { phase: "approval_fetch" },
  );
  if (state.status === "redirect") {
    recordTrellisDuration(
      "trellis.auth.flow.duration",
      performance.now() - startedAt,
      { phase: "total" },
    );
    return;
  }
  if (state.status === "approval_required") {
    await args.prepareApproval?.(state);
    const approvalStartedAt = performance.now();
    const approved = await submitPortalApproval(
      {
        authUrl: args.trellisUrl,
        portalOrigin: new URL(args.trellisUrl).origin,
      },
      args.flowId,
      args.binding,
      "approved",
    );
    recordTrellisDuration(
      "trellis.auth.flow.duration",
      performance.now() - approvalStartedAt,
      { phase: "approval_submit" },
    );
    if (approved.status === "redirect") {
      recordTrellisDuration(
        "trellis.auth.flow.duration",
        performance.now() - startedAt,
        { phase: "total" },
      );
      return;
    }
    throw new Error(
      `Trellis auth approval did not complete; portal state is '${approved.status}'`,
    );
  }
  throw new Error(
    `Trellis local login did not reach approval; portal state is '${state.status}'`,
  );
}

export async function completeLocalAuthFlow(args: {
  trellisUrl: string;
  loginUrl: string;
  password: string;
}): Promise<ClientAuthContinuation> {
  const startedAt = performance.now();
  const flowId = flowIdFromUrl(args.loginUrl);
  const binding = await performLocalLogin({
    trellisUrl: args.trellisUrl,
    flowId,
    password: args.password,
  });
  await approveLocalFlowIfNeeded({
    trellisUrl: args.trellisUrl,
    flowId,
    binding,
  });
  recordTrellisDuration(
    "trellis.auth.flow.duration",
    performance.now() - startedAt,
    { phase: "total" },
  );
  return { status: "bound", flowId };
}
