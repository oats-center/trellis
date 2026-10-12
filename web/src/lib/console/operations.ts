/**
 * Exact console operation registry.
 *
 * Every operation is keyed by the generated client method name, so page code
 * asks for the operation it is about to perform rather than naming an API
 * surface by hand. Each entry carries the generated API identity and the exact
 * API interaction, plus the platform privilege required for administrative
 * surfaces.
 *
 * The registry exists so a route's primary query and each optional panel or
 * action are authorized independently. A denied optional read is never
 * dispatched, and a permitted primary read is never discarded because an
 * unrelated optional read failed.
 */

import { apis } from "trellis-web-generated";

import { type Authority, hasExactPermission } from "../control-panel.ts";

/** API surface families that can appear in a generated permission atom. */
export type OperationSurface = "rpc" | "feed" | "event";

/** Exact interaction required for one surface family. */
export type OperationAction = "call" | "subscribe" | "publish";

/** One exact generated API interaction. */
export type ConsoleOperation = {
  /** Generated API identity, for example `trellis.auth@v1`. */
  readonly api: string;
  /** API surface family. */
  readonly surface: OperationSurface;
  /** Exact API-local surface name, for example `Deployments.List`. */
  readonly name: string;
  /** Exact interaction. */
  readonly action: OperationAction;
  /** Platform privilege required in addition to the exact permission atom. */
  readonly privilege?: string;
};

function authRpc(
  name: string,
  privilege?: ConsoleOperation["privilege"],
): ConsoleOperation {
  return {
    api: apis.auth.API.identity,
    surface: "rpc",
    name,
    action: "call",
    ...(privilege === undefined ? {} : { privilege }),
  };
}

function eventsRpc(name: string): ConsoleOperation {
  return {
    api: apis.events.API.identity,
    surface: "rpc",
    name,
    action: "call",
  };
}

function eventsFeed(name: string): ConsoleOperation {
  return {
    api: apis.events.API.identity,
    surface: "feed",
    name,
    action: "subscribe",
  };
}

function healthRpc(name: string): ConsoleOperation {
  return {
    api: apis.health.API.identity,
    surface: "rpc",
    name,
    action: "call",
  };
}

function healthEvent(name: string): ConsoleOperation {
  return {
    api: apis.health.API.identity,
    surface: "event",
    name,
    action: "subscribe",
  };
}

function healthFeed(name: string): ConsoleOperation {
  return {
    api: apis.health.API.identity,
    surface: "feed",
    name,
    action: "subscribe",
  };
}

function jobsRpc(name: string): ConsoleOperation {
  return {
    api: apis.jobs.API.identity,
    surface: "rpc",
    name,
    action: "call",
  };
}

function jobsFeed(name: string): ConsoleOperation {
  return {
    api: apis.jobs.API.identity,
    surface: "feed",
    name,
    action: "subscribe",
  };
}

function stateRpc(name: string): ConsoleOperation {
  return {
    api: apis.state.API.identity,
    surface: "rpc",
    name,
    action: "call",
  };
}

/**
 * Every console operation, keyed by generated client method name.
 *
 * Entries are added only for operations the console actually performs; an
 * absent operation is not implicitly permitted. Self-service operations carry
 * no platform privilege.
 */
