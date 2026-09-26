import {
  type ClientAuthContinuation,
  type ClientAuthRequiredContext,
  TrellisClient,
} from "@oatscenter/trellis";

import {
  adminAccountTokenFromUrl,
  approveLocalFlowIfNeeded,
  completeLocalAuthFlow,
  flowIdFromUrl,
  performLocalLogin,
} from "./admin/auth_flow.ts";
import * as adminDeployment from "./admin/deployment.ts";
import {
  ADMIN_USERNAME,
  type AdminClient,
  adminMethods,
  adminParticipant,
  type AdminRpc,
  type AdminRpcInput,
  type TrellisTestAdminRpcMethod,
} from "./admin/methods.ts";
import { recordTrellisDuration } from "./admin/metrics.ts";
import { isRecord, postJson } from "./admin/transport.ts";
import { generateSessionSeed } from "./control_plane_config.ts";
import type {
  TrellisTestParticipantApplyResult,
  TrellisTestParticipantApproval,
  TrellisTestParticipantLike,
  TrellisTestServiceKey,
} from "./types.ts";

export { adminMethods, type TrellisTestAdminRpcMethod };

/** Internal public-surface admin automation used by `TrellisTestRuntime`. */
export class TrellisTestAdminAutomation {
  #configuredConsentPolicies = new Set<string>();
  readonly #trellisUrl: string;
  readonly #adminPassword: string;
  readonly #getBootstrapUrl: () => Promise<string>;
  #bootstrapComplete: Promise<void> | undefined;
  #adminClient: Promise<AdminClient> | undefined;
  #connectedAdminClient: AdminClient | undefined;
  readonly #deployment: adminDeployment.AdminDeploymentContext;

