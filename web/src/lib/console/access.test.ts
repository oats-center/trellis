import { assertEquals, assertThrows } from "@std/assert";
import {
  type Capability,
  type CapabilityGroup,
  type Permission,
  permissionKey,
  presetPermissions,
} from "./access.ts";

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
