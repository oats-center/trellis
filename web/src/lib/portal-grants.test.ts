import { equal } from "node:assert/strict";

import { hasDuplicateRoleMapping } from "./portal-grants.ts";

declare const Deno: {
  test(name: string, fn: () => void): void;
};

Deno.test("portal grants reject duplicate provider and role keys", () => {
  equal(
    hasDuplicateRoleMapping([
      { providerId: "oidc-a", role: "operator" },
      { providerId: "oidc-a", role: "operator" },
    ]),
    true,
  );
  equal(
    hasDuplicateRoleMapping([
      { providerId: "oidc-a", role: "operator" },
      { providerId: "oidc-b", role: "operator" },
    ]),
    false,
  );
});
