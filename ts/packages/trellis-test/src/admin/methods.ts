import type { CallerRuntime } from "@oats-center/trellis";
import { RemoteError } from "@oats-center/trellis/errors";
import type { Codec } from "@oats-center/trellis/generated";
import { apis, participants } from "../../trellis/index.js";

export const adminParticipant = {
  ...participants.cli.participant,
  id: "trellis.cli",
  identity: "trellis.cli",
} as const;

/** @internal Returns the server-computed consent request from an approval-required error. */
export function deploymentConsentRequest(error: unknown) {
  let authError: apis.auth.AuthError;
  if (error instanceof apis.auth.AuthError) {
    authError = error;
  } else if (error instanceof RemoteError) {
    try {
      authError = apis.auth.AuthError.fromSerializable(error.remoteError);
    } catch {
      return undefined;
    }
  } else {
    return undefined;
  }
  return authError.data.code === "approval_required"
    ? authError.data.consentRequest
    : undefined;
}

export const ADMIN_USERNAME = "admin";

export type AdminClient = CallerRuntime<typeof adminParticipant>;

function adminMethod<I, O>(
  descriptor: { input: Codec<I>; output: Codec<O> },
  call: (client: AdminClient, input: I) => Promise<O>,
) {
  return {
    input: descriptor.input,
    output: descriptor.output,
    call: (client: AdminClient, value: unknown) => call(client, value as I),
  } as const;
}

