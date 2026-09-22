import { deepEqual, ok } from "node:assert/strict";
import { apis } from "trellis-web-generated";

import {
  type Authority,
  canAccessRoute,
  getPageTitle,
  getRoleLabel,
  getVisibleNavSections,
  hasExactPermission,
  PLATFORM_ADMIN,
} from "./control-panel.ts";

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};

type PermissionAtom = Authority["grants"]["permissions"][number];

const AUTH_API = apis.auth.API.identity;
const HEALTH_API = apis.health.API.identity;
const EVENTS_API = apis.events.API.identity;
const JOBS_API = apis.jobs.API.identity;

function permit(
  api: string,
  name: string,
  action: PermissionAtom["action"] = "call",
): PermissionAtom {
  return {
    action,
    target: new TextEncoder().encode(
      JSON.stringify({ kind: "apiSurface", api, surface: "rpc", name }),
    ),
  };
}

function authority(
  platformPrivileges: readonly string[],
  permissions: readonly PermissionAtom[],
): Authority {
  return {
    platformPrivileges: [...platformPrivileges],
    grants: { format: "trellis.grant-set.v1", permissions: [...permissions] },
  };
}

function labels(value: Authority | null): string[] {
  return getVisibleNavSections(value).flatMap((section) =>
    section.items.map((item) => item.label)
  );
}

Deno.test(
  "control panel exposes administrative navigation with admin privilege and exact grants",
  () => {
    const admin = authority(
      [PLATFORM_ADMIN],
      [
        permit(HEALTH_API, "Query"),
        permit(EVENTS_API, "Query"),
        permit(JOBS_API, "Query"),
      ],
    );

    deepEqual(labels(admin), [
      "Account",
      "Overview",
      "Health Events",
      "Sessions",
      "Events",
      "Jobs",
      "Grants",
      "Capability Groups",
      "Portals",
      "Services",
      "Devices",
      "Users",
    ]);
    deepEqual(getRoleLabel(admin), "Operator");
    ok(canAccessRoute("/admin", admin));
    ok(canAccessRoute("/admin/users", admin));
    ok(canAccessRoute("/admin/health-events", admin));
    ok(canAccessRoute("/admin/jobs", admin));
  },
);

Deno.test(
  "control panel hides Auth administration from exact grants without admin privilege",
  () => {
    const operator = authority(
      [],
      [
        permit(AUTH_API, "Users.List"),
        permit(HEALTH_API, "Query"),
      ],
    );

    deepEqual(labels(operator), ["Account", "Health Events"]);
    deepEqual(getRoleLabel(operator), "Member");
    ok(!canAccessRoute("/admin", operator));
    ok(!canAccessRoute("/admin/users", operator));
    ok(!canAccessRoute("/admin/sessions", operator));
    ok(canAccessRoute("/admin/health-events", operator));
    ok(!canAccessRoute("/admin/jobs", operator));
  },
);

Deno.test(
  "control panel does not treat an admin as granted an exact action",
  () => {
    const admin = authority([PLATFORM_ADMIN], [permit(HEALTH_API, "Query")]);
    const revoke = {
      api: AUTH_API,
      surface: "rpc",
      name: "Sessions.Revoke",
      action: "call",
    };

    ok(!hasExactPermission(admin, revoke));

    const revoker = authority([PLATFORM_ADMIN], [
      permit(AUTH_API, "Sessions.Revoke", "control"),
    ]);
    ok(!hasExactPermission(revoker, revoke));
    ok(
      hasExactPermission(revoker, { ...revoke, action: "control" }),
    );
  },
);

Deno.test("control panel keeps account navigation without admin privilege", () => {
  const member = authority([], []);

  deepEqual(getVisibleNavSections(member).map((section) => section.title), [
    "Account",
  ]);
  deepEqual(getRoleLabel(member), "Member");
  ok(canAccessRoute("/profile", member));
  ok(!canAccessRoute("/admin", member));
});

Deno.test("control panel titles cover new admin routes", () => {
  deepEqual(getPageTitle("/admin/services"), "Services");
  deepEqual(getPageTitle("/admin/devices"), "Devices");
  deepEqual(getPageTitle("/admin/jobs"), "Jobs");
});

Deno.test("control panel titles cover every current route", () => {
  deepEqual(getPageTitle("/admin/users/new"), "Create User");
  deepEqual(getPageTitle("/admin/apps"), "User Grants");
  deepEqual(getPageTitle("/admin/apps/revoke"), "Revoke User Grant");
  deepEqual(getPageTitle("/admin/grants/new"), "Edit Portal Grant Policy");
  deepEqual(getPageTitle("/admin/portals/new"), "Create Portal");
  deepEqual(getPageTitle("/admin/portals/edit"), "Edit Portal");
});

Deno.test("control panel detail titles never leak a previous target", () => {
  deepEqual(getPageTitle("/admin/services/dep_1"), "Service Deployment");
  deepEqual(getPageTitle("/admin/services/dep_2"), "Service Deployment");
  deepEqual(getPageTitle("/admin/jobs/job_1"), "Job");
  deepEqual(getPageTitle("/admin/unknown-route"), "Trellis");
});
