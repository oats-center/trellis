import { apis } from "trellis-web-generated";

type Connection = apis.auth.SessionsMeOutput["connection"];

/** Participant-scoped authority retained from the live Trellis connection. */
export type Authority = Pick<Connection, "platformPrivileges" | "grants">;

/** Platform privilege that gates Trellis Auth administration. */
export const PLATFORM_ADMIN = "trellis.auth::admin";

const HEALTH_API = apis.health.API.identity;
const EVENTS_API = apis.events.API.identity;
const JOBS_API = apis.jobs.API.identity;

/** One exact API-surface permission atom required by a route or action. */
export type ExactPermission = {
  api: string;
  surface: string;
  name: string;
  action: string;
};

type Requirement = {
  privilege?: string;
  permission?: ExactPermission;
};

type DisplayProfile = {
  name?: string | null;
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
  "/admin/sessions/revoke": "Revoke Session",
  "/admin/sessions/kick": "Kick Connection",
  "/admin/services/new": "Create Service Deployment",
  "/admin/devices/profiles/new": "Create Device Deployment",
  "/admin/devices/profiles/disable": "Disable Device Deployment",
  "/admin/devices/instances/provision": "Provision Device Instance",
  "/admin/devices/instances/disable": "Disable Device Instance",
  "/admin/devices/activations/revoke": "Revoke Device Activation",
  "/admin/devices/reviews/decide": "Decide Device Review",
  "/admin/health-events": "Health",
  "/admin/events": "Events",
  "/admin/jobs": "Jobs",
  "/admin/grants": "Grants",
  "/admin/grants/new": "Edit Portal Grant Policy",
  "/admin/apps": "User Grants",
  "/admin/apps/revoke": "Revoke User Grant",
  "/admin/capability-groups": "Capability Groups",
  "/admin/capability-groups/edit": "Edit Capability Group",
  "/admin/capability-groups/new": "New Capability Group",
  "/admin/portals": "Portals",
  "/admin/portals/new": "Create Portal",
  "/admin/portals/edit": "Edit Portal",
  "/admin/portals/login": "Portal Policy",
  "/admin/portals/login/default": "Built-In Login Portal",
  "/admin/portals/login/selection": "Portal Routes",
  "/admin/portals/devices": "Device Portal Policy",
  "/admin/portals/devices/default": "Default Device Portal",
  "/admin/portals/devices/selection": "Device Portal Selection",
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

const ADMIN_ONLY: readonly Requirement[] = [{ privilege: PLATFORM_ADMIN }];

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
        href: "/admin/grants",
        label: "Grants",
        icon: "key",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/capability-groups",
        label: "Capability Groups",
        icon: "key",
        requires: ADMIN_ONLY,
      },
      {
        href: "/admin/portals",
        label: "Portals",
        icon: "database",
        requires: ADMIN_ONLY,
      },
    ],
  },
  {
    title: "Manage",
    items: [
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

type GrantTarget = {
  kind?: unknown;
  api?: unknown;
  surface?: unknown;
  name?: unknown;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function permissionTarget(
  permission: Connection["grants"]["permissions"][number],
): GrantTarget {
  if (!(permission.target instanceof Uint8Array)) return {};
  try {
    const parsed: unknown = JSON.parse(
      new TextDecoder().decode(permission.target),
    );
    if (!isRecord(parsed)) return {};
    return parsed;
  } catch {
    return {};
  }
}

/** True when the current exact GrantSet contains the required permission atom. */
export function hasExactPermission(
  authority: Authority,
  required: ExactPermission,
): boolean {
  return authority.grants.permissions.some((permission) => {
    if (permission.action !== required.action) return false;
    const target = permissionTarget(permission);
    return target.kind === "apiSurface" &&
      target.api === required.api &&
      target.surface === required.surface &&
      target.name === required.name;
  });
}

/** True when the connection carries the platform administration privilege. */
export function hasPlatformAdmin(
  authority: Pick<Authority, "platformPrivileges"> | null,
): boolean {
  return authority?.platformPrivileges.includes(PLATFORM_ADMIN) ?? false;
}

function meetsRequirements(
  authority: Authority | null,
  requirements: readonly Requirement[],
): boolean {
  return requirements.every((requirement) =>
    (requirement.privilege === undefined ||
      authority?.platformPrivileges.includes(requirement.privilege) === true) &&
    (requirement.permission === undefined ||
      (authority !== null &&
        hasExactPermission(authority, requirement.permission)))
  );
}

export function canAccessRoute(
  pathname: string,
  authority: Authority | null,
): boolean {
  if (!pathname.startsWith("/admin")) return true;
  const item = navSections.flatMap((section) => section.items)
    .filter((candidate) =>
      pathname === candidate.href || pathname.startsWith(`${candidate.href}/`)
    )
    .sort((left, right) => right.href.length - left.href.length)[0];
  return item !== undefined &&
    meetsRequirements(authority, item.requires ?? []);
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
        meetsRequirements(authority, item.requires ?? [])
      ),
    }))
    .filter((section) => section.items.length > 0);
}

function hasRouteTitle(pathname: string): pathname is keyof typeof routeTitles {
  return Object.hasOwn(routeTitles, pathname);
}

export function getPageTitle(pathname: string): string {
  if (hasRouteTitle(pathname)) return routeTitles[pathname];
  const detail = detailRouteTitles.find(([prefix]) =>
    pathname.startsWith(prefix) && pathname.length > prefix.length
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
  const name = profile?.name?.trim();
  if (!name) return "TR";

  const parts = name.split(/\s+/).filter(Boolean);
  const initials = parts.slice(0, 2).map((part) => part[0]?.toUpperCase() ?? "")
    .join("");
  return initials || name.slice(0, 2).toUpperCase();
}
