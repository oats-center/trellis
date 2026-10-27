import { apis } from "trellis-web-generated";

import type { OwnAuthoritySnapshot } from "@oatscenter/trellis";

/** Advisory capabilities and actions from the verified promoted installation. */
export type Authority = Pick<
  OwnAuthoritySnapshot,
  "authorityDigest" | "capabilities" | "actions"
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
  "/admin/services/new": "New Service",
  "/admin/devices/profiles/new": "New Device Type",
  "/admin/devices/profiles/disable": "Disable Device Type",
  "/admin/devices/instances/provision": "Add Device",
  "/admin/devices/instances/disable": "Disable Device",
  "/admin/health-events": "Health",
  "/admin/events": "Events",
  "/admin/jobs": "Jobs",
  "/admin/apps": "Connected Apps",
  "/admin/apps/revoke": "Disconnect App",
  "/admin/roles": "Roles",
  "/admin/roles/edit": "Edit Role",
  "/admin/roles/new": "New Role",
  "/admin/clients": "Apps",
  "/admin/apis": "APIs",
  "/admin/sign-in": "Sign-in methods",
  "/admin/branding": "Sign-in page",
  "/admin/administration": "Admin approvals",
} as const;

/**
 * Titles for routes whose last path segment is an opaque target ID. The title
 * depends only on the current path, so a target change replaces it immediately
 * and no previous target's name can linger.
 */
const detailRouteTitles: ReadonlyArray<
  readonly [prefix: string, title: string]
> = [
  ["/admin/services/", "Service"],
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
    title: "",
    items: [
      {
        href: "/admin",
        label: "Overview",
        icon: "home",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/health-events",
        label: "Health",
        icon: "activity",
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
        href: "/admin/events",
        label: "Events",
        icon: "fileText",
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
        icon: "cog",
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
        href: "/admin/sessions",
        label: "Sessions",
        icon: "monitor",
        requires: ADMIN_ONLY,
      },
    ],
  },
  {
    title: "Access",
    items: [
      {
        href: "/admin/users",
        label: "Users",
        icon: "user",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/roles",
        label: "Roles",
        icon: "shield",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/apps",
        label: "Connected apps",
        icon: "plug",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/administration",
        label: "Admin approvals",
        icon: "lock",
        requires: [
          {
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "Administration.Reviews.List",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/clients",
        label: "Apps",
        icon: "appWindow",
        requires: [
          {
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "OAuthClients.List",
              action: "call",
            },
          },
        ],
      },
    ],
  },
  {
    title: "Platform",
    items: [
      {
        href: "/admin/services",
        label: "Services",
        icon: "box",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/devices",
        label: "Devices",
        icon: "laptop",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/apis",
        label: "APIs",
        icon: "braces",
        requires: [
          {
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "Apis.List",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/sign-in",
        label: "Sign-in methods",
        icon: "key",
        requires: [
          {
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "SignInSettings.Get",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/branding",
        label: "Sign-in page",
        icon: "palette",
        requires: [
          {
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "PortalThemes.Get",
              action: "call",
            },
          },
        ],
      },
      {
        href: "/admin/resources",
        label: "Resources",
        icon: "database",
        requires: [
          {
            permission: {
              api: apis.auth.API.identity,
              surface: "rpc",
              name: "Resources.List",
              action: "call",
            },
          },
        ],
      },
    ],
  },
  {
    title: "Account",
    items: [{ href: "/profile", label: "Account", icon: "settings" }],
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

const ADMINISTRATIVE_CAPABILITIES: ReadonlySet<string> = new Set([
  "trellis.auth.v1:users.manage",
  "trellis.auth.v1:roles.manage",
  "trellis.auth.v1:apis.accept",
  "trellis.auth.v1:apis.forceReplace",
  "trellis.auth.v1:clients.manage",
  "trellis.auth.v1:platform.manage",
]);

/** Whether a capability is one of the six administrative capabilities. */
export function isAdministrativeCapability(capabilityId: string): boolean {
  return ADMINISTRATIVE_CAPABILITIES.has(capabilityId);
}

/** Human-facing operator classification: holds any administrative capability. */
export function hasPlatformAdmin(
  authority: Pick<Authority, "capabilities"> | null,
): boolean {
  return (
    authority?.capabilities.some(({ capabilityId }) =>
      isAdministrativeCapability(capabilityId),
    ) ?? false
  );
}

function meetsRequirements(
  authority: Authority | null,
  requirements: readonly Requirement[],
): boolean {
  return requirements.every(
    (requirement) =>
      (!requirement.administrative || hasPlatformAdmin(authority)) &&
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
  authority: Pick<Authority, "capabilities"> | null,
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
