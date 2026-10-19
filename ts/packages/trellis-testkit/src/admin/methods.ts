import type { CallerRuntime } from "@oatscenter/trellis";
import type { Codec } from "@oatscenter/trellis/generated";
import { apis, apps } from "../../trellis/index.js";

/** Generated native application selections used by the registered test administrator. */
export const adminParticipant: typeof apps.cli.request = apps.cli.request;

export const ADMIN_USERNAME = "admin";

export type AdminClient = CallerRuntime<typeof adminParticipant>;

function adminMethod<I, O>(
  descriptor: { input: Codec<I>; output: Codec<O> },
  call: (client: AdminClient, input: I) => Promise<O>,
) {
  return {
    input: descriptor.input,
    output: descriptor.output,
    // Dispatch erases the method's model type; the generated client validates
    // and encodes it. Decoding here would incorrectly treat bigint models as wire JSON.
    call: (client: AdminClient, value: unknown) => call(client, value as I),
  } as const;
}

/** @internal Concrete Auth RPCs available to the shared test host. */
export const adminMethods = {
  authIssuersRotate: adminMethod(
    apis.auth.API.actions["rpc:Issuers.Rotate"],
    (client, input) => client.issuersRotate(input).orThrow(),
  ),
  authUsersGet: adminMethod(
    apis.auth.API.actions["rpc:Users.Get"],
    (client, input) => client.usersGet(input).orThrow(),
  ),
  authUsersUpdate: adminMethod(
    apis.auth.API.actions["rpc:Users.Update"],
    (client, input) => client.usersUpdate(input).orThrow(),
  ),
  authAuthorizationSessionsRevoke: adminMethod(
    apis.auth.API.actions["rpc:AuthorizationSessions.Revoke"],
    (client, input) => client.authorizationSessionsRevoke(input).orThrow(),
  ),
  authDevicesReviewsGet: adminMethod(
    apis.auth.API.actions["rpc:Devices.Reviews.Get"],
    (client, input) => client.devicesReviewsGet(input).orThrow(),
  ),
  authDevicesReviewsDecide: adminMethod(
    apis.auth.API.actions["rpc:Devices.Reviews.Decide"],
    (client, input) => client.devicesReviewsDecide(input).orThrow(),
  ),
  authUsersResolve: adminMethod(
    apis.auth.API.actions["rpc:Users.Resolve"],
    (client, input) => client.usersResolve(input).orThrow(),
  ),
  authCapabilitiesList: adminMethod(
    apis.auth.API.actions["rpc:Capabilities.List"],
    (client, input) => client.capabilitiesList(input).orThrow(),
  ),
  authConnectionsList: adminMethod(
    apis.auth.API.actions["rpc:Connections.List"],
    (client, input) => client.connectionsList(input).orThrow(),
  ),
  authConnectionsKick: adminMethod(
    apis.auth.API.actions["rpc:Connections.Kick"],
    (client, input) => client.connectionsKick(input).orThrow(),
  ),
  authAuthorizationSessionsList: adminMethod(
    apis.auth.API.actions["rpc:AuthorizationSessions.List"],
    (client, input) => client.authorizationSessionsList(input).orThrow(),
  ),
  authResourcesList: adminMethod(
    apis.auth.API.actions["rpc:Resources.List"],
    (client, input) => client.resourcesList(input).orThrow(),
  ),
  authDevicesProvision: adminMethod(
    apis.auth.API.actions["rpc:Devices.Provision"],
    (client, input) => client.devicesProvision(input).orThrow(),
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
  authDeploymentsDisable: adminMethod(
    apis.auth.API.actions["rpc:Deployments.Disable"],
    (client, input) => client.deploymentsDisable(input).orThrow(),
  ),
  authDeploymentsEnable: adminMethod(
    apis.auth.API.actions["rpc:Deployments.Enable"],
    (client, input) => client.deploymentsEnable(input).orThrow(),
  ),
  authApisReview: adminMethod(
    apis.auth.API.actions["rpc:Apis.Review"],
    (client, input) => client.apisReview(input).orThrow(),
  ),
  authApisAccept: adminMethod(
    apis.auth.API.actions["rpc:Apis.Accept"],
    (client, input) => client.apisAccept(input).orThrow(),
  ),
  authApisForceReplace: adminMethod(
    apis.auth.API.actions["rpc:Apis.ForceReplace"],
    (client, input) => client.apisForceReplace(input).orThrow(),
  ),
  authOAuthClientsPut: adminMethod(
    apis.auth.API.actions["rpc:OAuthClients.Put"],
    (client, input) => client.oAuthClientsPut(input).orThrow(),
  ),
  authCapabilityGrantsGrant: adminMethod(
    apis.auth.API.actions["rpc:CapabilityGrants.Grant"],
    (client, input) => client.capabilityGrantsGrant(input).orThrow(),
  ),
  authCapabilityGrantsList: adminMethod(
    apis.auth.API.actions["rpc:CapabilityGrants.List"],
    (client, input) => client.capabilityGrantsList(input).orThrow(),
  ),
  authCapabilityGrantsRevoke: adminMethod(
    apis.auth.API.actions["rpc:CapabilityGrants.Revoke"],
    (client, input) => client.capabilityGrantsRevoke(input).orThrow(),
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
  authServiceInstancesRemove: adminMethod(
    apis.auth.API.actions["rpc:ServiceInstances.Remove"],
    (client, input) => client.serviceInstancesRemove(input).orThrow(),
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
