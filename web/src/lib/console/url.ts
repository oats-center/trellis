/**
 * Pure Console URL construction.
 *
 * `console_paths.ts` is the single console-internal resolver; it supplies the
 * configured `base` to these functions. Keeping the encoding here free of the
 * SvelteKit virtual module makes the round-trip rules unit-testable.
 */

/** Serializable query values accepted by `buildConsoleUrl`. */
export type ConsoleQuery = Record<
  string,
  string | number | bigint | null | undefined
>;

/** Encodes query fields with `URLSearchParams`; null/undefined fields are dropped. */
export function queryString(query: ConsoleQuery): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value === null || value === undefined) continue;
    params.set(key, typeof value === "string" ? value : value.toString());
  }
  return params.toString();
}

/**
 * Resolves a route with path params encoded exactly once beneath `base`.
 * Query values must go through `query`, never pasted into `route`, so an opaque
 * identifier cannot be double-encoded or split on a separator inside a value.
 */
export function resolveWithBase(
  base: string,
  route: string,
  params: Record<string, string> = {},
): string {
  let path = route.replace("/(app)", "");
  for (const [name, value] of Object.entries(params)) {
    path = path.replace(`[${name}]`, encodeURIComponent(value));
  }
  return `${base}${path === "/" ? "" : path}`;
}

/** Builds one Console-internal URL from a base, route, path params, and query. */
export function buildConsoleUrl(
  base: string,
  route: string,
  options: {
    params?: Record<string, string>;
    query?: ConsoleQuery;
  } = {},
): string {
  const path = resolveWithBase(base, route, options.params);
  const search = queryString(options.query ?? {});
  return search ? `${path}?${search}` : path;
}
