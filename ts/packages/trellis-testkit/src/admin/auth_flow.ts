import type { ClientAuthContinuation } from "@oatscenter/trellis";
import {
  createPortalBinding,
  fetchPortalFlowState,
  type PortalBinding,
  type PortalFlowState,
  startPortalTransaction,
  submitPortalApproval,
} from "@oatscenter/trellis/auth/browser";

import { ADMIN_USERNAME } from "./methods.ts";
import { recordTrellisDuration } from "./metrics.ts";
import { postJson } from "./transport.ts";

export function intentFromUrl(url: string): string {
  const intent = new URL(url).searchParams.get("intent");
  if (!intent) throw new Error(`Trellis auth URL is missing intent: ${url}`);
  return intent;
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
      transactionId: args.flowId,
      username: ADMIN_USERNAME,
      password: args.password,
      portalBindingDigest: binding.digest,
    }, { "trellis-portal-binding": binding.secret });
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
  const intent = intentFromUrl(args.loginUrl);
  const portalBinding = await createPortalBinding();
  const flowId = await startPortalTransaction(
    { authUrl: args.trellisUrl, portalOrigin: new URL(args.trellisUrl).origin },
    intent,
    portalBinding,
  );
  const binding = await performLocalLogin({
    trellisUrl: args.trellisUrl,
    flowId,
    password: args.password,
    binding: portalBinding,
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
  return { status: "bound", transactionId: flowId };
}