  /** Creates admin automation backed by the supplied bootstrap URL provider. */
  constructor(args: {
    trellisUrl: string;
    adminPassword: string;
    defaultDeployment: string;
    getBootstrapUrl: () => Promise<string>;
    bootstrapComplete?: boolean;
    resourceReadyTimeoutMs?: number;
  }) {
    this.#trellisUrl = args.trellisUrl.replace(/\/$/, "");
    this.#adminPassword = args.adminPassword;
    this.#getBootstrapUrl = args.getBootstrapUrl;
    this.#deployment = {
      defaultDeployment: args.defaultDeployment,
      createdDeployments: new Map(),
      deploymentBindingRevisions: new Map(),
      deploymentIds: new Map(),
      pendingApprovals: new Map(),
      installedParticipants: new Map(),
      deploymentResourceExpectations: new Map(),
      resourceReadyTimeoutMs: args.resourceReadyTimeoutMs ?? 30_000,
      rpc: <M extends TrellisTestAdminRpcMethod>(
        method: M,
        input: AdminRpc[M]["input"],
      ) => this.#rpc(method, input),
    };
    if (args.bootstrapComplete === true) {
      this.#bootstrapComplete = Promise.resolve();
    }
  }

  /** Completes the one-time first-administrator bootstrap through the real account-flow endpoint. */
  async completeBootstrap(): Promise<void> {
    this.#bootstrapComplete ??= (async () => {
      const startedAt = performance.now();
      try {
        const bootstrapUrl = await this.#getBootstrapUrl();
        const flowId = adminAccountTokenFromUrl(bootstrapUrl);
        const response = await postJson(
          `${this.#trellisUrl}/auth/account-flow/${
            encodeURIComponent(flowId)
          }/local-password`,
          { username: ADMIN_USERNAME, password: this.#adminPassword },
        );
        if (!isRecord(response) || response.status !== "created") {
          throw new Error(
            "Trellis first-admin bootstrap returned an unexpected response",
          );
        }
      } finally {
        recordTrellisDuration(
          "trellis.admin.workflow.duration",
          performance.now() - startedAt,
          { operation: "complete_bootstrap", phase: "total" },
        );
      }
    })();
    await this.#bootstrapComplete;
  }

  async #client(): Promise<AdminClient> {
    this.#adminClient ??= (async () => {
      const startedAt = performance.now();
      try {
        await this.completeBootstrap();
        const sessionKeySeed = generateSessionSeed();
        const client = await TrellisClient.connect({
          trellisUrl: this.#trellisUrl,
          name: "trellis-testkit-admin",
          timeout: 60_000,
          participant: adminParticipant,
          auth: {
            mode: "session_key",
            sessionKeySeed,
            redirectTo: `${this.#trellisUrl}/_trellis/test/admin-auth`,
          },
          onAuthRequired: (ctx: ClientAuthRequiredContext) =>
            completeLocalAuthFlow({
              trellisUrl: this.#trellisUrl,
              loginUrl: ctx.loginUrl,
              password: this.#adminPassword,
            }),
        }).orThrow();
        this.#connectedAdminClient = client;
        return client;
      } finally {
        recordTrellisDuration(
          "trellis.admin.workflow.duration",
          performance.now() - startedAt,
          { operation: "register_service", phase: "connect" },
        );
      }
    })();
    return await this.#adminClient;
  }

  /** Forwards one validated low-level Auth RPC over the local or shared admin transport. */
  async callAdminRpc(
    method: string,
    input: unknown,
  ): Promise<unknown> {
    if (!Object.hasOwn(adminMethods, method)) {
      throw new Error(`unsupported Trellis test admin RPC ${method}`);
    }
    const rpcMethod = method as TrellisTestAdminRpcMethod;
    const descriptor = adminMethods[rpcMethod];
    return await descriptor.call(await this.#client(), input);
  }

  async #rpc<M extends TrellisTestAdminRpcMethod>(
    method: M,
    input: AdminRpc[M]["input"],
  ): Promise<AdminRpc[M]["output"]> {
    return await this.callAdminRpc(method, input) as AdminRpc[M]["output"];
  }

  /** Creates a service deployment through `Auth.Deployments.Create`. */
  async createDeployment(args: {
    deployment?: string;
    kind?: "service" | "device";
    reviewMode?: "none" | "required";
  } = {}): Promise<void> {
    return await adminDeployment.createDeployment(this.#deployment, args);
  }

  async provisionDevice(
    input: import("../trellis/index.js").apis.auth.DevicesProvisionInput,
  ): Promise<
    import("../trellis/index.js").apis.auth.DevicesProvisionOutput
  > {
    return await this.#rpc(
      "authDevicesProvision",
      {
        ...input,
        deploymentId: this.#deployment.deploymentIds.get(input.deploymentId) ??
          input.deploymentId,
      },
    );
  }

  async stateResourcesInspect(
    input: import("../trellis/index.js").apis.state.ResourcesInspectInput,
  ): Promise<import("../trellis/index.js").apis.state.ResourcesInspectOutput> {
    return await this.#rpc("stateResourcesInspect", input);
  }

  async stateResourcesQuery(
    input: import("../trellis/index.js").apis.state.ResourcesQueryInput,
  ): Promise<import("../trellis/index.js").apis.state.ResourcesQueryOutput> {
    return await this.#rpc("stateResourcesQuery", input);
  }

  async eventsConsumersQuery(input: AdminRpcInput<"eventsConsumersQuery">) {
    return await this.#rpc("eventsConsumersQuery", input);
  }

  async eventsDeadLettersQuery(input: AdminRpcInput<"eventsDeadLettersQuery">) {
    return await this.#rpc("eventsDeadLettersQuery", input);
  }

  async eventsDeadLettersInspect(
    input: AdminRpcInput<"eventsDeadLettersInspect">,
  ) {
    return await this.#rpc("eventsDeadLettersInspect", input);
  }

  async eventsDeadLettersReplay(
    input: AdminRpcInput<"eventsDeadLettersReplay">,
  ) {
    return await this.#rpc("eventsDeadLettersReplay", input);
  }

  /** Completes a public app/client authentication flow as the test admin user. */
  async completeClientAuth(
    ctx: ClientAuthRequiredContext,
  ): Promise<ClientAuthContinuation> {
    const startedAt = performance.now();
    await this.completeBootstrap();
    const flowId = flowIdFromUrl(ctx.loginUrl);
    const binding = await performLocalLogin({
      trellisUrl: this.#trellisUrl,
      flowId,
      password: this.#adminPassword,
    });
    await approveLocalFlowIfNeeded({
      trellisUrl: this.#trellisUrl,
      flowId,
      binding,
      prepareApproval: async (state) => {
        const participantId = state.approval.contractId;
        await this.ensurePortalConsentPolicy(
          participantId,
          Object.keys(state.approval.capabilities),
        );
      },
    });
    recordTrellisDuration(
      "trellis.admin.workflow.duration",
      performance.now() - startedAt,
      { operation: "register_client", phase: "total" },
    );
    return { status: "bound", flowId };
  }

  /** Configures the built-in portal's test consent ceiling for a participant. */
  async ensurePortalConsentPolicy(
    participantId: string,
    selectionIds: readonly string[],
  ): Promise<void> {
    if (this.#configuredConsentPolicies.has(participantId)) return;
    await this.completeBootstrap();
    await this.#rpc("authPortalsGrantOverridesPut", {
      portalId: "builtin",
      participantId,
      directCapabilities: selectionIds
        .filter((key) => key.startsWith("capability:"))
        .map((key) => key.slice("capability:".length)),
      capabilityGroupKeys: [],
      roleMappings: [],
      expectedVersion: null,
      idempotencyKey: crypto.randomUUID(),
    });
    this.#configuredConsentPolicies.add(participantId);
  }

  /** Installs a participant and atomically replaces its deployment GrantBinding. */
  async applyParticipant(args: {
    deployment?: string;
    contract: TrellisTestParticipantLike;
  }): Promise<TrellisTestParticipantApproval> {
    return await adminDeployment.applyParticipant(this.#deployment, args);
  }

  /** Applies a deployment without consent; returns the pending approval when required. */
  async requestParticipantApply(args: {
    deployment?: string;
    contract: TrellisTestParticipantLike;
  }): Promise<TrellisTestParticipantApplyResult> {
    return await adminDeployment.requestParticipantApply(
      this.#deployment,
      args,
    );
  }

  /** Completes a deployment apply with server-computed consent for a pending approval. */
  async approveParticipantApply(
    pendingId: string,
  ): Promise<TrellisTestParticipantApproval> {
    return await adminDeployment.approveParticipantApply(
      this.#deployment,
      pendingId,
    );
  }

  async installParticipant(args: {
    contract: TrellisTestParticipantLike;
  }): Promise<TrellisTestParticipantApproval> {
    return await adminDeployment.installParticipant(this.#deployment, args);
  }

  /** Provisions a service instance key through `Auth.ServiceInstances.Provision`. */
  async provisionServiceInstance(args: {
    deployment?: string;
    contract: TrellisTestParticipantLike;
  }): Promise<TrellisTestServiceKey> {
    return await adminDeployment.provisionServiceInstance(
      this.#deployment,
      args,
    );
  }

  /** Runs the full service registration sequence used by test services. */
  async registerService(args: {
    deployment?: string;
    contract: TrellisTestParticipantLike;
  }): Promise<TrellisTestServiceKey> {
    return await adminDeployment.registerService(this.#deployment, args);
  }

  /** Provisions a service instance key without applying a participant or grant. */
  async provisionServiceInstanceOnly(args: {
    deployment?: string;
  }): Promise<{ seed: string; sessionKey: string }> {
    return await adminDeployment.provisionServiceInstanceOnly(
      this.#deployment,
      args,
    );
  }

  /** Disables one service instance through the bootstrap administrator identity. */
  async disableServiceInstance(
    input: AdminRpcInput<"authServiceInstancesDisable">,
  ): Promise<AdminRpc["authServiceInstancesDisable"]["output"]> {
    return await this.#rpc("authServiceInstancesDisable", input);
  }

  /** Ensures bootstrap is complete and clears the admin connection before a Trellis restart. */
  async prepareForControlPlaneRestart(): Promise<void> {
    await this.completeBootstrap();
    await this.close();
  }

  /** Closes the lazily connected admin client, when it exists. */
  async close(): Promise<void> {
    const client = this.#connectedAdminClient;
    this.#connectedAdminClient = undefined;
    this.#adminClient = undefined;
    await client?.connection.close();
  }
}
