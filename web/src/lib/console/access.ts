import type { apis } from "trellis-web-generated";
import { canonicalizeJsonValue } from "@oatscenter/trellis/auth";
import { decodeKnownJsonBytes, displayJson } from "./display_value.ts";

export type Permission =
  apis.auth.GrantsSetInput["grants"]["permissions"][number];
export type Capability = apis.auth.CapabilitiesListOutput["items"][number];
export type CapabilityGroup =
  apis.auth.CapabilityGroupsListOutput["items"][number];

/** Matches server-authored permission targets regardless of JSON property order. */
export function permissionKey(permission: Permission): string {
  const decoded = decodeKnownJsonBytes(permission.target);
  return `${permission.action}:${
    decoded.ok
      ? canonicalizeJsonValue(decoded.value)
      : Array.from(permission.target).join(",")
  }`;
}

/** Describes a server-authored permission; unfamiliar targets remain visible. */
export function permissionLabel(permission: Permission): string {
  const decoded = decodeKnownJsonBytes(permission.target);
  if (!decoded.ok) {
    return `${permission.action} · ${displayJson(permission.target)}`;
  }
  const target = decoded.value;
  if (typeof target === "object" && target !== null && "kind" in target) {
    if (
      target.kind === "operationSignal" && "api" in target &&
      "operation" in target && "signal" in target
    ) {
      return `${target.api} / operation ${target.operation} / signal ${target.signal} · ${permission.action}`;
    }
    if (
      target.kind === "participantResource" && "participant" in target &&
      "resource" in target && "name" in target
    ) {
      return `${target.participant} / ${target.resource} resource ${target.name} · ${permission.action}`;
    }
  }
  if (
    typeof target === "object" && target !== null && "api" in target &&
    "name" in target
  ) {
    return `${target.api} / ${
      "surface" in target ? target.surface : ""
    } ${target.name} · ${permission.action}`;
  }
  return `${displayJson(target)} · ${permission.action}`;
}

/** Expands a permission preset completely, failing on missing groups or labels. */
export function presetPermissions(
  groupKey: string,
  groups: readonly CapabilityGroup[],
  capabilities: readonly Capability[],
): Permission[] {
  const pending = [groupKey];
  const visited = new Set<string>();
  const names = new Set<string>();
  while (pending.length) {
    const key = pending.pop()!;
    if (visited.has(key)) continue;
    visited.add(key);
    const group = groups.find((entry) => entry.groupKey === key);
    if (!group) {
      throw new Error(
        `Permission preset refers to unavailable group '${key}'.`,
      );
    }
    for (const name of group.capabilities) names.add(name);
    pending.push(...group.includedGroups);
  }
  const permissions = new Map<string, Permission>();
  for (const name of names) {
    const matches = capabilities.filter((entry) => entry.capability === name);
    if (matches.length !== 1 || matches[0].allows.length === 0) {
      throw new Error(
        `Capability '${name}' has no unique concrete permission mapping. Select exact permissions instead.`,
      );
    }
    for (const permission of matches[0].allows) {
      permissions.set(permissionKey(permission), permission);
    }
  }
  return [...permissions.values()];
}
