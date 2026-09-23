import {
  decodePaginationCursor,
  encodePaginationCursor,
  paginationQueryDigest,
} from "@oatscenter/trellis";

/** Pages authorized demo rows with the shared opaque cursor codec. */
export async function paginate<T>(options: {
  endpoint: string;
  cursor?: string;
  limit?: number;
  rows: T[];
  key: (row: T) => string;
  compare?: (left: string, right: string) => number;
}): Promise<{ items: T[]; page: { nextCursor?: string } }> {
  const limit = options.limit ?? 100;
  if (!Number.isInteger(limit) || limit < 1 || limit > 500) {
    throw new Error("pagination limit must be between 1 and 500");
  }
  const digest = await paginationQueryDigest(options.endpoint, {});
  const after = options.cursor === undefined
    ? undefined
    : await decodePaginationCursor<unknown>(options.cursor, digest);
  if (after !== undefined && typeof after !== "string") {
    throw new Error("invalid pagination cursor");
  }
  const compare = options.compare ??
    ((left, right) => left < right ? -1 : left > right ? 1 : 0);
  const remaining = options.rows
    .toSorted((left, right) => compare(options.key(left), options.key(right)))
    .filter((row) =>
      after === undefined || compare(options.key(row), after) > 0
    );
  const items = remaining.slice(0, limit);
  return {
    items,
    page: remaining.length > limit
      ? {
        nextCursor: await encodePaginationCursor(
          digest,
          options.key(items.at(-1)!),
        ),
      }
      : {},
  };
}
