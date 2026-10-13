import { apis } from "trellis-web-generated";

import type { OwnAuthoritySnapshot } from "@oatscenter/trellis";

/** Advisory actions and privileges from the verified promoted installation. */
export type Authority = Pick<
  OwnAuthoritySnapshot,
  "authorityDigest" | "platformPrivileges" | "actions"
>;

const HEALTH_API = apis.health.API.identity;
const EVENTS_API = apis.events.API.identity;
const JOBS_API = apis.jobs.API.identity;

/** One exact API-surface permission atom required by a route or action. */
export type ExactPermission = {
  api: string;
  surface: OwnAuthoritySnapshot["actions"][number]["action"]["kind"] | "feed";
  name: string;
  action: OwnAuthoritySnapshot["actions"][number]["action"]["direction"];
};

type Requirement = {
  administrative?: boolean;
  privilege?: string;
  permission?: ExactPermission;
};

type DisplayProfile = {
  displayName?: string | null;
};

export const routeTitles = {
  "/profile": "Account",
  "/admin": "Overview",
  "/admin/users": "Users",
  "/admin/users/edit": "Edit User",
  "/admin/users/new": "Create User",
  "/admin/sessions": "Sessions",
  "/admin/services": "Services",
  "/admin/devices": "Devices",
  "/admin/resources": "Resources",
  "/admin/sessions/revoke": "Revoke Session",
  "/admin/sessions/kick": "Kick Connection",
  "/admin/services/new": "Create Service Deployment",
  "/admin/devices/profiles/new": "Create Device Deployment",
  "/admin/devices/profiles/disable": "Disable Device Deployment",
  "/admin/devices/instances/provision": "Provision Device Instance",
  "/admin/devices/instances/disable": "Disable Device Instance",
  "/admin/health-events": "Health",
  "/admin/events": "Events",
  "/admin/jobs": "Jobs",
  "/admin/apps": "Delegations",
  "/admin/apps/revoke": "Revoke Delegation",
  "/admin/roles": "Roles",
  "/admin/roles/edit": "Edit Role",
  "/admin/roles/new": "New Role",
  "/admin/clients": "OAuth clients",
  "/admin/apis": "API catalog",
} as const;

/**
 * Titles for routes whose last path segment is an opaque target ID. The title
 * depends only on the current path, so a target change replaces it immediately
 * and no previous target's name can linger.
 */
const detailRouteTitles: ReadonlyArray<
  readonly [prefix: string, title: string]
> = [
  ["/admin/services/", "Service Deployment"],
  ["/admin/jobs/", "Job"],
];

type AppPathname = keyof typeof routeTitles;

export type NavItem = {
  href: AppPathname;
  label: string;
  icon: string;
  requires?: readonly Requirement[];
};

export type NavSection = {
  title: string;
  items: NavItem[];
};

const ADMIN_ONLY: readonly Requirement[] = [{ administrative: true }];

const navSections: NavSection[] = [
  {
    title: "Account",
    items: [{ href: "/profile", label: "Account", icon: "settings" }],
  },
  {
    title: "Operate",
    items: [
      {
        href: "/admin",
        label: "Overview",
        icon: "users",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/health-events",
        label: "Health Events",
        icon: "alert",
        requires: [
          {
            permission: {
              api: HEALTH_API,
              surface: "rpc",
              name: "Query",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/sessions",
        label: "Sessions",
        icon: "activity",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/events",
        label: "Events",
        icon: "activity",
        requires: [
          {
            permission: {
              api: EVENTS_API,
              surface: "rpc",
              name: "Query",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/jobs",
        label: "Jobs",
        icon: "clipboard",
        requires: [
          {
            permission: {
              api: JOBS_API,
              surface: "rpc",
              name: "Query",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/apps",
        label: "Delegations",
        icon: "key",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/roles",
        label: "Roles",
        icon: "key",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/clients",
        label: "OAuth clients",
        icon: "key",
        requires: [
          {
            privilege: "clients.manage",
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "OAuthClients.List",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/apis",
        label: "API catalog",
        icon: "server",
        requires: [
          {
            privilege: "apis.accept",
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "Apis.List",
              action: "call",
            },
          },
        ],
      },
    ],
  },
  {
    title: "Manage",
    items: [
      {
        href: "/admin/resources",
        label: "Resources",
        icon: "database",
        requires: [
          {
            privilege: "principals.manage",
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "Resources.List",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/services",
        label: "Services",
        icon: "server",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/devices",
        label: "Devices",
        icon: "phone",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/users",
        label: "Users",
        icon: "users",
        requires: ADMIN_ONLY,
      },
    ],
  },
];

/** True when the owner authorized the exact action through its verified accepted catalog. */
export function hasExactPermission(
  authority: Authority,
  required: ExactPermission,
): boolean {
  return authority.actions.some(
    ({ apiId, action }) =>
      apiId === required.api &&
      action.kind ===
        (required.surface === "feed" ? "live" : required.surface) &&
      action.name === required.name &&
      action.direction === required.action,
  );
}

/** Human-facing operator classification from finite delegated administrative privileges. */
export function hasPlatformAdmin(
  authority: Pick<Authority, "platformPrivileges"> | null,
): boolean {
  return (authority?.platformPrivileges.length ?? 0) > 0;
}

function meetsRequirements(
  authority: Authority | null,
  requirements: readonly Requirement[],
): boolean {
  return requirements.every(
    (requirement) =>
      (!requirement.administrative || hasPlatformAdmin(authority)) &&
      (requirement.privilege === undefined ||
        authority?.platformPrivileges.some(
          (privilege) => privilege === requirement.privilege,
        ) === true) &&
      (requirement.permission === undefined ||
        (authority !== null &&
          hasExactPermission(authority, requirement.permission))),
  );
}

export function canAccessRoute(
  pathname: string,
  authority: Authority | null,
): boolean {
  if (!pathname.startsWith("/admin")) return true;
  const item = navSections
    .flatMap((section) => section.items)
    .filter(
      (candidate) =>
        pathname === candidate.href ||
        pathname.startsWith(`${candidate.href}/`),
    )
    .sort((left, right) => right.href.length - left.href.length)[0];
  return (
    item !== undefined && meetsRequirements(authority, item.requires ?? [])
  );
}

export function requiresAdministrativeRoute(pathname: string): boolean {
  return pathname === "/admin" || pathname.startsWith("/admin/");
}

export function getVisibleNavSections(
  authority: Authority | null,
): NavSection[] {
  return navSections
    .map((section) => ({
      ...section,
      items: section.items.filter((item) =>
        meetsRequirements(authority, item.requires ?? []),
      ),
    }))
    .filter((section) => section.items.length > 0);
}

function hasRouteTitle(pathname: string): pathname is keyof typeof routeTitles {
  return Object.hasOwn(routeTitles, pathname);
}

export function getPageTitle(pathname: string): string {
  if (hasRouteTitle(pathname)) return routeTitles[pathname];
  const detail = detailRouteTitles.find(
    ([prefix]) =>
      pathname.startsWith(prefix) && pathname.length > prefix.length,
  );
  return detail?.[1] ?? "Trellis";
}

export function getRoleLabel(
  authority: Pick<Authority, "platformPrivileges"> | null,
): string {
  return hasPlatformAdmin(authority) ? "Operator" : "Member";
}

export function getInitials(
  profile: DisplayProfile | null | undefined,
): string {
  const name = profile?.displayName?.trim();
  if (!name) return "TR";

  const parts = name.split(/\s+/).filter(Boolean);
  const initials = parts
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase() ?? "")
    .join("");
  return initials || name.slice(0, 2).toUpperCase();
}