export const CONSOLE_OPERATIONS = {
  // Auth: session and account self-service.
  usersPasswordChange: {
    api: apis.auth.API.identity,
    surface: "rpc",
    name: "Users.Password.Change",
    action: "call",
  },
  userIdentitiesList: {
    api: apis.auth.API.identity,
    surface: "rpc",
    name: "UserIdentities.List",
    action: "call",
  },
  userIdentitiesUnlink: {
    api: apis.auth.API.identity,
    surface: "rpc",
    name: "UserIdentities.Unlink",
    action: "call",
  },

  // Auth: deployments, participants, service instances.
  deploymentsList: authRpc("Deployments.List"),
  deploymentsGet: authRpc("Deployments.Get"),
  deploymentsCreate: authRpc("Deployments.Create"),
  deploymentsApply: authRpc("Deployments.Apply"),
  deploymentsDisable: authRpc("Deployments.Disable"),
  deploymentsEnable: authRpc("Deployments.Enable"),
  deploymentsRemove: authRpc("Deployments.Remove"),
  serviceInstancesList: authRpc("ServiceInstances.List"),
  serviceInstancesProvision: authRpc("ServiceInstances.Provision"),
  serviceInstancesDisable: authRpc("ServiceInstances.Disable"),
  serviceInstancesEnable: authRpc("ServiceInstances.Enable"),
  serviceInstancesRemove: authRpc("ServiceInstances.Remove"),

  // Auth: devices, authorities, reviews.
  devicesList: authRpc("Devices.List"),
  devicesProvision: authRpc("Devices.Provision"),
  devicesDisable: authRpc("Devices.Disable"),
  devicesEnable: authRpc("Devices.Enable"),
  devicesRemove: authRpc("Devices.Remove"),

  // Auth: retained allocations, independent of active deployment declarations.
  resourcesList: authRpc("Resources.List", "principals.manage"),
  resourcesGet: authRpc("Resources.Get", "principals.manage"),
  resourcesRemove: authRpc("Resources.Remove", "principals.manage"),

  // Auth: sessions, connections, users, identities.
  sessionsList: authRpc("Sessions.List"),
  sessionsRevoke: authRpc("Sessions.Revoke"),
  connectionsList: authRpc("Connections.List"),
  connectionsKick: authRpc("Connections.Kick"),
  usersList: authRpc("Users.List"),
  usersGet: authRpc("Users.Get"),
  usersCreate: authRpc("Users.Create"),
  usersUpdate: authRpc("Users.Update"),
  usersResolve: authRpc("Users.Resolve"),
  usersIdentityLinkCreate: authRpc("Users.IdentityLink.Create"),
  usersPasswordResetCreate: authRpc("Users.PasswordReset.Create"),

  // Auth: grants, capability groups, capabilities, issuers.
  rolesList: authRpc("Roles.List", "roles.manage"),
  rolesGet: authRpc("Roles.Get", "roles.manage"),
  rolesPut: authRpc("Roles.Put", "roles.manage"),
  rolesDelete: authRpc("Roles.Delete", "roles.manage"),
  roleAssignmentsList: authRpc("RoleAssignments.List", "roles.manage"),
  roleAssignmentsAssign: authRpc("RoleAssignments.Assign", "roles.manage"),
  roleAssignmentsRevoke: authRpc("RoleAssignments.Revoke", "roles.manage"),
  capabilityGrantsList: authRpc("CapabilityGrants.List", "roles.manage"),
  capabilityGrantsGrant: authRpc("CapabilityGrants.Grant", "roles.manage"),
  capabilityGrantsRevoke: authRpc("CapabilityGrants.Revoke", "roles.manage"),
  principalsEffectiveAccess: authRpc(
    "Principals.EffectiveAccess",
    "roles.manage",
  ),
  delegationsList: authRpc("Delegations.List"),
  delegationsRevoke: authRpc("Delegations.Revoke"),
  authorizationSessionsList: authRpc("AuthorizationSessions.List"),
  authorizationSessionsRevoke: authRpc("AuthorizationSessions.Revoke"),
  capabilitiesList: authRpc("Capabilities.List", "roles.manage"),
  issuersRevoke: authRpc("Issuers.Revoke"),

  // Events: primary query, optional panels, actions, watch.
  eventsQuery: eventsRpc("Query"),
  eventsInspect: eventsRpc("Inspect"),
  eventsMetrics: eventsRpc("Metrics"),
  eventsDiagnostics: eventsRpc("Diagnostics"),
  consumersQuery: eventsRpc("Consumers.Query"),
  consumersInspect: eventsRpc("Consumers.Inspect"),
  deadLettersQuery: eventsRpc("DeadLetters.Query"),
  deadLettersInspect: eventsRpc("DeadLetters.Inspect"),
  deadLettersReplay: eventsRpc("DeadLetters.Replay"),
  deadLettersDismiss: eventsRpc("DeadLetters.Dismiss"),
  eventsWatch: eventsFeed("Watch"),

  // Health: primary query, optional panels, watch.
  healthQuery: healthRpc("Query"),
  healthSummary: healthRpc("Summary"),
  healthMetrics: healthRpc("Metrics"),
  healthInspect: healthRpc("Inspect"),
  healthWatch: healthFeed("Watch"),
  healthStatusChanged: healthEvent("StatusChanged"),

  // Jobs: primary query, optional panels, actions, watch.
  jobsQuery: jobsRpc("Query"),
  jobsSummary: jobsRpc("Summary"),
  jobsListServices: jobsRpc("ListServices"),
  jobsMetrics: jobsRpc("Metrics"),
  jobsInspect: jobsRpc("Inspect"),
  jobsListDLQ: jobsRpc("ListDLQ"),
  jobsGetKey: jobsRpc("GetKey"),
  jobsCancel: jobsRpc("Cancel"),
  jobsRetry: jobsRpc("Retry"),
  jobsReplayDLQ: jobsRpc("ReplayDLQ"),
  jobsDismissDLQ: jobsRpc("DismissDLQ"),
  jobsWatch: jobsFeed("Watch"),

  // State: administrative resource inspection only.
  resourcesInspect: stateRpc("Resources.Inspect"),
  resourcesQuery: stateRpc("Resources.Query"),
} as const satisfies Record<string, ConsoleOperation>;

/** Generated client method names that have a registered exact operation. */
export type ConsoleOperationName = keyof typeof CONSOLE_OPERATIONS;

/** True when `name` is a registered console operation. */
export function isConsoleOperation(name: string): name is ConsoleOperationName {
  return Object.hasOwn(CONSOLE_OPERATIONS, name);
}

/**
 * True when the current authority may perform `name`.
 *
 * Both the exact contract permission atom and any required platform privilege
 * must be present. An unregistered operation is never permitted.
 */
export function canPerform(authority: Authority | null, name: string): boolean {
  if (authority === null) return false;
  if (!isConsoleOperation(name)) return false;
  const operation: ConsoleOperation = CONSOLE_OPERATIONS[name];
  if (
    operation.privilege !== undefined &&
    !authority.platformPrivileges.some(
      (privilege) => privilege === operation.privilege,
    )
  ) {
    return false;
  }
  return hasExactPermission(authority, {
    api: operation.api,
    surface: operation.surface,
    name: operation.name,
    action: operation.action,
  });
}
