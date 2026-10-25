import { createAuth } from "@oatscenter/trellis";
import type { codecs } from "@oatscenter/trellis/generated";
import { ulid } from "ulid";
import { generateSessionSeed } from "../auth/random.ts";
import type {
  TrellisTestParticipantApproval,
  TrellisTestParticipantLike,
  TrellisTestServiceKey,
} from "../types.ts";
import type {
  AdminRpc,
  AdminRpcInput,
  TrellisTestAdminRpcMethod,
} from "./methods.ts";
import { types } from "../../trellis/index.js";

/** Generated administrative calls used by deployment automation. */
export type AdminDeploymentRpc = <M extends TrellisTestAdminRpcMethod>(
  method: M,
  input: AdminRpcInput<M>,
) => Promise<AdminRpc[M]["output"]>;

/** Per-runtime deployment identities and accepted source evidence. */
export type AdminDeploymentContext = {
  defaultDeployment: string;
  /** The participant each created deployment runs, by deployment key. */
  deploymentParticipants: Map<string, string>;
  deploymentIds: Map<string, ReturnType<typeof codecs.ulid.decode>>;
  deploymentRevisions: Map<string, bigint>;
  deploymentDigests: Map<string, string>;
  acceptedApis: Map<string, string>;
  rpc: AdminDeploymentRpc;
};

/**
 * The deployment a call targets: the named one, else the runtime default, unless
 * the default already runs a different participant. Then the participant gets
 * its own deployment, so registering a second service never re-applies the
 * first one's deployment.
 */
function deploymentKey(
  context: AdminDeploymentContext,
  args: { deployment?: string; contract: TrellisTestParticipantLike },
): string {
  if (args.deployment) return args.deployment;
  const running = context.deploymentParticipants.get(context.defaultDeployment);
  return running === undefined || running === args.contract.identity
    ? context.defaultDeployment
    : args.contract.identity;
}

/** Accepts the generated participant's APIs through ordinary catalog review. */
export async function installParticipant(
  context: AdminDeploymentContext,
  args: { contract: TrellisTestParticipantLike },
): Promise<TrellisTestParticipantApproval> {
  const evidence = types.AuthPackageEvidenceCodec.decode(
    args.contract.packageEvidence,
  );
  for (const api of [
    ...args.contract.implements,
    ...args.contract.uses.map((selection) => selection.api),
  ]) {
    if (context.acceptedApis.get(api.identity) === evidence.rootDigest)
      continue;
    const { review } = await context.rpc("authApisReview", {
      apiId: api.identity,
      packageEvidence: evidence,
    });
    if (!review.compatible) {
      throw new Error(
        `API ${api.identity} requires explicit force replacement`,
      );
    }
    await context.rpc("authApisAccept", {
      reviewId: review.reviewId,
      canonicalDigest: review.canonicalDigest,
      expectedGeneration: review.currentGeneration ?? 0n,
      expectedAcceptedRevision: review.currentAcceptedRevision ?? 0n,
      mappingEdits: [],
      roleEdits: [],
      idempotencyKey: ulid(),
    });
    context.acceptedApis.set(api.identity, evidence.rootDigest);
  }
  return { participantId: args.contract.identity };
}

/** Creates a deployment bound to an actual generated service or device descriptor. */
export async function createDeployment(
  context: AdminDeploymentContext,
  args: { deployment?: string; contract: TrellisTestParticipantLike },
): Promise<void> {
  const deployment = deploymentKey(context, args);
  if (context.deploymentIds.has(deployment)) return;
  await installParticipant(context, args);
  const { deployment: created } = await context.rpc("authDeploymentsCreate", {
    title: deployment,
    kind: args.contract.kind,
    participantId: args.contract.identity,
    packageEvidence: types.AuthPackageEvidenceCodec.decode(
      args.contract.packageEvidence,
    ),
    idempotencyKey: ulid(),
  });
  context.deploymentIds.set(deployment, created.deploymentId);
  context.deploymentParticipants.set(deployment, args.contract.identity);
  context.deploymentRevisions.set(created.deploymentId, created.revision);
  context.deploymentDigests.set(
    created.deploymentId,
    types.AuthPackageEvidenceCodec.decode(args.contract.packageEvidence)
      .rootDigest,
  );
}

/** Applies current generated source evidence with the last observed deployment revision. */
export async function applyParticipant(
  context: AdminDeploymentContext,
  args: { deployment?: string; contract: TrellisTestParticipantLike },
): Promise<TrellisTestParticipantApproval> {
  const deployment = deploymentKey(context, args);
  const existing = context.deploymentIds.get(deployment);
  await installParticipant(context, args);
  if (!existing) await createDeployment(context, { ...args, deployment });
  const deploymentId = context.deploymentIds.get(deployment);
  if (!deploymentId)
    throw new Error(`Deployment ${deployment} was not created`);
  if (existing) {
    const expectedRevision = context.deploymentRevisions.get(deploymentId);
    if (expectedRevision === undefined)
      throw new Error(`Unknown deployment revision ${deploymentId}`);
    const { deployment: applied } = await context.rpc("authDeploymentsApply", {
      deploymentId,
      expectedRevision,
      packageEvidence: types.AuthPackageEvidenceCodec.decode(
        args.contract.packageEvidence,
      ),
      idempotencyKey: ulid(),
    });
    context.deploymentRevisions.set(deploymentId, applied.revision);
    context.deploymentDigests.set(
      deploymentId,
      types.AuthPackageEvidenceCodec.decode(args.contract.packageEvidence)
        .rootDigest,
    );
  }
  return {
    participantId: args.contract.identity,
    deploymentId,
    revision: context.deploymentRevisions.get(deploymentId),
  };
}

/** Provisions an independently keyed service instance through generated Auth. */
export async function provisionServiceInstance(
  context: AdminDeploymentContext,
  args: { deployment?: string; contract: TrellisTestParticipantLike },
): Promise<TrellisTestServiceKey> {
  const deployment = deploymentKey(context, args);
  const deploymentId = context.deploymentIds.get(deployment);
  const digest = types.AuthPackageEvidenceCodec.decode(
    args.contract.packageEvidence,
  ).rootDigest;
  const approved =
    deploymentId && context.deploymentDigests.get(deploymentId) === digest
      ? { deploymentId, participantId: args.contract.identity }
      : await applyParticipant(context, { ...args, deployment });
  if (!approved.deploymentId) throw new Error("Deployment has no identity");
  const identity = await provisionServiceInstanceOnly(context, { deployment });
  return {
    ...identity,
    deploymentId: approved.deploymentId,
    participantId: approved.participantId,
  };
}

/** Registers the generated provider and provisions its ordinary runtime identity. */
export const registerService = provisionServiceInstance;

/** Provisions a key for an existing deployment without changing its source or selection. */
export async function provisionServiceInstanceOnly(
  context: AdminDeploymentContext,
  args: { deployment?: string },
): Promise<{ seed: string; sessionKey: string; instanceId: string }> {
  const deployment = args.deployment ?? context.defaultDeployment;
  const deploymentId = context.deploymentIds.get(deployment);
  if (!deploymentId)
    throw new Error(`Deployment ${deployment} was not created`);
  const seed = generateSessionSeed();
  const auth = await createAuth({ sessionKeySeed: seed });
  const { instance } = await context.rpc("authServiceInstancesProvision", {
    deploymentId,
    identityPublicKey: auth.sessionKey,
    title: "service",
    idempotencyKey: ulid(),
  });
  return { seed, sessionKey: auth.sessionKey, instanceId: instance.instanceId };
}
