import { apiLabel, sentenceName } from "./plain_labels.ts";

/**
 * Capability IDs are `api.vN:area.action` (`trellis.auth.v1:users.edit`), and a
 * group is `api.vN:name`. People choose access by API area, so pickers group
 * on the area; a group sits in the area it is named for.
 */
export function capabilityArea(capabilityId: string): {
  id: string;
  title: string;
  api: string;
} {
  const colon = capabilityId.indexOf(":");
  const prefix = colon < 0 ? "" : capabilityId.slice(0, colon);
  const local = colon < 0 ? capabilityId : capabilityId.slice(colon + 1);
  const area = local.split(".")[0] ?? local;
  // "trellis.auth.v1" → "trellis.auth@v1" for the API's short name.
  const api = apiLabel(prefix.replace(/\.(v\d+)$/, "@$1"));
  return { id: `${prefix}:${area}`, title: sentenceName(area), api };
}

export type CapabilityArea<T> = {
  id: string;
  title: string;
  /** The API the area belongs to, to tell same-named areas apart. */
  description: string;
  items: T[];
};

/** Groups capabilities by API area, ordered by title then API. */
export function groupByArea<T>(
  items: readonly T[],
  capabilityIdOf: (item: T) => string,
): CapabilityArea<T>[] {
  const byId = new Map<string, CapabilityArea<T>>();
  for (const item of items) {
    const area = capabilityArea(capabilityIdOf(item));
    const group = byId.get(area.id) ?? {
      id: area.id,
      title: area.title,
      description: `${area.api} API`,
      items: [],
    };
    group.items.push(item);
    byId.set(area.id, group);
  }
  return [...byId.values()].sort(
    (left, right) =>
      left.title.localeCompare(right.title) ||
      left.description.localeCompare(right.description),
  );
}
