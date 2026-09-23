import { createAuth } from "@oats-center/trellis";
import { machineErrorCode } from "@oats-center/trellis/errors";
import { ulid } from "ulid";
import { generateSessionSeed } from "../control_plane_config.ts";
import type {
  TrellisTestParticipantApplyResult,
  TrellisTestParticipantApproval,
  TrellisTestParticipantLike,
  TrellisTestServiceKey,
} from "../types.ts";
import type {
  AdminRpc,
  AdminRpcInput,
  TrellisTestAdminRpcMethod,
} from "./methods.ts";
import { deploymentConsentRequest } from "./methods.ts";
import { recordTrellisDuration } from "./metrics.ts";

function participantPresentation(participant: TrellisTestParticipantLike) {
  const packageEvidence = participant.packageEvidence as AdminRpcInput<
    "authParticipantsInstall"
  >["packageEvidence"];
  if (typeof packageEvidence?.rootDigest !== "string") {
    throw new Error("Generated participant has invalid package evidence");
  }
  return {
    packageEvidence,
    participantPath: participant.path,
    packageDigest: packageEvidence.rootDigest,
  };
}

export type AdminDeploymentRpc = <M extends TrellisTestAdminRpcMethod>(
  method: M,
  input: AdminRpcInput<M>,
) => Promise<AdminRpc[M]["output"]>;

export type AdminDeploymentContext = {
  defaultDeployment: string;
  createdDeployments: Map<string, Promise<void>>;
  deploymentBindingRevisions: Map<string, bigint>;
  deploymentIds: Map<string, string>;
  pendingApprovals: Map<string, PendingDeploymentApply>;
  installedParticipants: Map<
    string,
    { digest: string; participantId: string; revision: bigint }
  >;
  rpc: AdminDeploymentRpc;
};

/** @internal Applies a deployment, explicitly approving server-computed consent when required. */
export async function applyWithServerConsent<T>(
  rpc: (
    input: AdminRpcInput<"authDeploymentsApply">,
  ) => Promise<T>,
  request: AdminRpcInput<"authDeploymentsApply">,
): Promise<T> {
  try {
    return await rpc(request);
  } catch (error) {
    const consent = deploymentConsentRequest(error);
    if (!consent) throw error;
    const applied = await rpc({
      ...request,
      idempotencyKey: ulid(),
      approval: {
        approvedCapabilities: consent.capabilities.filter((item) =>
          item.eligible
        )
          .map((item) => ({ id: item.id, consentDigest: item.consentDigest })),
        approvedResources: consent.resources.filter((item) => item.eligible)
          .map((item) => ({
            kind: item.kind,
            name: item.name,
            commitment: item.requestedCommitment,
          })),
        companionApproved: consent.companion !== undefined &&
          consent.companion !== null,
        decisionDigest: consent.decisionDigest,
        expectedGrantRevision: consent.expectedGrantRevision,
        installedRevision: consent.installedRevision,
        mode: "capabilities",
      },
    });
    return applied;
  }
}

function deploymentKey(kind: "service" | "device", deployment: string): string {
  return `${kind}:${deployment}`;
}

/** @internal Creates a service or device deployment. */
export async function createDeployment(
  context: AdminDeploymentContext,
  args: {
    deployment?: string;
    kind?: "service" | "device";
    reviewMode?: "none" | "required";
  } = {},
): Promise<void> {
  const deployment = args.deployment ?? context.defaultDeployment;
  const kind = args.kind ?? "service";
  const key = deploymentKey(kind, deployment);
  const existing = context.createdDeployments.get(key);
  if (existing !== undefined) return existing;
  const promise = (async () => {
    const created = await context.rpc(
      "authDeploymentsCreate",
      {
        displayName: deployment,
        expiresAt: null,
        idempotencyKey: ulid(),
        kind,
        participantId: null,
        portalId: null,
        requiresDeviceDelegation: false,
        reviewMode: args.kind === "device"
          ? new TextEncoder().encode(JSON.stringify(args.reviewMode ?? "none"))
          : null,
      },
    );
    context.deploymentIds.set(deployment, created.deployment.deploymentId);
    context.createdDeployments.set(key, Promise.resolve());
  })();
  context.createdDeployments.set(key, promise);
  void promise.catch(() => {
    if (context.createdDeployments.get(key) === promise) {
      context.createdDeployments.delete(key);
    }
  });
  await promise;
}

