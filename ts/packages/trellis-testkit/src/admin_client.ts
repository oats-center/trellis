import { codecs } from "@oatscenter/trellis/generated";
import {
  connectNativeClient,
  type NativeOAuthSession,
} from "@oatscenter/trellis/native";
import { ulid } from "ulid";
import { loginNativeTestClient } from "./admin/native_auth.ts";
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
import type {
  TrellisTestParticipantApproval,
  TrellisTestParticipantLike,
  TrellisTestServiceKey,
} from "./types.ts";

export { adminMethods, type TrellisTestAdminRpcMethod };

/** Internal public-surface admin automation used by `TrellisTestRuntime`. */
export class TrellisTestAdminAutomation {
  #clientCapabilitySetup = new Map<string, Promise<void>>();
  readonly #oauthIssuer: string;
  readonly #adminPassword: string;
  readonly #getBootstrapUrl: () => Promise<string>;
  #nativeSession: NativeOAuthSession | undefined;
  #bootstrapComplete: Promise<void> | undefined;
  #adminClient: Promise<AdminClient> | undefined;
  #connectedAdminClient: AdminClient | undefined;
  readonly #deployment: adminDeployment.AdminDeploymentContext;

  /** Creates admin automation backed by the supplied bootstrap URL provider. */
  constructor(args: {
    oauthIssuer: string;
    adminPassword: string;
    defaultDeployment: string;
    getBootstrapUrl: () => Promise<string>;
    bootstrapComplete?: boolean;
    resourceReadyTimeoutMs?: number;
  }) {
    this.#oauthIssuer = args.oauthIssuer;
    this.#adminPassword = args.adminPassword;
    this.#getBootstrapUrl = args.getBootstrapUrl;
    this.#deployment = {
      defaultDeployment: args.defaultDeployment,
      deploymentIds: new Map(),
      deploymentRevisions: new Map(),
      deploymentDigests: new Map(),
      acceptedApis: new Map(),
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
        const url = new URL(bootstrapUrl);
        const token = url.searchParams.get("token");
        if (!token)
          throw new Error(
            "Trellis administrator URL is missing its reset token",
          );
        const page = await fetch(url, {
          redirect: "error",
          signal: AbortSignal.timeout(30_000),
        });
        const cookies = page.headers
          .getSetCookie()
          .map((cookie) => cookie.split(";", 1)[0]);
        const csrf = cookies
          .find((cookie) => cookie.startsWith("__Host-trellis-portal-csrf="))
          ?.split("=", 2)[1];
        if (!page.ok || !csrf)
          throw new Error(
            "Trellis administrator setup could not establish a portal session",
          );
        await page.body?.cancel();
        const response = await fetch(new URL("/portal/account/reset", url), {
          method: "POST",
          redirect: "error",
          signal: AbortSignal.timeout(30_000),
          headers: {
            Origin: url.origin,
            Cookie: cookies.join("; "),
            "Content-Type": "application/x-www-form-urlencoded",
          },
          body: new URLSearchParams({
            token,
            csrf,
            password: this.#adminPassword,
          }),
        });
        await response.body?.cancel();
        if (!response.ok) {
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
        const login = await loginNativeTestClient({
          issuer: this.#oauthIssuer,
          // OAuth registration identity is independent of the local generated request descriptor.
          clientId: "trellis.cli",
          request: adminParticipant,
          username: ADMIN_USERNAME,
          password: this.#adminPassword,
          administrativeCapabilities: [
            "trellis.auth.v1:users.manage",
            "trellis.auth.v1:roles.manage",
            "trellis.auth.v1:apis.accept",
            "trellis.auth.v1:apis.forceReplace",
            "trellis.auth.v1:clients.manage",
            "trellis.auth.v1:platform.manage",
          ],
        });
        this.#nativeSession = login.session;
        const client = await connectNativeClient({
          ...login,
          name: "trellis-testkit-admin",
          timeout: 60_000,
          request: adminParticipant,
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

  /** Grants declared test-user capabilities once, preserving subsequent explicit revocations. */
  async prepareClientCapabilities(
    capabilityIds: readonly string[],
    targetPrincipalId?: string,
  ): Promise<void> {
    const client = await this.#client();
    const principalId =
      targetPrincipalId ??
      (await client.sessionsMe({}).orThrow()).principal.principalId;
    for (const capabilityId of new Set(capabilityIds)) {
      const key = JSON.stringify([principalId, capabilityId]);
      let setup = this.#clientCapabilitySetup.get(key);
      if (!setup) {
        setup = this.grantClientCapabilities(principalId, [capabilityId]);
        this.#clientCapabilitySetup.set(key, setup);
      }
      await setup;
    }
  }

  /** Explicitly grant missing capabilities to one principal, including after a deliberate revocation. */
  async grantClientCapabilities(
    principalId: string,
    capabilityIds: readonly string[],
  ): Promise<void> {
    const principal = codecs.ulid.decode(principalId);
    const client = await this.#client();
    for (const capabilityId of new Set(capabilityIds)) {
      const grants = await client
        .capabilityGrantsList({ principalId: principal, capabilityId })
        .orThrow();
      if (grants.items.length === 0) {
        await client
          .capabilityGrantsGrant({
            principalId: principal,
            capabilityId,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
      }
    }
  }

  /** Forwards one low-level Auth RPC through generated model validation and encoding. */
  async callAdminRpc(method: string, input: unknown): Promise<unknown> {
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
    return (await this.callAdminRpc(method, input)) as AdminRpc[M]["output"];
  }

  /** Creates a service deployment through `Auth.Deployments.Create`. */
  async createDeployment(args: {
    deployment?: string;
    contract: TrellisTestParticipantLike;
  }): Promise<void> {
    return await adminDeployment.createDeployment(this.#deployment, args);
  }

  async provisionDevice(
    input: import("../trellis/index.js").apis.auth.DevicesProvisionInput,
  ): Promise<import("../trellis/index.js").apis.auth.DevicesProvisionOutput> {
    return await this.#rpc("authDevicesProvision", {
      ...input,
      deploymentId:
        this.#deployment.deploymentIds.get(input.deploymentId) ??
        input.deploymentId,
    });
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

  /** Accepts generated APIs and applies the deployment's native source evidence. */
  async applyParticipant(args: {
    deployment?: string;
    contract: TrellisTestParticipantLike;
  }): Promise<TrellisTestParticipantApproval> {
    return await adminDeployment.applyParticipant(this.#deployment, args);
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
    const session = this.#nativeSession;
    this.#nativeSession = undefined;
    this.#connectedAdminClient = undefined;
    this.#adminClient = undefined;
    try {
      await client?.connection.close();
    } finally {
      await session?.close();
    }
  }
}
