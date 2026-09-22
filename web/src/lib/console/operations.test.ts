import { equal } from "node:assert/strict";

import { canPerform, CONSOLE_OPERATIONS } from "./operations.ts";
import type { Authority } from "../control-panel.ts";

declare const Deno: {
  test(name: string, fn: () => void): void;
};

function authorityWith(
  permissions: Array<{
    api: string;
    surface: string;
    name: string;
    action: string;
  }>,
  privileges: string[] = ["trellis.auth::admin"],
): Authority {
  return {
    platformPrivileges: privileges,
    grants: {
      format: "v1",
      permissions: permissions.map((permission) => ({
        action: permission.action,
        target: new TextEncoder().encode(
          JSON.stringify({
            kind: "apiSurface",
            api: permission.api,
            surface: permission.surface,
            name: permission.name,
          }),
        ),
      })),
    } as Authority["grants"],
  };
}

Deno.test("U16 an unregistered operation is never permitted", () => {
  const authority = authorityWith([{
    api: CONSOLE_OPERATIONS.deploymentsList.api,
    surface: "rpc",
    name: "Deployments.List",
    action: "call",
  }]);
  equal(canPerform(authority, "notARegisteredOperation"), false);
});

Deno.test("U17 a registered operation requires its exact permission atom", () => {
  const operation = CONSOLE_OPERATIONS.deploymentsList;
  const permitted = authorityWith([{
    api: operation.api,
    surface: operation.surface,
    name: "Deployments.List",
    action: operation.action,
  }]);
  equal(canPerform(permitted, "deploymentsList"), true);
  equal(
    canPerform(permitted, "deploymentsGet"),
    false,
    "a different surface name must not be satisfied by the list permission",
  );
});

Deno.test("U18 an administrative operation requires the platform privilege", () => {
  const operation = CONSOLE_OPERATIONS.deploymentsGet;
  const atom = {
    api: operation.api,
    surface: operation.surface,
    name: operation.name,
    action: operation.action,
  };
  equal(canPerform(authorityWith([atom], []), "deploymentsGet"), false);
  equal(
    canPerform(
      authorityWith([atom], ["trellis.auth::admin"]),
      "deploymentsGet",
    ),
    true,
  );
});

Deno.test("U19 self-service operations do not require the admin privilege", () => {
  const operation = CONSOLE_OPERATIONS.usersPasswordChange;
  const authority = authorityWith(
    [{
      api: operation.api,
      surface: operation.surface,
      name: operation.name,
      action: operation.action,
    }],
    [],
  );
  equal(canPerform(authority, "usersPasswordChange"), true);
});

Deno.test("U20 a feed subscription is not satisfied by an RPC permission", () => {
  const watch = CONSOLE_OPERATIONS.eventsWatch;
  const asRpc = authorityWith([{
    api: watch.api,
    surface: "rpc",
    name: watch.name,
    action: "call",
  }]);
  equal(canPerform(asRpc, "eventsWatch"), false);
  const asFeed = authorityWith([{
    api: watch.api,
    surface: "feed",
    name: watch.name,
    action: "subscribe",
  }]);
  equal(canPerform(asFeed, "eventsWatch"), true);
});

Deno.test("U21 a null authority permits nothing", () => {
  for (const name of Object.keys(CONSOLE_OPERATIONS)) {
    equal(canPerform(null, name), false, name);
  }
});
