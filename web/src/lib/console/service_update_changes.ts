/**
 * Plain-language descriptions of what a service update changed: the access it
 * asked for or gave up, the storage it gained or lost, and the APIs it offers.
 */

import { capabilityArea } from "./capability_areas.ts";
import { apiLabel, sentenceName } from "./plain_labels.ts";

type Resource = { kind: string; name: string };

/** The changes an update records, as `ServiceUpdates.List` returns them. */
export type UpdateChanges = {
  addedCapabilities: readonly string[];
  removedCapabilities: readonly string[];
  addedResources: readonly Resource[];
  reattachedResources: readonly Resource[];
  changedResources: readonly Resource[];
  removedResources: readonly Resource[];
  addedApis: readonly string[];
  sharedApis: readonly string[];
  removedApis: readonly string[];
  acceptedApis: readonly string[];
  acceptedUsedApis: readonly string[];
};

const RESOURCE_KINDS: Record<string, [string, string]> = {
  kv: ["Key-value store", "key-value store"],
  store: ["File store", "file store"],
  state: ["Saved state", "saved state"],
  job: ["Job queue", "job queue"],
  eventConsumer: ["Event delivery", "event delivery"],
  webhook: ["Webhook", "webhook"],
};

/** A capability's title when the Console knows it, else a readable form of its ID. */
export function capabilityLabel(
  capabilityId: string,
  titles: ReadonlyMap<string, string>,
): string {
  const title = titles.get(capabilityId);
  if (title) return title;
  const area = capabilityArea(capabilityId);
  // Everything after the area is the action, which may itself be dotted.
  const local = capabilityId.slice(capabilityId.indexOf(":") + 1);
  const action = local.slice(local.indexOf(".") + 1);
  return `${area.title}: ${sentenceName(action).toLowerCase()} (${area.api})`;
}

function resourceLabel(resource: Resource): string {
  return `${RESOURCE_KINDS[resource.kind]?.[0] ?? sentenceName(resource.kind)} “${resource.name}”`;
}

/** Each change as a sentence, grouped the way the review asks about them. */
export function describeUpdateChanges(
  changes: UpdateChanges,
  titles: ReadonlyMap<string, string>,
): { asks: string[]; givesUp: string[]; provides: string[] } {
  return {
    asks: [
      ...changes.addedCapabilities.map((id) => capabilityLabel(id, titles)),
      ...changes.addedResources.map(resourceLabel),
      ...changes.reattachedResources.map(
        (resource) => `${resourceLabel(resource)} again, with the data it kept`,
      ),
      ...changes.addedApis.map((api) =>
        changes.sharedApis.includes(api)
          ? `To offer ${apiLabel(api)}, which another service already offers`
          : `To offer ${apiLabel(api)}`,
      ),
      ...changes.changedResources.map(
        (resource) => `${resourceLabel(resource)}, with more room`,
      ),
      ...changes.acceptedUsedApis.map(
        (api) =>
          `To add ${apiLabel(api)}, which it uses, before any service offers it`,
      ),
    ],
    givesUp: [
      ...changes.removedCapabilities.map((id) => capabilityLabel(id, titles)),
      ...changes.removedResources.map(
        (resource) => `${resourceLabel(resource)} (its data is kept)`,
      ),
    ],
    provides: [
      ...changes.removedApis.map((api) => `Stops offering ${apiLabel(api)}`),
      ...changes.acceptedApis.map(
        (api) => `Installs the new version of ${apiLabel(api)}`,
      ),
    ],
  };
}

function counted(count: number, singular: string, plural = `${singular}s`) {
  return `${count} ${count === 1 ? singular : plural}`;
}

function byKind(resources: readonly Resource[]): Map<string, number> {
  const kinds = new Map<string, number>();
  for (const { kind } of resources) kinds.set(kind, (kinds.get(kind) ?? 0) + 1);
  return kinds;
}

/**
 * One line for a table cell, such as "+2 permissions, +1 key-value store,
 * offers 1 API". Empty when the update changed no access, storage, or API.
 */
export function summarizeUpdateChanges(changes: UpdateChanges): string[] {
  const parts: string[] = [];
  const resource = (kind: string, count: number) => {
    const [, noun] = RESOURCE_KINDS[kind] ?? ["", sentenceName(kind)];
    return counted(count, noun.toLowerCase());
  };
  if (changes.addedCapabilities.length)
    parts.push(`+${counted(changes.addedCapabilities.length, "permission")}`);
  for (const [kind, count] of byKind([
    ...changes.addedResources,
    ...changes.reattachedResources,
  ]))
    parts.push(`+${resource(kind, count)}`);
  if (changes.changedResources.length)
    parts.push(`${counted(changes.changedResources.length, "store")} enlarged`);
  if (changes.removedCapabilities.length)
    parts.push(`−${counted(changes.removedCapabilities.length, "permission")}`);
  for (const [kind, count] of byKind(changes.removedResources))
    parts.push(`−${resource(kind, count)}`);
  if (changes.addedApis.length)
    parts.push(`offers ${counted(changes.addedApis.length, "API")}`);
  if (changes.removedApis.length)
    parts.push(`stops offering ${counted(changes.removedApis.length, "API")}`);
  if (changes.acceptedApis.length)
    parts.push(`${counted(changes.acceptedApis.length, "API")} updated`);
  if (changes.acceptedUsedApis.length)
    parts.push(`uses ${counted(changes.acceptedUsedApis.length, "new API")}`);
  return parts;
}
