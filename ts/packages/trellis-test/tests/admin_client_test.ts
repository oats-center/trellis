import { assertEquals, assertRejects } from "@std/assert";

import {
  type AdminDeploymentContext,
  createDeployment,
  installParticipant,
} from "../src/admin/deployment.ts";
import type {
  AdminRpc,
  TrellisTestAdminRpcMethod,
} from "../src/admin/methods.ts";
import {
  adminMethods,
  TrellisTestAdminAutomation,
} from "../src/admin_client.ts";
import { participants } from "../trellis/index.js";

const expectedAdminMethods = {
  authCapabilityGroupsGet: "capabilityGroupsGet",
  authCapabilityGroupsPut: "capabilityGroupsPut",
  authConnectionsList: "connectionsList",
  authPortalsGrantOverridesRemove: "portalsGrantOverridesRemove",
  authPortalsGrantOverridesPut: "portalsGrantOverridesPut",
  authPortalsGet: "portalsGet",
  authPortalsList: "portalsList",
  authPortalsLoginSettingsGet: "portalsLoginSettingsGet",
  authPortalsLoginSettingsUpdate: "portalsLoginSettingsUpdate",
  authPortalsPut: "portalsPut",
  authPortalsRoutesPut: "portalsRoutesPut",
  authDevicesProvision: "devicesProvision",
  stateResourcesInspect: "resourcesInspect",
  stateResourcesQuery: "resourcesQuery",
  authDeploymentsCreate: "deploymentsCreate",
  authDeploymentsGet: "deploymentsGet",
  authDeploymentsApply: "deploymentsApply",
  authParticipantsInstall: "participantsInstall",
  authParticipantsGet: "participantsGet",
  authGrantsGet: "grantsGet",
  authGrantsList: "grantsList",
  authGrantsSet: "grantsSet",
  authGrantsRevoke: "grantsRevoke",
  authServiceInstancesProvision: "serviceInstancesProvision",
  authUsersCreate: "usersCreate",
  authUsersPasswordResetCreate: "usersPasswordResetCreate",
  authServiceInstancesDisable: "serviceInstancesDisable",
  authSessionsList: "sessionsList",
  authSessionsRevoke: "sessionsRevoke",
  eventsConsumersQuery: "consumersQuery",
  eventsDeadLettersQuery: "deadLettersQuery",
  eventsDeadLettersInspect: "deadLettersInspect",
  eventsDeadLettersReplay: "deadLettersReplay",
} as const;

Deno.test("admin registry and local dispatch stay in parity", async () => {
  assertEquals(Object.keys(adminMethods), Object.keys(expectedAdminMethods));

  for (const [method, clientMethod] of Object.entries(expectedAdminMethods)) {
    let called: PropertyKey | undefined;
    const client = new Proxy({}, {
      get: (_target, property) => (_input: unknown) => ({
        orThrow: () => {
          called = property;
          return Promise.resolve({});
        },
      }),
    });

    await adminMethods[method as keyof typeof adminMethods].call(
      client as never,
      {},
    );
    assertEquals(called, clientMethod);
  }
});

Deno.test("test CLI exposes the authority workflow actions", () => {
  const auth = participants.cli.participant.uses.find((use) =>
    use.api.identity === "trellis.auth@v1"
  );
  assertEquals(
    auth?.actions.map((action) => action.descriptorName).filter((name) =>
      name.startsWith("rpc:Grants.") || name === "rpc:Sessions.Revoke"
    ),
    [
      "rpc:Grants.Get",
      "rpc:Grants.List",
      "rpc:Grants.Revoke",
      "rpc:Grants.Set",
      "rpc:Sessions.Revoke",
    ],
  );
  assertEquals(
    Object.fromEntries(
      Object.entries(participants.cli.participant.actionNames).filter(([key]) =>
        key.includes(":rpc:Grants.") || key.endsWith(":rpc:Sessions.Revoke")
      ),
    ),
    {
      "trellis.auth@v1:rpc:Grants.Get": "Grants.Get",
      "trellis.auth@v1:rpc:Grants.List": "Grants.List",
      "trellis.auth@v1:rpc:Grants.Revoke": "Grants.Revoke",
      "trellis.auth@v1:rpc:Grants.Set": "Grants.Set",
      "trellis.auth@v1:rpc:Sessions.Revoke": "Sessions.Revoke",
    },
  );
});

Deno.test("client auth completion is not an admin RPC", async () => {
  assertEquals(Object.hasOwn(adminMethods, "completeClientAuth"), false);

  const admin = new TrellisTestAdminAutomation({
    trellisUrl: "http://127.0.0.1",
    adminPassword: "test",
    defaultDeployment: "test",
    getBootstrapUrl: () => Promise.reject(new Error("not used")),
    bootstrapComplete: true,
  });

  await assertRejects(
    () => admin.callAdminRpc("completeClientAuth", {}),
    Error,
    "unsupported Trellis test admin RPC completeClientAuth",
  );
});

Deno.test("participant install forwards opaque package evidence", async () => {
  let request: unknown;
  const context: AdminDeploymentContext = {
    defaultDeployment: "test",
    createdDeployments: new Map(),
    deploymentBindingRevisions: new Map(),
    deploymentIds: new Map(),
    installedParticipants: new Map(),
    pendingApprovals: new Map(),
    rpc: <M extends TrellisTestAdminRpcMethod>(
      method: M,
      input: AdminRpc[M]["input"],
    ): Promise<AdminRpc[M]["output"]> => {
      assertEquals(method, "authParticipantsInstall");
      request = input;
      return Promise.resolve({ participant: { revision: 1n } }) as Promise<
        AdminRpc[M]["output"]
      >;
    },
  };

  await installParticipant(context, { contract: participants.cli.participant });

  const input = request as Record<string, unknown>;
  assertEquals(
    input.packageEvidence,
    participants.cli.participant.packageEvidence,
  );
  assertEquals(input.participantPath, participants.cli.participant.path);
  assertEquals(Object.hasOwn(input, "participantArtifact"), false);
  assertEquals(Object.hasOwn(input, "apiArtifacts"), false);
});

Deno.test("concurrent deployment creation shares failure and permits retry", async () => {
  const failure = new Error("deployment creation failed");
  let attempts = 0;
  const context: AdminDeploymentContext = {
    defaultDeployment: "test",
    createdDeployments: new Map(),
    deploymentBindingRevisions: new Map(),
    deploymentIds: new Map(),
    installedParticipants: new Map(),
    pendingApprovals: new Map(),
    rpc: <M extends TrellisTestAdminRpcMethod>(
      method: M,
      _input: AdminRpc[M]["input"],
    ): Promise<AdminRpc[M]["output"]> => {
      if (method !== "authDeploymentsCreate") {
        return Promise.reject(new Error(`unexpected admin RPC ${method}`));
      }
      attempts += 1;
      if (attempts === 1) return Promise.reject(failure);
      return Promise.resolve(
        {
          deployment: {
            kind: "service",
            deploymentId: "deployment-1",
            namespaces: [],
          },
        } as AdminRpc[M]["output"],
      );
    },
  };

  const first = createDeployment(context);
  const second = createDeployment(context);
  const firstError = await assertRejects(() => first, Error, failure.message);
  const secondError = await assertRejects(() => second, Error, failure.message);
  assertEquals(firstError, failure);
  assertEquals(secondError, failure);
  assertEquals(attempts, 1);
  await createDeployment(context);
  assertEquals(attempts, 2);
});
