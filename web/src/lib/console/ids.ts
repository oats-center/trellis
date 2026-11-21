/**
 * Workspace-qualified IDs as people read them. An ID such as
 * `ghcr.io/acme/orders.billing@v1` is shown as its short ID,
 * `orders.billing@v1`, while no other ID on the page has the same short ID,
 * and in full when one does.
 */

/** The ID without its workspace: everything after the last `/`. */
export function shortId(id: string): string {
  return id.slice(id.lastIndexOf("/") + 1);
}

/** Each ID's label: its short ID while it is unique among `ids`, else the full ID. */
export function idLabels(ids: Iterable<string>): Map<string, string> {
  const unique = [...new Set(ids)];
  const counts = new Map<string, number>();
  for (const id of unique)
    counts.set(shortId(id), (counts.get(shortId(id)) ?? 0) + 1);
  return new Map(
    unique.map((id) => [id, counts.get(shortId(id)) === 1 ? shortId(id) : id]),
  );
}
