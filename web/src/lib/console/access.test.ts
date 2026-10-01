import { assertEquals, assertThrows } from "@std/assert";
import {
  type Capability,
  type CapabilityGroup,
  type Permission,
  permissionKey,
  permissionLabel,
  presetPermissions,
} from "./access.ts";

Deno.test("permission descriptions identify operation signals and declared resources", () => {
  const encoder = new TextEncoder();
  assertEquals(
    permissionLabel({
      action: "control",
      target: encoder.encode(
        '{"api":"example.work@v1","kind":"operationSignal","operation":"Upload","signal":"Continue"}',
      ),
    }),
    "example.work@v1 / operation Upload / signal Continue · control",
  );
  assertEquals(
    permissionLabel({
      action: "read",
      target: encoder.encode(
        '{"kind":"participantResource","name":"documents","participant":"example.editor","resource":"state"}',
      ),
    }),
    "example.editor / state resource documents · read",
  );
});

Deno.test("applying nested permission presets deduplicates actions regardless of target JSON order", () => {
  const first: Permission = {
    action: "call",
    target: new TextEncoder().encode(
      '{"kind":"apiSurface","api":"example.docs@v1","surface":"rpc","name":"Read"}',
    ),
  };
  const equivalent: Permission = {
    action: "call",
    target: new TextEncoder().encode(
      '{"name":"Read","surface":"rpc","api":"example.docs@v1","kind":"apiSurface"}',
    ),
  };
  const second: Permission = { ...first, action: "observe" };
  const capabilities: Capability[] = [
    {
      capability: "read",
      displayName: "Read",
      description: "Read documents",
      sourceApi: "example.docs@v1",
      allows: [first],
    },
    {
      capability: "inspect",
      displayName: "Inspect",
      description: "Inspect documents",
      sourceApi: "example.docs@v1",
      allows: [equivalent, second],
    },
  ];
  const groups: CapabilityGroup[] = [
    {
      groupKey: "readers",
      displayName: "Readers",
      description: "",
      capabilities: ["read"],
      includedGroups: [],
      createdAt: 1n,
      updatedAt: 1n,
      version: 1n,
    },
    {
      groupKey: "reviewers",
      displayName: "Reviewers",
      description: "",
      capabilities: ["inspect"],
      includedGroups: ["readers"],
      createdAt: 1n,
      updatedAt: 1n,
      version: 1n,
    },
  ];
  const applied = presetPermissions("reviewers", groups, capabilities);
  assertEquals(
    applied.map(permissionKey).sort(),
    [permissionKey(first), permissionKey(second)].sort(),
  );
  assertThrows(
    () => presetPermissions("reviewers", groups, capabilities.slice(0, 1)),
    Error,
    "no unique concrete permission mapping",
  );
  assertThrows(
    () => presetPermissions("reviewers", groups.slice(1), capabilities),
    Error,
    "unavailable group",
  );
});