/** Applies a deployment, explicitly approving server-computed consent when required. */
export async function applyParticipant(
  context: AdminDeploymentContext,
  args: { deployment?: string; contract: TrellisTestParticipantLike },
): Promise<TrellisTestParticipantApproval> {
  const result = await requestParticipantApply(context, args);
  return result.status === "approved"
    ? result.approval
    : await approveParticipantApply(context, result.pendingId);
}

/** Pending deployment consent retained until an explicit approval is supplied. */
export type PendingDeploymentApply = {
  deployment: string;
  evidence: ReturnType<typeof participantPresentation>;
  request: AdminRpcInput<"authDeploymentsApply">;
  consent: NonNullable<ReturnType<typeof deploymentConsentRequest>>;
  startedAt: number;
};

function consentApproval(
  consent: NonNullable<ReturnType<typeof deploymentConsentRequest>>,
): AdminRpcInput<"authDeploymentsApply">["approval"] {
  return {
    approvedCapabilities: consent.capabilities.filter((item) => item.eligible)
      .map((item) => ({ id: item.id, consentDigest: item.consentDigest })),
    approvedResources: consent.resources.filter((item) => item.eligible)
      .map((item) => ({
        kind: item.kind,
        name: item.name,
        commitment: item.requestedCommitment,
      })),
    companionApproved: consent.companion !== undefined &&
      consent.companion !== null,
    decisionDigest: consent.decisionDigest,
    expectedGrantRevision: consent.expectedGrantRevision,
    installedRevision: consent.installedRevision,
    mode: "capabilities",
  };
}

function finalizeParticipantApply(
  context: AdminDeploymentContext,
  pending: Pick<
    PendingDeploymentApply,
    "deployment" | "evidence" | "startedAt"
  >,
  deploymentId: string,
  applied: AdminRpc["authDeploymentsApply"]["output"],
): TrellisTestParticipantApproval {
  context.deploymentBindingRevisions.set(
    deploymentId,
    applied.binding.revision,
  );
  context.installedParticipants.set(pending.evidence.participantPath, {
    digest: pending.evidence.packageDigest,
    participantId: applied.binding.participantId,
    revision: applied.binding.installedRevision,
  });
  recordTrellisDuration(
    "trellis.admin.workflow.duration",
    performance.now() - pending.startedAt,
    {
      deployment: pending.deployment,
      participantId: pending.evidence.participantPath,
      operation: "approve_contract",
      phase: "apply",
    },
  );
  return {
    participantId: applied.binding.participantId,
    installedRevision: applied.binding.installedRevision,
    deploymentId,
    binding: applied.binding,
  };
}

/** Applies a deployment without providing consent; returns the pending approval when required. */
export async function requestParticipantApply(
  context: AdminDeploymentContext,
  args: { deployment?: string; contract: TrellisTestParticipantLike },
): Promise<TrellisTestParticipantApplyResult> {
  const startedAt = performance.now();
  const deployment = args.deployment ?? context.defaultDeployment;
  if (!context.deploymentIds.has(deployment)) {
    await createDeployment(context, { deployment });
  }
  const deploymentId = context.deploymentIds.get(deployment);
  if (!deploymentId) {
    throw new Error(`Trellis deployment '${deployment}' was not created`);
  }
  const evidence = participantPresentation(args.contract);
  let request: AdminRpcInput<"authDeploymentsApply"> = {
    deploymentId,
    ...evidence,
    expectedRevision: context.deploymentBindingRevisions.get(deploymentId) ??
      0n,
    idempotencyKey: ulid(),
    approval: undefined,
  };
  for (;;) {
    try {
      const applied = await context.rpc("authDeploymentsApply", request);
      return {
        status: "approved",
        approval: finalizeParticipantApply(
          context,
          { deployment, evidence, startedAt },
          deploymentId,
          applied,
        ),
      };
    } catch (error) {
      if (
        machineErrorCode(error) === "revision_conflict" &&
        request.expectedRevision < 64n
      ) {
        request = {
          ...request,
          expectedRevision: request.expectedRevision + 1n,
          idempotencyKey: ulid(),
        };
        continue;
      }
      const consent = deploymentConsentRequest(error);
      if (!consent) throw error;
      const pendingId = ulid();
      context.pendingApprovals.set(pendingId, {
        deployment,
        evidence,
        request,
        consent,
        startedAt,
      });
      return { status: "approval_required", pendingId };
    }
  }
}