/** @internal Concrete Auth RPCs available to the shared test host. */
export const adminMethods = {
  authCapabilityGroupsGet: adminMethod(
    apis.auth.API.actions["rpc:CapabilityGroups.Get"],
    (client, input) => client.capabilityGroupsGet(input).orThrow(),
  ),
  authCapabilityGroupsPut: adminMethod(
    apis.auth.API.actions["rpc:CapabilityGroups.Put"],
    (client, input) => client.capabilityGroupsPut(input).orThrow(),
  ),
  authConnectionsList: adminMethod(
    apis.auth.API.actions["rpc:Connections.List"],
    (client, input) => client.connectionsList(input).orThrow(),
  ),
  authPortalsGrantOverridesRemove: adminMethod(
    apis.auth.API.actions["rpc:Portals.GrantOverrides.Remove"],
    (client, input) => client.portalsGrantOverridesRemove(input).orThrow(),
  ),
  authPortalsGrantOverridesPut: adminMethod(
    apis.auth.API.actions["rpc:Portals.GrantOverrides.Put"],
    (client, input) => client.portalsGrantOverridesPut(input).orThrow(),
  ),
  authPortalsGet: adminMethod(
    apis.auth.API.actions["rpc:Portals.Get"],
    (client, input) => client.portalsGet(input).orThrow(),
  ),
  authPortalsList: adminMethod(
    apis.auth.API.actions["rpc:Portals.List"],
    (client, input) => client.portalsList(input).orThrow(),
  ),
  authPortalsLoginSettingsGet: adminMethod(
    apis.auth.API.actions["rpc:Portals.LoginSettings.Get"],
    (client, input) => client.portalsLoginSettingsGet(input).orThrow(),
  ),
  authPortalsLoginSettingsUpdate: adminMethod(
    apis.auth.API.actions["rpc:Portals.LoginSettings.Update"],
    (client, input) => client.portalsLoginSettingsUpdate(input).orThrow(),
  ),
  authPortalsPut: adminMethod(
    apis.auth.API.actions["rpc:Portals.Put"],
    (client, input) => client.portalsPut(input).orThrow(),
  ),
  authPortalsRoutesPut: adminMethod(
    apis.auth.API.actions["rpc:Portals.Routes.Put"],
    (client, input) => client.portalsRoutesPut(input).orThrow(),
  ),
  authDevicesProvision: adminMethod(
    apis.auth.API.actions["rpc:Devices.Provision"],
    (client, input) => client.devicesProvision(input).orThrow(),
  ),
  stateResourcesInspect: adminMethod(
    apis.state.API.actions["rpc:Resources.Inspect"],
    (client, input) => client.resourcesInspect(input).orThrow(),
  ),
  stateResourcesQuery: adminMethod(
    apis.state.API.actions["rpc:Resources.Query"],
    (client, input) => client.resourcesQuery(input).orThrow(),
  ),
  authDeploymentsCreate: adminMethod(
    apis.auth.API.actions["rpc:Deployments.Create"],
    (client, input) => client.deploymentsCreate(input).orThrow(),
  ),
  authDeploymentsGet: adminMethod(
    apis.auth.API.actions["rpc:Deployments.Get"],
    (client, input) => client.deploymentsGet(input).orThrow(),
  ),
  authDeploymentsApply: adminMethod(
    apis.auth.API.actions["rpc:Deployments.Apply"],
    (client, input) => client.deploymentsApply(input).orThrow(),
  ),
  authParticipantsInstall: adminMethod(
    apis.auth.API.actions["rpc:Participants.Install"],
    (client, input) => client.participantsInstall(input).orThrow(),
  ),
  authParticipantsGet: adminMethod(
    apis.auth.API.actions["rpc:Participants.Get"],
    (client, input) => client.participantsGet(input).orThrow(),
  ),
  authGrantsGet: adminMethod(
    apis.auth.API.actions["rpc:Grants.Get"],
    (client, input) => client.grantsGet(input).orThrow(),
  ),
  authGrantsList: adminMethod(
    apis.auth.API.actions["rpc:Grants.List"],
    (client, input) => client.grantsList(input).orThrow(),
  ),
  authGrantsSet: adminMethod(
    apis.auth.API.actions["rpc:Grants.Set"],
    (client, input) => client.grantsSet(input).orThrow(),
  ),
  authGrantsRevoke: adminMethod(
    apis.auth.API.actions["rpc:Grants.Revoke"],
    (client, input) => client.grantsRevoke(input).orThrow(),
  ),
  authServiceInstancesProvision: adminMethod(
    apis.auth.API.actions["rpc:ServiceInstances.Provision"],
    (client, input) => client.serviceInstancesProvision(input).orThrow(),
  ),
  authUsersCreate: adminMethod(
    apis.auth.API.actions["rpc:Users.Create"],
    (client, input) => client.usersCreate(input).orThrow(),
  ),
  authUsersPasswordResetCreate: adminMethod(
    apis.auth.API.actions["rpc:Users.PasswordReset.Create"],
    (client, input) => client.usersPasswordResetCreate(input).orThrow(),
  ),
  authServiceInstancesDisable: adminMethod(
    apis.auth.API.actions["rpc:ServiceInstances.Disable"],
    (client, input) => client.serviceInstancesDisable(input).orThrow(),
  ),
  authSessionsList: adminMethod(
    apis.auth.API.actions["rpc:Sessions.List"],
    (client, input) => client.sessionsList(input).orThrow(),
  ),
  authSessionsRevoke: adminMethod(
    apis.auth.API.actions["rpc:Sessions.Revoke"],
    (client, input) => client.sessionsRevoke(input).orThrow(),
  ),
  eventsConsumersQuery: adminMethod(
    apis.events.API.actions["rpc:Consumers.Query"],
    (client, input) => client.consumersQuery(input).orThrow(),
  ),
  eventsDeadLettersQuery: adminMethod(
    apis.events.API.actions["rpc:DeadLetters.Query"],
    (client, input) => client.deadLettersQuery(input).orThrow(),
  ),
  eventsDeadLettersInspect: adminMethod(
    apis.events.API.actions["rpc:DeadLetters.Inspect"],
    (client, input) => client.deadLettersInspect(input).orThrow(),
  ),
  eventsDeadLettersReplay: adminMethod(
    apis.events.API.actions["rpc:DeadLetters.Replay"],
    (client, input) => client.deadLettersReplay(input).orThrow(),
  ),
} as const;

export type AdminRpc = {
  [M in keyof typeof adminMethods]: {
    input: ReturnType<(typeof adminMethods)[M]["input"]["decode"]>;
    output: ReturnType<(typeof adminMethods)[M]["output"]["decode"]>;
  };
};

export type AdminRpcInput<M extends TrellisTestAdminRpcMethod> =
  AdminRpc[M]["input"];

export type TrellisTestAdminRpcMethod = keyof typeof adminMethods;
