import { equal, ok } from "node:assert/strict";
import { test } from "node:test";
import { apis } from "trellis-web-generated";
import {
  type Authority,
  canAccessRoute,
  getVisibleNavSections,
  hasExactPermission,
} from "./control-panel.ts";

test("navigation separates delegated administration from an ordinary API action", () => {
  const member: Authority = {
    authorityDigest: "current-ui-snapshot",
    platformPrivileges: [],
    actions: [
      {
        apiId: apis.health.API.identity,
        action: { kind: "rpc", name: "Query", direction: "call" },
      },
    ],
  };
  const visible = getVisibleNavSections(member).flatMap(
    (section) => section.items,
  );
  ok(visible.some((item) => item.href === "/admin/health-events"));
  ok(!visible.some((item) => item.href === "/admin/users"));
  equal(canAccessRoute("/admin/users", member), false);
  equal(canAccessRoute("/admin/health-events", member), true);
  equal(canAccessRoute("/profile", member), true);
  equal(
    canAccessRoute("/admin/users", {
      ...member,
      platformPrivileges: ["principals.manage"],
    }),
    true,
  );
});

test("a finite privilege does not replace an exact catalog-authorized action", () => {
  const authority: Authority = {
    authorityDigest: "current-ui-snapshot",
    platformPrivileges: ["principals.manage"],
    actions: [
      {
        apiId: apis.auth.API.identity,
        action: { kind: "rpc", name: "Sessions.Revoke", direction: "call" },
      },
    ],
  };
  const action = {
    api: apis.auth.API.identity,
    surface: "rpc",
    name: "Sessions.Revoke",
    action: "call",
  } as const;
  equal(hasExactPermission(authority, action), true);
  equal(
    hasExactPermission(authority, { ...action, name: "Sessions.List" }),
    false,
  );
  equal(hasExactPermission(authority, { ...action, action: "control" }), false);
  equal(
    hasExactPermission(authority, { ...action, api: apis.jobs.API.identity }),
    false,
  );
});