/** Completes a deployment apply with server-computed consent for a pending approval. */
export async function approveParticipantApply(
  context: AdminDeploymentContext,
  pendingId: string,
): Promise<TrellisTestParticipantApproval> {
  const pending = context.pendingApprovals.get(pendingId);
  if (!pending) {
    throw new Error("Trellis pending deployment approval was not found");
  }
  const deploymentId = context.deploymentIds.get(pending.deployment);
  if (!deploymentId) {
    throw new Error(
      `Trellis deployment '${pending.deployment}' was not created`,
    );
  }
  const applied = await context.rpc("authDeploymentsApply", {
    ...pending.request,
    idempotencyKey: ulid(),
    approval: consentApproval(pending.consent),
  });
  context.pendingApprovals.delete(pendingId);
  return finalizeParticipantApply(context, pending, deploymentId, applied);
}

/** @internal Installs a participant definition without creating a deployment or grants. */
export async function installParticipant(
  context: AdminDeploymentContext,
  args: { contract: TrellisTestParticipantLike },
): Promise<TrellisTestParticipantApproval> {
  const evidence = participantPresentation(args.contract);
  const participantPath = evidence.participantPath;
  const digest = evidence.packageDigest;
  const current = context.installedParticipants.get(participantPath);
  if (current?.digest === digest) {
    return {
      participantId: current.participantId,
      installedRevision: current.revision,
    };
  }
  const installed = await context.rpc(
    "authParticipantsInstall",
    {
      ...evidence,
      expectedRevision: current?.revision ?? 0n,
      idempotencyKey: ulid(),
    },
  );
  const revision = installed.participant.revision;
  context.installedParticipants.set(participantPath, {
    digest,
    participantId: installed.participant.participantId,
    revision,
  });
  return {
    participantId: installed.participant.participantId,
    installedRevision: revision,
  };
}

/** @internal Provisions a service instance key after installing its participant. */
export async function provisionServiceInstance(
  context: AdminDeploymentContext,
  args: { deployment?: string; contract: TrellisTestParticipantLike },
): Promise<TrellisTestServiceKey> {
  const deployment = args.deployment ?? context.defaultDeployment;
  const evidence = participantPresentation(args.contract);
  const deploymentId = context.deploymentIds.get(deployment);
  const installed = context.installedParticipants.get(evidence.participantPath);
  const approved =
    deploymentId && installed?.digest === evidence.packageDigest &&
      context.deploymentBindingRevisions.has(deploymentId)
      ? { deploymentId, participantId: installed.participantId }
      : await applyParticipant(context, args);
  if (!approved.deploymentId) {
    throw new Error("deployment apply returned no deployment ID");
  }
  const identitySeed = generateSessionSeed();
  const auth = await createAuth({ sessionKeySeed: identitySeed });
  const provisioned = await context.rpc(
    "authServiceInstancesProvision",
    {
      deploymentId: approved.deploymentId,
      idempotencyKey: ulid(),
      identityPublicKey: auth.sessionKey,
      instanceId: null,
      participantId: approved.participantId,
    },
  );
  return {
    seed: identitySeed,
    deploymentId: approved.deploymentId,
    instanceId: provisioned.instance.instanceId,
    participantId: approved.participantId,
  };
}

/** @internal Runs the service registration sequence used by test services. */
export function registerService(
  context: AdminDeploymentContext,
  args: { deployment?: string; contract: TrellisTestParticipantLike },
): Promise<TrellisTestServiceKey> {
  return provisionServiceInstance(context, args);
}

/** @internal Provisions a service instance without changing its installed participant. */
export async function provisionServiceInstanceOnly(
  context: AdminDeploymentContext,
  args: { deployment?: string },
): Promise<{ seed: string; sessionKey: string }> {
  const deployment = args.deployment ?? context.defaultDeployment;
  const seed = generateSessionSeed();
  const auth = await createAuth({ sessionKeySeed: seed });
  const deploymentId = context.deploymentIds.get(deployment);
  if (!deploymentId) {
    throw new Error(`Trellis deployment '${deployment}' was not created`);
  }
  await context.rpc(
    "authServiceInstancesProvision",
    {
      deploymentId,
      idempotencyKey: ulid(),
      identityPublicKey: auth.sessionKey,
      instanceId: null,
      participantId: null,
    },
  );
  return { seed, sessionKey: auth.sessionKey };
}
