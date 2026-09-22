/**
 * Cursor pagination and exact-target resolution for console reads.
 *
 * Tables advance one page at a time with an opaque cursor history. Selectors
 * and catalogs that need completeness traverse pages with a bounded limit and a
 * cycle guard. Exact-target actions resolve through `Get` when the API has one,
 * otherwise through complete traversal of a correctly filtered list; they never
 * fall back to the first loaded row.
 */

/** One server page request. Page size stays a JSON number, never a wire string. */
export type PageRequest = {
  readonly cursor?: string;
  readonly limit: number;
};

/** Table page size prescribed by the console plan. */
export const TABLE_PAGE_LIMIT = 50;

/** Catalog traversal page size; also respects the stricter Auth sessions limit. */
export const CATALOG_PAGE_LIMIT = 100;

/** Maximum independent catalog traversals a page may run concurrently. */
export const MAX_CONCURRENT_TRAVERSALS = 4;

/** Builds a table page request, omitting an absent cursor. */
export function tablePage(cursor?: string): PageRequest {
  return cursor === undefined
    ? { limit: TABLE_PAGE_LIMIT }
    : { limit: TABLE_PAGE_LIMIT, cursor };
}

/** Builds a catalog/selector page request, omitting an absent cursor. */
export function catalogPage(cursor?: string): PageRequest {
  return cursor === undefined
    ? { limit: CATALOG_PAGE_LIMIT }
    : { limit: CATALOG_PAGE_LIMIT, cursor };
}

/** One page returned by a list read. */
export type CursorPage<T> = {
  readonly cursor?: string;
  readonly items: readonly T[];
};

/** Result of a bounded complete traversal. */
export type Traversed<T> = {
  readonly complete: boolean;
  readonly items: readonly T[];
  /** Set when traversal stopped early; the catalog is not complete. */
  readonly error?: unknown;
};

/** Raised when a server repeats a cursor, which would otherwise loop forever. */
export class CursorCycleError extends Error {
  readonly cursor: string;

  constructor(cursor: string) {
    super(`server repeated pagination cursor '${cursor}'`);
    this.name = "CursorCycleError";
    this.cursor = cursor;
  }
}

/** Raised when a requested exact target is absent from the complete list. */
export class ExactTargetMissingError extends Error {
  readonly target: string;

  constructor(target: string) {
    super(`exact target '${target}' was not found`);
    this.name = "ExactTargetMissingError";
    this.target = target;
  }
}

/**
 * Loads every page in order. Stops on an absent `nextCursor`, on a repeated
 * cursor (visible failure, not a loop), or on a failed later page (partial
 * result with `complete: false`).
 */
export async function traverseAll<T>(
  loadPage: (page: PageRequest) => Promise<CursorPage<T>>,
  options: { limit?: number } = {},
): Promise<Traversed<T>> {
  const limit = options.limit ?? CATALOG_PAGE_LIMIT;
  const items: T[] = [];
  const seen = new Set<string>();
  let cursor: string | undefined;
  for (;;) {
    let page: CursorPage<T>;
    try {
      page = await loadPage(
        cursor === undefined ? { limit } : { cursor, limit },
      );
    } catch (error) {
      return { complete: false, items, error };
    }
    items.push(...page.items);
    const next = page.cursor;
    if (next === undefined) return { complete: true, items };
    if (seen.has(next)) {
      return { complete: false, items, error: new CursorCycleError(next) };
    }
    seen.add(next);
    cursor = next;
  }
}

/**
 * Traverses with bounded concurrency and returns the first item matching
 * `matches`. Absence produces `undefined`; the caller decides how to present an
 * unavailable target and must never substitute another record.
 */
export async function resolveExact<T>(
  loadPage: (page: PageRequest) => Promise<CursorPage<T>>,
  matches: (item: T) => boolean,
  options: { limit?: number } = {},
): Promise<{ item?: T; complete: boolean; error?: unknown }> {
  const limit = options.limit ?? CATALOG_PAGE_LIMIT;
  let cursor: string | undefined;
  const seen = new Set<string>();
  for (;;) {
    let page: CursorPage<T>;
    try {
      page = await loadPage(
        cursor === undefined ? { limit } : { cursor, limit },
      );
    } catch (error) {
      return { complete: false, error };
    }
    const item = page.items.find(matches);
    if (item !== undefined) return { item, complete: true };
    const next = page.cursor;
    if (next === undefined) return { complete: true };
    if (seen.has(next)) {
      return { complete: false, error: new CursorCycleError(next) };
    }
    seen.add(next);
    cursor = next;
  }
}

/**
 * Runs at most `limit` traversals at once, preserving input order. Pages use
 * this so a screen with several independent catalogs cannot fan out without
 * bound.
 */
export async function mapWithConcurrency<T, R>(
  items: readonly T[],
  limit: number,
  run: (item: T, index: number) => Promise<R>,
): Promise<R[]> {
  if (limit < 1) throw new RangeError("concurrency limit must be positive");
  const results = new Array<R>(items.length);
  let next = 0;
  const workers = Array.from(
    { length: Math.min(limit, items.length) },
    async () => {
      for (;;) {
        const index = next++;
        if (index >= items.length) return;
        results[index] = await run(items[index], index);
      }
    },
  );
  await Promise.all(workers);
  return results;
}
